# SPDX-License-Identifier: Apache-2.0
"""Flash a UF2 to an RP2040 board, entering BOOTSEL automatically.

If the board is already running FlintOS firmware that brought up native USB with
`allow_reset` (the CDC device is present), this opens that serial port at 1200
baud -- the "1200bps touch" -- which the firmware answers by rebooting into the
ROM UF2 bootloader. No BOOTSEL button, no debug probe. The board then mounts as
the `RPI-RP2` drive and the UF2 is copied to it.

Fallbacks, in order:
  1. An `RPI-RP2` drive is already mounted (blank chip, or the button was held)
     -> copy straight to it.
  2. A FlintOS CDC serial port is found -> 1200bps touch, wait for the drive,
     copy.
  3. Neither -> explain how to enter BOOTSEL by hand and exit non-zero.

Cross-platform and standard-library only (pyserial is used if importable, but is
never required): Windows uses `winreg` + `ctypes`, POSIX uses `termios` and the
usual mount locations.

Usage: rp2040-flash.py <image.uf2> [--port COMx|/dev/ttyACM0] [--timeout S]
"""
import argparse
import glob
import os
import sys
import time

# CDC identities that accept the 1200bps touch: FlintOS uses the pid.codes
# prototype id; the Pico SDK's stdio_usb uses Raspberry Pi's.
FLINT_IDS = [(0x1209, 0x0001), (0x2E8A, 0x000A)]
BOOTSEL_LABEL = "RPI-RP2"


# ── Finding the mounted BOOTSEL drive ────────────────────────────────────────
def find_bootsel_drive():
    if sys.platform.startswith("win"):
        import ctypes
        from ctypes import wintypes

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        drives = k32.GetLogicalDrives()
        for i in range(26):
            if not (drives & (1 << i)):
                continue
            root = "%s:\\" % chr(ord("A") + i)
            label = ctypes.create_unicode_buffer(261)
            if k32.GetVolumeInformationW(ctypes.c_wchar_p(root), label, 261,
                                         None, None, None, None, 0):
                if label.value == BOOTSEL_LABEL:
                    return root
        return None

    for pat in ("/Volumes/%s", "/media/%s/%s", "/media/%s", "/run/media/%s/%s"):
        for cand in glob.glob(pat % ((os.environ.get("USER", "*"), BOOTSEL_LABEL)
                                     if pat.count("%s") == 2 else (BOOTSEL_LABEL,))):
            if os.path.isdir(cand):
                return cand
    # Last resort on Linux: the by-label symlink, if it is already mounted.
    for cand in glob.glob("/media/*/%s" % BOOTSEL_LABEL) + \
            glob.glob("/run/media/*/%s" % BOOTSEL_LABEL):
        if os.path.isdir(cand):
            return cand
    return None


# ── Finding the FlintOS CDC serial port ──────────────────────────────────────
def find_cdc_port(ids):
    # pyserial, if present, is the cleanest cross-platform enumerator.
    try:
        from serial.tools import list_ports
        for p in list_ports.comports():
            if p.vid is not None and (p.vid, p.pid) in ids:
                return p.device
        return None
    except ImportError:
        pass

    if sys.platform.startswith("win"):
        import winreg
        # The CDC is a composite device: its COM port lives under an interface
        # key `VID_xxxx&PID_yyyy&MI_nn`, a sibling of the bare `VID_xxxx&PID_yyyy`
        # key -- so prefix-match every Enum\USB subkey, not just the exact id.
        prefixes = tuple("VID_%04X&PID_%04X" % (v, p) for v, p in ids)
        try:
            usb = winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE,
                                 r"SYSTEM\CurrentControlSet\Enum\USB")
        except OSError:
            usb = None
        if usb:
            try:
                for i in range(winreg.QueryInfoKey(usb)[0]):
                    dev = winreg.EnumKey(usb, i)
                    if not dev.startswith(prefixes):
                        continue
                    devk = winreg.OpenKey(usb, dev)
                    try:
                        for j in range(winreg.QueryInfoKey(devk)[0]):
                            inst = winreg.EnumKey(devk, j)
                            try:
                                params = winreg.OpenKey(devk, inst + r"\Device Parameters")
                                return winreg.QueryValueEx(params, "PortName")[0]
                            except OSError:
                                continue
                    finally:
                        winreg.CloseKey(devk)
            finally:
                winreg.CloseKey(usb)
        return None

    # POSIX without pyserial: match the USB serial VID:PID via the by-id links.
    for link in glob.glob("/dev/serial/by-id/*"):
        low = link.lower()
        if any("%04x_%04x" % (v, p) in low or "%04x:%04x" % (v, p) in low
               for v, p in ids):
            return os.path.realpath(link)
    cands = sorted(glob.glob("/dev/ttyACM*") + glob.glob("/dev/cu.usbmodem*"))
    return cands[0] if cands else None


