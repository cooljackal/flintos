<!-- SPDX-License-Identifier: Apache-2.0 -->

# RP2040 native USB and unattended transport (#172)

This is a full-speed **device**, providing a CDC byte stream and an explicitly
enabled development reset interface. It is not a USB host, mass-storage driver,
general class framework, or production firmware updater.

## Layer boundaries and operating contract

| Layer | Responsibility |
|---|---|
| `hal::usb::DeviceController` | Exclusive packet ownership and bounded events, no USB class policy |
| `drivers/physical/rp2040/usb` | Registers, DPRAM, data toggles, clock and silicon workarounds; depends only on HAL/SoC |
| `drivers/bus/usb-device` | Portable descriptors, control state machine, CDC buffering and reset permission; API only, no unsafe code |
| `board::usb_init` | Driver construction and interrupt routing; returns the portable serial-service contract |
| `apps/tests/usb-selftest` | Fresh image/nonce replies, exact byte echo and deliberate fault injection; no MMIO or chip driver selection |

Enable `board/native-usb` and call `board::usb_init(identity)` from a boot-core
task. Call `service` at least every millisecond even though the board also
services USB interrupts: periodic work finishes errata handling and queues data.
Each service call consumes at most 16 events and submits one CDC IN packet.
Reads and writes use the existing nonblocking `ByteStream`, with 512-byte rings,
64-byte USB packets, OUT backpressure and short/final-zero-length IN packets.
Writes require configuration, DTR and a nonsuspended bus. Check service errors;
UART-style parity/framing flags do not represent USB controller failures.

Only one controller may be open. On B0/B1 silicon, opening USB also reserves
GPIO15 and GPIO16 for the E5 enumeration workaround; applications cannot use
those pins concurrently. No jumper is needed. Drop releases the controller and
pins. B2+ uses endpoint abort; this newer-silicon path has not been tested here.
E2 means B0/B1 cannot guarantee cancellation of a packet already in flight;
the driver follows the vendor's best-effort control-buffer clear on those chips.

USB builds opt into a configured 125 MHz CPU clock and a dedicated 48 MHz USB
clock. Non-USB builds retain 12 MHz CPU. UART/SPI/I2C peripherals and ADC remain
on the 12 MHz crystal; the timer/watchdog reference is unchanged. These are
configured frequencies, not frequency-counter measurements. Datasheet E16
requires the CPU clock to be at least 10% faster than USB. This Pico missed USB
events with the original 12 MHz profile and enumerated with 125 MHz. That
measurement does not establish the cause of every earlier reset problem.
E15 defers bulk-IN ownership near frame end; E5 and E2 follow the vendor
old-silicon sequencing with bounded waits.

## Reset and test identity

Reset is an application permission (`allow_reset`), disabled unless explicitly
requested in its identity. The SDK-compatible vendor interface supports normal
reboot and BOOTSEL; CDC line coding at 1200 baud also requests BOOTSEL. The
service publishes a reset only after the control status packet is acknowledged.
The application takes that request in task context and calls `board::usb_reset`.
The board resets both cores through the watchdog; BOOTSEL is entered at startup,
never by jumping from a live dual-core application directly into ROM.

The fixture uses **1209:0001 only for private testing**. It is not a unique
product identity and must not be shipped, redistributed or manufactured into a
product. Supply an allocated VID/PID and a real serial identity for a product.
The fixture deliberately has no invented serial; its host binds the exact USB
physical location plus the ROM's real serial, and rediscovers the CDC COM port.
The Wio probe's serial port is a separate UART path, not the Pico's native USB.

Every result must contain the expected eight-hex-digit image ID and a fresh
random eight-byte host challenge. ROM enumeration, an old PASS line, another
device and a reset command acknowledgment are not passing test results.

## Reproduce on Windows

Connect Pico native USB plus the Wio Debug Probe's existing SWD/ground wiring.
No peripheral jumper or console UART is required for the USB suite. SWD is
required for initial installation and independent recovery when firmware cannot
service USB. Install stable Rust with `thumbv6m-none-eabi`, the ARM linker,
`probe-rs`, PowerShell 7, and the existing image-conversion prerequisites.
Windows binds its built-in CDC and WinUSB drivers using the device descriptors;
no custom driver installation is needed.

From a shell with the repository's normal Make environment:

```sh
make test-arm-usb ARM_USB_CYCLES=100 \
  ARM_USB_LOCATION='PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(6)#USB(4)#USB(4)' \
  ARM_PROBE_SERIAL=4150325537323116 ARM_BOOTSEL_SERIAL=E0C912D24340
```

Those identities belong to the measured fixture, not every Pico. Obtain the
parent's `DEVPKEY_Device_LocationPaths` using `Get-PnpDeviceProperty`, and select
the actual probe/ROM serials. Never substitute the first COM port or first
`RPI-RP2` disk. The copy helper verifies the selected disk belongs to that ROM
USB device. The command replaces firmware; it does not intentionally erase NVS.

`ARM_USB_IMAGE_ID` defaults to `17200001` and is passed into the build and judge.
For an already-built matching image, use `tools/rp2040-run-usb-selftest.ps1` with
explicit ELF, UF2, image ID, probe serial, ROM serial and location parameters.
`-SkipInitialDownload` requires the expected application already running.
Use `-InitialImageId <old-id> -SkipInitialDownload` when the first cycle must
replace a different running image with the requested `-ImageId`. The old image
is challenged first; every post-flash challenge requires the new identity.
Never rebuild or overwrite the selected artifacts while a soak is using them.
USB harnesses take an exclusive, per-location process lock and refuse a second
runner. Other tools and manual diagnostics must still be serialized with them;
never run a separate reset/flash check while this harness owns the target.

