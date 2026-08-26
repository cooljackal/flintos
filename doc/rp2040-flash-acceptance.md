<!-- SPDX-License-Identifier: Apache-2.0 -->

# RP2040 flash and key/value acceptance (#171)

The board opens its reserved region through `board::nvs_flash()`. The kernel's
`FlashStorage` adapts the existing `NorFlash` trait to `kvstore::Storage`; neither
the adapter nor applications select a vendor driver. The physical RP2040 driver
depends only on `hal` and `soc-rp2040`. ESP32 keeps its existing flash behavior.

## Partition and operating contract

Both supported 2 MiB board images reserve the final 16 KiB, offsets
`0x1fc000..0x200000` (XIP addresses `0x101fc000..0x10200000`). The linker excludes
that range from firmware and checks the RAM load image fits below it. The board
checks its partition constants against linker symbols before opening a handle.
Never run the destructive test over wanted NVS data: it erases this partition.

- Only one RP2040 flash-region handle may exist, even for disjoint regions.
  Drop releases ownership; another board open can then succeed.
- Reads/writes are word-aligned and range-checked without overflow. Writes
  accept erased words only. A page buffer fills untouched words with ones, so
  partial-page appends do not reprogram earlier entries. Every mutation is read
  back; sector erase leaves other sectors alone.
- Writes/erases are task-only, outside critical sections. Local interrupts are
  disabled, the peer acknowledges from SRAM, and every DMA channel must be idle.
  Missing peer acknowledgment returns an error after 50 ms without removing XIP.
- The existing SIO IRQ services flash requests as well as scheduler notifications.
  Generation-tagged acknowledgments cannot authorize the next request after a
  timeout. Spinlock 28 is reserved for XIP exclusion, separate from scheduler 14,
  entropy 29, DMA 30 and device ownership 31.
- NMI handlers, external bus masters and debugger memory windows must not access
  XIP during a mutation. This is not a real-time storage path: both cores pause,
  normal interrupts are delayed, and kernel tick accounting can lose ticks.
- ROM function lookup, page data and boot2 copying finish before XIP is disabled.
  The RAM function connects flash, exits XIP, erases/programs, flushes the cache,
  calls its RAM boot2 copy, then restores QSPI pads and XIP configuration.
  `rp2040-check-ram-code.ps1` rejects direct flash references in generated RAM
  code; indirect ROM/boot2 calls additionally require disassembly review.

## Bounded recovery, including the watchdog erratum

The ROM flash-ready loop has no software deadline. A new watchdog guard gives
one page/sector operation three seconds. If a watchdog is already running at
the supported 1 MHz tick, the guard never disables or reloads it; its original
deadline remains, bounded by the 24-bit hardware maximum (~8.4 seconds). An
unknown existing watchdog time base is refused. Debug pauses are removed during
the operation and restored afterward, along with reset routing and scratch state.

Do **not** save/restore the apparent remaining count: RP2040 `CTRL.TIME` does not
decrement, confirmed here and documented in Pico SDK issue #1492. A stalled
operation resets to ROM recovery instead of returning into unavailable flash.
This relies on the board's established crystal/tick clock continuing to run.

## Reproduce and measured coverage

Run `make test-arm-flash` with the Pico target and Wio Debug Probe. Override
`ARM_PROBE_SERIAL` and `ARM_UART_PORT` as needed (measured fixture: probe
`4150325537323116`, UART `COM9`, 115200 baud). SWD, ground and console UART are
required; no peripheral jumper, target USB enumeration or manual BOOTSEL is used.

The harness downloads the image, waits for an SRAM GO gate, detaches, then judges
UART completion. It does not poll flash while XIP is unavailable. After a deliberate
stall it reads watchdog registers and retained SRAM: timeout reason, consumed Flint
marker, ROM recovery-vector signature and elapsed time. It automatically reloads through SWD and verifies
the persisted keys again. The retained-signature judge is for this RP2040 ROM fixture;
it is not a generic Cortex-M recovery detector.

