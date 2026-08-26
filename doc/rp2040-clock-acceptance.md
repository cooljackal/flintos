<!-- SPDX-License-Identifier: Apache-2.0 -->

# RP2040 CPU frequency measurement (#174)

Status: Pico 12/125 MHz clock acceptance, kernel/UART/USB/watchdog regressions
and all four required gates pass. The README measured-clock row is now verified.

## Contract and limits

- The existing `SystemOnChip::measure_cpu_hz` contract selects the implementation.
  RP2040 reads its frequency counter in `soc/rp2040/src/frequency.rs`; it does not
  need an architectural cycle counter. No register access moved into the kernel.
- The counter measures `clk_sys`, after its divider, against a nominal 12 MHz
  crystal-backed `clk_ref`. Boot normalizes inherited reference/system dividers
  on the slower crystal path before selecting the optional 125 MHz profile.
- Interval 10 follows the Pico SDK and is approximately 1 ms. The datasheet
  gives 2 kHz uncertainty at that interval. Fractional result bits are converted
  to integer Hz without overflow, but extra printed digits are not extra accuracy.
  Values outside the supported 1–133 MHz operating range are refused.
- Spinlock 27 is dedicated to FC0. Its single-read acquisition refuses a busy
  owner rather than waiting, including same-core interrupt reentry. Single-core
  boot clears an inherited claim; later calls never forcibly clear another owner.
- The two polls share a 10 ms timer deadline, with a separate 100,000-iteration
  cap per poll if the timer stops while the CPU still runs. Scheduling delays can
  delay the return; this is not a hard real-time service deadline.
- A busy/waiting inherited count is not overwritten. After a new measurement,
  source NULL is written on success or failure and ownership is released. That
  NULL write can itself briefly run the counter; the next call still waits for
  idle before programming it.
- Unknown/changed reference selection, timeout, hardware failure, zero or an
  implausible result produces `None`. ARM boot uses the configured fallback and
  explicitly prints `ASSUMED ... not measured`; only a real result gets the
  measured label and calibrates SysTick.
- This checks clock ratios against the board crystal, **not absolute crystal
  calibration**. The peripheral clocks remain on the same configured reference.

## Reproduce on the existing Pico fixture

Use the Pico target, the Wio SWD probe and its UART (Pico GP0 TX to probe RX,
common ground). No new peripheral jumper is required. Only one target harness
may run at a time. The clock runner shares the native-USB harness's exclusive
physical-location lock; legacy/manual tools must also be kept out of the run.

```sh
make test-arm-clock ARM_CLOCK_HZ=12000000 ARM_UART_PORT=COM9 \
  ARM_PROBE_SERIAL=4150325537323116 \
  ARM_USB_LOCATION='PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(6)#USB(4)#USB(4)'
make test-arm-clock ARM_CLOCK_HZ=125000000 ARM_UART_PORT=COM9 \
  ARM_PROBE_SERIAL=4150325537323116 \
  ARM_USB_LOCATION='PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(6)#USB(4)#USB(4)'
```

These identities belong to this bench. Select the actual probe, UART and target
USB physical location on another bench. The command replaces target firmware;
it never flashes the Wio probe. The 125 MHz fixture enables `board/native-usb`
for its clock profile but does not open a USB device; test that separately with
the [native USB regression](rp2040-usb-acceptance.md).

The runner opens UART with DTR before downloading, waits for a fresh ready gate,
writes a nonzero random nonce, then checks the retained result contains that
exact nonce. A three-second window without debugger attachment precedes result
reads: the RP2040 timer defaults to pausing when either core enters debug mode,
which can distort a comparison against the other core's running SysTick.
Neither ROM enumeration nor an old PASS status is accepted. External processes
have deadlines, including download (60 s) and individual SWD reads/writes (10 s).
There is no automatic reset/reflash retry loop in this fixture.

| Required target evidence | Acceptance |
|---|---|
| Boot UART report | Explicit measured label and same Hz as the tick's stored frequency; no assumed fallback |
| Both cores | 32 successful measurements per core; busy retries counted, not substituted for measurements |
| Frequency range | Boot and sample extrema within ±5,000 Hz of the configured profile |
| Scheduler tick | A 100 ms task sleep takes 90–120 ms on the raw timer and scheduler |
| Regression | Existing kernel/tick/UART coverage at both profiles; native USB update/data and watchdog fault recovery |

