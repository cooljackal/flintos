<!-- SPDX-License-Identifier: Apache-2.0 -->

# Owned programmed I/O on RP2040 (#175)

PIO acceptance passes on the Pico at both CPU profiles, with kernel/UART/GPIO,
USB and recovery regressions and all four required gates. The README now marks
the explicitly limited polled PIO subset verified.

## Interface and ownership

- `hal::pio::ProgrammableIo` is a programmed digital-I/O contract, not `Bus`.
  `Instruction` describes portable operations; only the physical driver lowers
  them into native instructions and relocates jumps. No application opcodes,
  register addresses, IRQ instructions, DMA or side-set escape are exposed.
- `board::programmable_io(index)` returns an opaque implementation of that
  contract. Pico/Wio manifests currently route GP2 output and GP3 input for
  either of the two engines. This initial route is one input and one output,
  not a claim that arbitrary parallel pin groups are supported.
- An owner leases an entire PIO block and its pins. Each of its four machines
  can reserve a separate contiguous program in the shared 32-word memory;
  programs are not shared between machines. Reconfiguring an occupied machine,
  overlapping routed-pin use, exhausted/fragmented memory, invalid divisors,
  out-of-program branches and oversized instructions are rejected before writes.
- The SoC claim broker reserves peripheral bits 2/3 alongside UART bits 0/1
  and USB bit 7, using the existing shared pin ownership. PIO acquisition makes
  one spinlock attempt, with the calling core's interrupts briefly masked to
  exclude legacy spinning claim handlers. A busy lock returns `Busy`.
- Drop first stops machines, flushes FIFOs, tri-states output and restores the
  saved pad/mux configuration. It then publishes a retirement without waiting
  on the claim lock, using atomic release/acquire publication rather than an
  unlocked volatile flag. The next ARM claim transaction reaps it before checking
  ownership, so pins cannot be reassigned before quiescence. Host claims use
  atomics; tests include eight concurrent contenders and pin-collision rollback.
- The block owner disables both IRQ enables/forces and clears internal flags.
  There is no CPU IRQ handler or DMA channel to allocate: this version is polled.
  Cancel affects only its machine; it returns that program memory and routed-pin
  use within the owner. Reset cancels all machines but retains the block/pin lease.
  Drop releases the whole lease. Native arbitrary MMIO is outside this contract.

## Bounded behavior

`try_write` and `try_read` check the four-word FIFO status before one word access;
full/empty means `WouldBlock`, never overwrite/underflow. `exchange` sends one
word then consumes one word in FIFO order; it is not a tagged transaction and
must not be mixed with outstanding writes when one-to-one replies are required.
It accepts a 1–1,000,000 µs timeout and additionally caps work at 100,000 polls
if the timer stops. Timeout cancels the machine, so the caller must configure it
again. Initialization also has a 100,000-poll reset-release bound. These are work
bounds, not promises of wall-time service while a task is descheduled.

The divider is 16.8 fixed-point, rounded upward so the nominal instruction rate
does not exceed the request. The special integer-zero encoding is accepted only
for a divisor of 65,536. Timing uses the configured CPU profile; this test proves
payload behavior, not independent waveform-frequency calibration.

## Pico evidence (2026-08-26)

Fixture: Raspberry Pi Pico target, Wio probe `4150325537323116`, UART `COM9`,
Pico ROM serial `E0C912D24340`, physical GP2→GP3 jumper already used by GPIO/PWM.
The probe's firmware and wiring were not changed. No manual BOOTSEL was needed.

| CPU profile | Fresh nonce | Ordered 32-bit words | Blocks | Timeout recovery | Rejected collisions | FIFO full / empty | Drop/reopen |
|---|---:|---:|---:|---:|---:|---|---:|
| 12 MHz | 269642934 | 2,000 | 2 | 2 | 8 | 2 / 2 | 2 |
| 125 MHz | 761902808 | 2,000 | 2 | 2 | 8 | 2 / 2 | 2 |