The harness checks descriptors and STALL recovery, then echoes lengths
1, 7, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513, 4096 and 65536.
Each cycle requests BOOTSEL, validates the ROM/volume identity, copies a verified
UF2, rediscovers CDC and checks a fresh challenge plus 4096 exact echo bytes.
Transport operations have deadlines (USB enumeration 25 s, copy 30 s, SWD 60 s).
A transport failure gets one SWD reload and is reported separately. Incorrect
image IDs, stale challenges or corrupted data fail without a retry. A second
transport failure ends the run; there is no unbounded BOOTSEL loop.

`-FaultTests` proves two distinct paths: an explicitly armed watchdog recovers
an interrupt-masked hang to ROM, while a stalled USB task first fails a fresh
challenge and the bounded USB update before recovering through SWD. The fixture
does not claim an always-fed, system-wide watchdog. USB cannot recover a dead
controller, failed clock or unplugged cable by itself; SWD also needs a working
probe and target power. Failures of both paths are reported, not called PASS.

## Evidence and limits

Portable host tests cover descriptor lengths, Unicode, control truncation/ZLP,
late address application, malformed requests, setup cancellation, reset-policy
and status-ACK ordering, CDC line coding, suspend, endpoint halt, ring wrap and
backpressure. The mock refuses overwriting a busy endpoint. PowerShell fixtures
cover stale/wrong results, wrong/ambiguous devices, COM renumbering, bounded
processes, one-shot fallback and verification failures that must not be retried.

Pico B1 measurements passed enumeration, descriptors/STALL recovery, all echo
lengths and a 100-cycle debug soak without a normal-cycle fallback or manual
intervention. Each cycle verified 4,096 exact bytes (409,600 total); cycle times
were 10.691–14.780 s, mean 12.069 s, including host enumeration and flash-copy
overhead. The cycle loop took 20 minutes 6.867 seconds. Its interrupt-masked
watchdog/ROM/USB recovery and stalled-task/SWD recovery both passed afterward.
The final release image (`17200002`) also passed ten isolated update/data cycles
with no normal-cycle fallback, then both deliberate fault/recovery cases.
Normal USB reboot was confirmed by a new UART boot marker plus native USB
challenge/echo; CDC 1200-baud BOOTSEL and subsequent USB reload also passed.
Final USB-only transitions `17200002 → 17200001 → 17200002` each passed the
old-image challenge, UF2 replacement, new-image challenge and 4,096-byte echo,
without SWD fallback. These prove replacement of a different firmware image,
not just reflashing an identical image. The target was left running release
image `17200002` with native CDC available.
This is a finite bench run, not a reliability
guarantee or a USB throughput measurement.

One earlier release run was confounded by an independently started reboot check:
a diagnostic sequencing script treated an empty log-read error as permission to
continue. The update harness recovered through its single SWD fallback, but that
cycle is not clean USB-only evidence. The diagnostic overlap was removed, the
release run repeated in isolation, and the production harness now refuses a
second USB runner for the same physical location. Other manual/legacy tools
still need operator serialization.

The independent SDK-only baseline also enumerated, echoed bytes and recovered
via its own 1200-baud reset and UF2 loader. All four required gates passed with
1,019 Rust host tests and 25 Windows harness fixtures. Additional USB-enabled
host checks passed: 126 ARM-configured kernel tests, 14 Pico board tests and
13 Wio board tests. Pico debug/release and Wio debug builds passed, as did
target-specific Clippy. Wio build success is not Wio hardware evidence.

Clock-change regressions passed the existing SPI/I²C suite at both 12 MHz and
125 MHz CPU: 4,096 SPI bytes, checksum 503,808, and 1,001 I²C exchanges / 8,008
bytes per run, including no-ACK and timeout recovery. At 125 MHz, measured
timeouts were 50,191 µs (SPI) and 50,120 µs (I²C). The 125 MHz UART DMA suite
passed timeout cleanup and 100 × 512-byte internal-loopback exchanges. These
tests enable the USB-safe clock profile but do not open the USB controller
concurrently with the peripheral fixture (whose SPI pins include GPIO16).
The full kernel suite also published its passing `0x600d` status at 125 MHz;
the SWD observer must tolerate the brief reset/reattach interval before reading
the retained result, rather than treating a transient attach failure as failure
of the suite itself.

Not claimed: USB certification, maximum throughput, remote wakeup, physical
cable/power-fault qualification, arbitrary USB classes, Linux/macOS host harness
support, Wio native-USB target measurements, or B2+ erratum-path measurements.

## Vendor sources

- [RP2040 datasheet: USB controller and E2/E5/E15/E16](https://datasheets.raspberrypi.com/rp2040/rp2040-datasheet.pdf)
- [TinyUSB RP2040 controller](https://github.com/hathach/tinyusb/blob/master/src/portable/raspberrypi/rp2040/dcd_rp2040.c)
- [Pico SDK 2.1.1 USB reset interface](https://github.com/raspberrypi/pico-sdk/blob/2.1.1/src/rp2_common/pico_stdio_usb/reset_interface.c)
- [Pico SDK 2.1.1 clock initialization](https://github.com/raspberrypi/pico-sdk/blob/2.1.1/src/rp2_common/pico_runtime_init/runtime_init_clocks.c)
- [Private-use PID restrictions](https://pid.codes/1209/0001/)
