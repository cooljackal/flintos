# SPDX-License-Identifier: Apache-2.0
"""Cross-platform serial capture for the RP2040 on-target self-tests.

The SWD half of every arm self-test is driven from bash through `probe-rs`,
which is portable on its own. The one thing bash cannot do portably is read a
serial port: git-bash's `stty`/`cat` cannot open a Windows COM port, and macOS
and Linux use `/dev/tty*` with different stty dialects. This helper reads the
port the same way on all three using nothing but the Python standard library --
`ctypes` against the Win32 API on Windows, `termios` on POSIX -- so the harness
needs only an interpreter, no `pip install`. (If `pyserial` happens to be
present it is used, but it is never required.)

Run it in the background before the target is reset; it appends every received
byte to `--out` until it is killed or `--seconds` elapses. The bash runner then
greps the captured text for the board's expected markers.
"""
import argparse
import sys
import time


def _capture_pyserial(port, baud, out, seconds):
    import serial  # noqa: F401  (only reached when installed)

    ser = serial.Serial(port, baud, timeout=0.2)
    ser.dtr = True
    deadline = time.monotonic() + seconds
    with open(out, "ab", buffering=0) as fh:
        while time.monotonic() < deadline:
            chunk = ser.read(256)
            if chunk:
                fh.write(chunk)
    ser.close()


def _capture_windows(port, baud, out, seconds):
    import ctypes
    from ctypes import wintypes

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    GENERIC_READ, GENERIC_WRITE = 0x80000000, 0x40000000
    OPEN_EXISTING = 3
    SETDTR = 5
    INVALID = ctypes.c_void_p(-1).value

    # \\.\COM9 is the reliable device path (plain "COM9" fails past COM9).
    path = r"\\.\%s" % port
    handle = k32.CreateFileW(path, GENERIC_READ | GENERIC_WRITE, 0, None,
                             OPEN_EXISTING, 0, None)
    if handle == INVALID:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        # BuildCommDCBW parses the mode string into a DCB, sparing us the full
        # struct definition; then SetCommState applies it.
        class DCB(ctypes.Structure):
            _fields_ = [("DCBlength", wintypes.DWORD)] + \
                       [(n, wintypes.DWORD) for n in ("BaudRate",)] + \
                       [("flags", wintypes.DWORD),
                        ("wReserved", wintypes.WORD),
                        ("XonLim", wintypes.WORD),
                        ("XoffLim", wintypes.WORD),
                        ("ByteSize", ctypes.c_ubyte),
                        ("Parity", ctypes.c_ubyte),
                        ("StopBits", ctypes.c_ubyte),
                        ("XonChar", ctypes.c_char),
                        ("XoffChar", ctypes.c_char),
                        ("ErrorChar", ctypes.c_char),
                        ("EofChar", ctypes.c_char),
                        ("EvtChar", ctypes.c_char),
                        ("wReserved1", wintypes.WORD)]

        dcb = DCB()
        dcb.DCBlength = ctypes.sizeof(dcb)
        if not k32.BuildCommDCBW("baud=%d parity=N data=8 stop=1" % baud,
                                 ctypes.byref(dcb)):
            raise ctypes.WinError(ctypes.get_last_error())
        if not k32.SetCommState(handle, ctypes.byref(dcb)):
            raise ctypes.WinError(ctypes.get_last_error())

        # Return from ReadFile promptly even with no data, so the loop stays
        # responsive to the deadline. (ReadIntervalTimeout = MAXDWORD with the
        # other two zero = return immediately with whatever is buffered.)
        class TIMEOUTS(ctypes.Structure):
            _fields_ = [("ReadIntervalTimeout", wintypes.DWORD),
                        ("ReadTotalTimeoutMultiplier", wintypes.DWORD),
                        ("ReadTotalTimeoutConstant", wintypes.DWORD),
                        ("WriteTotalTimeoutMultiplier", wintypes.DWORD),
                        ("WriteTotalTimeoutConstant", wintypes.DWORD)]

        to = TIMEOUTS(0xFFFFFFFF, 0, 0, 0, 0)
        k32.SetCommTimeouts(handle, ctypes.byref(to))
        # The debugprobe/TinyUSB CDC treats the link as connected only with DTR
        # asserted; without it the target's console bytes are dropped.
        k32.EscapeCommFunction(handle, SETDTR)

        buf = (ctypes.c_char * 256)()
        got = wintypes.DWORD(0)
        deadline = time.monotonic() + seconds
        with open(out, "ab", buffering=0) as fh:
            while time.monotonic() < deadline:
                if k32.ReadFile(handle, buf, 256, ctypes.byref(got), None):
                    if got.value:
                        fh.write(buf.raw[:got.value])
                    else:
                        time.sleep(0.02)
                else:
                    time.sleep(0.02)
    finally:
        k32.CloseHandle(handle)


def _capture_posix(port, baud, out, seconds):
    import os
    import termios

    fd = os.open(port, os.O_RDONLY | os.O_NOCTTY)
    try:
        speed = getattr(termios, "B%d" % baud)
        attrs = termios.tcgetattr(fd)
        iflag, oflag, cflag, lflag, ispeed, ospeed, cc = attrs
        cflag = (cflag | termios.CLOCAL | termios.CREAD) & ~termios.CSIZE
        cflag |= termios.CS8
        cflag &= ~(termios.PARENB | termios.CSTOPB)
        iflag &= ~(termios.IXON | termios.IXOFF | termios.ICRNL | termios.INLCR)
        lflag &= ~(termios.ICANON | termios.ECHO | termios.ISIG | termios.IEXTEN)
        oflag &= ~termios.OPOST
        cc = list(cc)
        cc[termios.VMIN] = 0
        cc[termios.VTIME] = 2  # 0.2s read timeout
        termios.tcsetattr(fd, termios.TCSANOW,
                          [iflag, oflag, cflag, lflag, speed, speed, cc])
        deadline = time.monotonic() + seconds
        with open(out, "ab", buffering=0) as fh:
            while time.monotonic() < deadline:
                try:
                    chunk = os.read(fd, 256)
                except OSError:
                    chunk = b""
                if chunk:
                    fh.write(chunk)
                else:
                    time.sleep(0.02)
    finally:
        os.close(fd)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", required=True)
    ap.add_argument("--baud", type=int, default=115200)
    ap.add_argument("--out", required=True)
    ap.add_argument("--seconds", type=float, default=40.0)
    args = ap.parse_args()

    try:
        import serial  # noqa: F401
        return _capture_pyserial(args.port, args.baud, args.out, args.seconds) or 0
    except ImportError:
        pass

    if sys.platform.startswith("win"):
        _capture_windows(args.port, args.baud, args.out, args.seconds)
    else:
        _capture_posix(args.port, args.baud, args.out, args.seconds)
    return 0


if __name__ == "__main__":
    sys.exit(main())