Both runs serialize every word LSB-first through the output, sample each bit
from the physical input, and compare the reconstructed word exactly. All four
machines are allocated to exhaust program memory; machines 0 and 3 execute the
payload/reopen tests in each block. A deliberately low output holds a WAIT-high
program stalled; the 2 ms exchange timeout cancels it and a newly configured
loopback succeeds. FIFO test words are flushed before judging real payloads.
There are four additional successful recovery/reopen words beyond each 2,000
count. Claims are checked from one target task; host threads separately exercise
concurrent acquisition. This is not a cross-core target ownership soak.

The first run's retained counts passed but its overlong log line was truncated
by the 64-byte logger, so the host rejected it. The final fixture emits two short
lines and the judge requires both; tolerances/counts were not relaxed.
Probe-rs emitted breakpoint-clear protocol warnings during some downloads;
subsequent fresh nonce and UART checks passed. The transport is not warning-free.

Host coverage: 16 physical-driver tests, two shared-claim/retirement tests, and
17 PowerShell result-judge fixtures. `make test-host` reports 1,076 Rust test
executions; existing USB/clock judges also run. All four required gates pass;
`check-all` includes both PIO profiles on both ARM board selections. Pico target
images were built, linked and executed; no Wio target execution is claimed.

The separate ARM-selected kernel host suite passes 129 tests. The existing full
kernel/UART/GPIO target suite also passes at 12 MHz, including 1,000 UART
payloads (16,000 bytes) and 10,000 physical GPIO-loopback edges.

Final USB regression image `17500002` passed descriptors/STALL recovery and
exact boundary echoes from 1 through 65,536 bytes. Both unattended USB update
cycles passed fresh-nonce/data checks without SWD fallback (13.239 and 13.868
seconds). Interrupt-masked watchdog recovery passed through ROM and USB reflash.
The deliberately stalled task failed its fresh challenge and required the
expected one bounded SWD recovery after a 25-second ROM deadline; fresh 513-byte
echo and final descriptors then passed. No manual BOOTSEL was needed. The Pico
was left running this tested USB image.

## Reproduction

```sh
make test-arm-pio ARM_PIO_HZ=12000000 ARM_UART_PORT=COM9 \
  ARM_USB_LOCATION='PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(6)#USB(4)#USB(4)'
make test-arm-pio ARM_PIO_HZ=125000000 ARM_UART_PORT=COM9 \
  ARM_USB_LOCATION='PCIROOT(0)#PCI(1400)#USBROOT(0)#USB(6)#USB(4)#USB(4)'
```

Use the actual probe/UART/USB location for another bench. The runner holds the
same physical-fixture lock as USB/clock tests. It opens UART before flashing,
checks a fresh ready state, writes a random nonce, then allows a debugger-free
window before reading retained counts. Download/read/write processes have
deadlines; it does not reset/reflash until a failed test appears to pass.

This does **not** implement CAN, I²S, SDIO, RMT compatibility, IRQ-driven or DMA
transfers, synchronized multi-machine starts, multi-pin parallel programs, an
assembler, or independent calibration.

## Primary references

- [Pico SDK 2.1.1 PIO allocation, relocation and initialization](https://github.com/raspberrypi/pico-sdk/blob/2.1.1/src/rp2_common/hardware_pio/pio.c).
- [SDK configuration, restart and FIFO clearing](https://github.com/raspberrypi/pico-sdk/blob/2.1.1/src/rp2_common/hardware_pio/include/hardware/pio.h).
- [SDK instruction encodings](https://github.com/raspberrypi/pico-sdk/blob/2.1.1/src/rp2_common/hardware_pio/include/hardware/pio_instructions.h).
- [RP2040 generated register definitions](https://github.com/raspberrypi/pico-sdk/blob/2.1.1/src/rp2040/hardware_regs/include/hardware/regs/pio.h).
- [RP2040 datasheet, chapter 3](https://datasheets.raspberrypi.com/rp2040/rp2040-datasheet.pdf).