| Target test | Evidence required before PASS |
|---|---|
| Region ownership | Duplicate open refused, drop/reopen succeeds |
| Validation | Unaligned/out-of-range operations and masked task context refused |
| Peer exclusion | Counter stops while parked and resumes after release |
| Peer timeout | Interrupt-masked peer refused in 50–70 ms; later request succeeds |
| DMA exclusion | Real stalled UART RX DMA channel refused; cancellation permits flash |
| Raw writes | 400 exact patterned bytes crossing three pages; adjacent words stay erased |
| Erase isolation | First-sector erase preserves the last-sector sentinel |
| Both writers | Core 1 programs a sentinel which core 0 reads and erases |
| Existing KV format | 32 updates, stable key, maximum 128-byte value, reopen and compaction |
| Torn append | Incomplete header rejected; earlier keys readable; write refused until compaction |
| Reset persistence | Same three keys after normal reset and both fault/reload cycles |
| New watchdog guard | Deliberate SRAM stall with XIP disabled returns to ROM recovery |
| Existing watchdog | Enter stall with ~950 ms left of 2 s; retained SRAM-loop timing rejects a replenished deadline |

Initial Pico measurements: 400-byte programming 31–33 ms, four-sector erase
192–205 ms, peer timeout 50.4–50.7 ms. These are software elapsed times at the
configured 12 MHz CPU clock, not logic-analyzer timings. The new watchdog reset
was observed through SWD about 3.8 s after the UART marker (includes host wait
and attach latency); it is an upper bound, not the reset instant. The final
fixture also records the last SRAM-loop elapsed time in retained SRAM before
reset, so deadline judging does not depend on host process-launch latency.
The final debug fixture measured 2,998,786 µs for the new guard and 948,103 µs
for an already-running 2 s watchdog entered after roughly 1.05 s had elapsed.
The release fixture also passed: 27,840 µs programming, 195,154 µs four-sector
erase, 50,454 µs peer timeout, and fault-stall durations 2,998,771 / 946,298 µs.
Both images retained all three keys after the normal reset and after each of
their two automatic fault/reload cycles. No manual BOOTSEL was needed.

Host coverage includes all 64 word positions in a page with lengths 1–130 words,
range/alignment/overflow validation, generation rollover, both board partitions,
and the existing KV CRC/torn-write/compaction suite. Wio builds use the same driver
and layout; flash acceptance was measured on the Pico, not on the Wio target.
All four mandatory gates passed (`make test-host`, `make lint`, `make check-layers`,
`make check-all`): 997 host tests, plus 126 ARM-configured kernel host tests and
14 Pico / 13 Wio board tests. Debug/release Pico and debug Wio SRAM-code audits
passed. Target-specific Clippy passed for the changed driver, board and fixture.
The existing Pico bus regression also passed after the new SIO/RAM-code path:
4,096 SPI bytes and 1,001 I²C exchanges, including no-ACK and timeout recovery.

## Explicit limits

The torn record is injected in software, not by cutting physical power. Earlier
committed records survive that test; no physical power-cut qualification is claimed.
Existing whole-store compaction copies live entries to RAM, erases, then rewrites:
power loss during compaction can lose the store. Torn tails require explicit
compaction or erase before new appends. No transactional compaction, wear leveling,
partition table, firmware update, encrypted storage or arbitrary flash-part support
is introduced. The stall test removes XIP but does not simulate a physically stuck
or damaged flash chip.

## Vendor sources

- [Pico SDK flash sequencing](https://github.com/raspberrypi/pico-sdk/blob/master/src/rp2_common/hardware_flash/flash.c)
- [Pico SDK multicore flash exclusion](https://github.com/raspberrypi/pico-sdk/blob/master/src/rp2_common/pico_flash/flash.c)
- [RP2040 ROM flash implementation](https://github.com/raspberrypi/pico-bootrom/blob/master/bootrom/program_flash_generic.c)
- [Watchdog count erratum](https://github.com/raspberrypi/pico-sdk/issues/1492)
- [Zephyr Pico flash driver](https://github.com/zephyrproject-rtos/zephyr/blob/main/drivers/flash/flash_rpi_pico.c)
- [Wio RP2040 Mini flash capacity](https://wiki.seeedstudio.com/Wio_RP2040_mini_Dev_Board-Onboard_Wifi/)