For the kernel/UART regressions, build `arm-selftest` at each profile (ordinary
Pico build, then `EXTRA_FEATURES=board/native-usb`) and run
`tools/rp2040-run-selftest.ps1 -Suite io` against that freshly linked ELF, with
the probe/ROM serials above and `-SerialPort COM9 -TimeoutSeconds 30`. The I/O
judge requires both UART markers and the completed full kernel-suite result.
An outer `Invoke-UsbBoundedProcess` deadline of 120 seconds bounds this legacy
runner's process calls. Never rebuild its ELF while the runner is using it.

```sh
make test-arm-usb ARM_USB_IMAGE_ID=17400001 ARM_USB_CYCLES=5 \
  ARM_USB_LOCATION='PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(6)#USB(4)#USB(4)'
```

## Evidence (2026-08-26)

| Profile | Boot Hz | 64-sample range, Hz | Core 0 / core 1 samples | Busy retries | Raw timer / scheduler sleep | Fresh nonce |
|---|---:|---|---|---:|---|---:|
| 12 MHz | 12,000,000 | 12,000,000–12,000,000 | 32 / 32 | 57 | 100,535 µs / 101 ms | 33879444 |
| 125 MHz | 125,000,000 | 125,000,000–125,001,000 | 32 / 32 | 60 | 99,936 µs / 100 ms | 937358993 |

Both boot lines explicitly reported measurement against the crystal-backed
reference. All values meet the ±5,000 Hz acceptance band. These are relative
counter measurements, not independent calibration of the crystal.

- Host: 12 SoC measurement tests, three ARM boot/reporting tests, and 17 Windows
  result-judge fixtures cover conversion/bounds, ownership, timeout/stopped timer,
  stale status/nonce, error cleanup/reuse, reference changes and explicit fallback.
- Full `make test-host` reports 1,058 Rust test executions; host lint, layer checks
  and all-target checks pass. Windows runs the existing 25 USB fixtures too.
- Both clock fixture profiles are included in `make check-all` for Pico and Wio.
- The ARM-selected kernel host run passes 129 tests separately from the default
  host selection. Both target clock profiles were built and linked, not only checked.
- The complete kernel suite passed at both 12 and 125 MHz, including SMP/pinning,
  scheduling/sleep, mutex/priority inheritance and task/ISR queue coverage. Each
  run also reported 1,000 UART loopback payloads (16,000 bytes) and 10,000 exact
  physical GPIO-loopback edges through the existing I/O judge.
- Fresh USB image `17400001` passed descriptor/STALL recovery and exact echoes
  from 1 through 65,536 bytes at the boundary lengths in the USB runner. All five
  unattended USB update cycles passed new nonce/data checks without SWD fallback
  or manual BOOTSEL (14.683–18.347 seconds each).
- Interrupt-masked fault recovery passed via watchdog → ROM → USB reflash. A
  stalled USB task first failed its fresh challenge, then passed one bounded SWD
  recovery and a fresh 513-byte echo. The expected 25-second ROM timeout on this
  stalled-task path is not counted as a USB-only success. Final descriptors passed;
  the Pico was left running `17400001`. Neither fault test required manual BOOTSEL.
- No manual BOOTSEL was used in either passing clock run. A Wio-only unplug/replug
  restored its previously inaccessible UART/CMSIS-DAP interfaces before testing.
- The first flash exposed a new boot-reporting bug: an ESP32-specific raw UART
  helper faulted at PC `0x100012b2`. Reporting now uses the initialized board-owned
  console; a host writer test covers the actual measured/fallback formatting.
- Polling SWD during the timing check produced 87,305 µs against 101 scheduler ms.
  The unchanged target image passed after the host stopped attaching during the
  measurement window. Debugger interference is consistent with `TIMER_DBGPAUSE`;
  no timer register or acceptance tolerance was changed to make it pass.
- The passing 125 MHz and USB runs emitted probe-rs breakpoint-clear protocol
  warnings; download and subsequent fresh-nonce/UART/data checks still succeeded.
  This is not a claim that the debug transport is warning-free.
- Optional strict target Clippy remains blocked by five pre-existing
  `needless_return` findings in `soc/rp2040/src/ctrl.rs` and `dma.rs`; the required
  host lint gate passes. Those unrelated findings were not suppressed or changed.

## Primary references

- [Pico SDK 2.1.1 frequency-counter sequence](https://github.com/raspberrypi/pico-sdk/blob/2.1.1/src/rp2_common/hardware_clocks/clocks.c).
- [Generated RP2040 clock registers](https://github.com/raspberrypi/pico-sdk/blob/2.1.1/src/rp2040/hardware_regs/include/hardware/regs/clocks.h).
- [RP2040 datasheet](https://datasheets.raspberrypi.com/rp2040/rp2040-datasheet.pdf), §§2.15.4 and 2.15.6.2: counter accuracy and programming model.