# ── The 1200bps touch ────────────────────────────────────────────────────────
def touch_1200(port):
    try:
        import serial
        s = serial.Serial(port, 1200)
        s.dtr = False
        time.sleep(0.2)
        s.close()
        return
    except ImportError:
        pass

    if sys.platform.startswith("win"):
        import ctypes
        from ctypes import wintypes

        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        GENERIC_READ, GENERIC_WRITE, OPEN_EXISTING, CLRDTR = 0x80000000, 0x40000000, 3, 6
        INVALID = ctypes.c_void_p(-1).value
        h = k32.CreateFileW(r"\\.\%s" % port, GENERIC_READ | GENERIC_WRITE, 0,
                            None, OPEN_EXISTING, 0, None)
        if h == INVALID:
            raise ctypes.WinError(ctypes.get_last_error())
        try:
            class DCB(ctypes.Structure):
                _fields_ = [("DCBlength", wintypes.DWORD), ("BaudRate", wintypes.DWORD),
                            ("flags", wintypes.DWORD), ("wReserved", wintypes.WORD),
                            ("XonLim", wintypes.WORD), ("XoffLim", wintypes.WORD),
                            ("ByteSize", ctypes.c_ubyte), ("Parity", ctypes.c_ubyte),
                            ("StopBits", ctypes.c_ubyte), ("XonChar", ctypes.c_char),
                            ("XoffChar", ctypes.c_char), ("ErrorChar", ctypes.c_char),
                            ("EofChar", ctypes.c_char), ("EvtChar", ctypes.c_char),
                            ("wReserved1", wintypes.WORD)]
            dcb = DCB()
            dcb.DCBlength = ctypes.sizeof(dcb)
            k32.BuildCommDCBW("baud=1200 parity=N data=8 stop=1", ctypes.byref(dcb))
            k32.SetCommState(h, ctypes.byref(dcb))   # sends SET_LINE_CODING baud=1200
            k32.EscapeCommFunction(h, CLRDTR)
            time.sleep(0.2)
        finally:
            k32.CloseHandle(h)
        return

    import termios
    fd = os.open(port, os.O_RDWR | os.O_NOCTTY)
    try:
        attrs = termios.tcgetattr(fd)
        attrs[4] = attrs[5] = termios.B1200        # ispeed, ospeed
        termios.tcsetattr(fd, termios.TCSANOW, attrs)
        time.sleep(0.2)
    finally:
        os.close(fd)


# ── Copying the image ────────────────────────────────────────────────────────
def copy_uf2(uf2, drive):
    data = open(uf2, "rb").read()
    dest = os.path.join(drive, os.path.basename(uf2))
    fh = open(dest, "wb")
    try:
        fh.write(data)
        fh.flush()
        try:
            os.fsync(fh.fileno())
        except OSError:
            pass  # the board reboots and yanks the drive as the last block lands
    finally:
        try:
            fh.close()
        except OSError:
            pass


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("uf2")
    ap.add_argument("--port", help="CDC port to touch (skip auto-detect)")
    ap.add_argument("--timeout", type=float, default=20.0)
    args = ap.parse_args()

    if not os.path.isfile(args.uf2):
        print("no such UF2: %s" % args.uf2, file=sys.stderr)
        return 2

    drive = find_bootsel_drive()
    if drive:
        print("board already in BOOTSEL (%s); copying %s" % (drive, os.path.basename(args.uf2)))
        copy_uf2(args.uf2, drive)
        print("flashed.")
        return 0

    port = args.port or find_cdc_port(FLINT_IDS)
    if not port:
        print(
            "No RPI-RP2 drive is mounted and no FlintOS USB serial device was found.\n"
            "  - First flash / blank chip: hold BOOTSEL while plugging in USB, then re-run.\n"
            "  - Otherwise the running firmware has no reset-enabled USB; use SWD, or\n"
            "    build with native USB + allow_reset so the 1200bps touch works.",
            file=sys.stderr)
        return 1

    print("1200bps touch on %s ..." % port)
    touch_1200(port)

    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        drive = find_bootsel_drive()
        if drive:
            break
        time.sleep(0.3)
    if not drive:
        print("the board did not enter BOOTSEL within %gs after the 1200-baud reset"
              % args.timeout, file=sys.stderr)
        return 1

    print("entered BOOTSEL (%s); copying %s" % (drive, os.path.basename(args.uf2)))
    copy_uf2(args.uf2, drive)
    print("flashed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
