<!-- SPDX-License-Identifier: Apache-2.0 -->

# RP2040 on-target test matrix

This is the coverage contract for issue #88. A smaller ARM suite is not ESP32
parity: every test below must report `PASS`, `FAIL`, or an explicit `SKIP` in
the existing `[FLINT] SELFTEST` stream. The host judge counts only pass/fail
lines and reconciles them with the summary; skips remain visible but cannot
turn missing coverage green.

The classifications come from the code in `kernel/src/selftest*.rs`, not its
comments alone. Implementation waits for the ARM kernel modules; this file is
the reviewable test list they must wire up.

## Portable kernel tests: run unchanged

| Existing test(s) | RP2040 acceptance | Gate |
|---|---|---|
| `tick_advances`, `tick_never_goes_backwards` | SysTick advances and is monotonic | SysTick |
| `critical_section_masks_the_tick` | PRIMASK suppresses the tick and restores its prior state | PRIMASK |
| `nested_critical_sections_stay_masked`, `interrupt_depth_returns_to_zero` | Nested masking and bookkeeping survive a real exception | PRIMASK |
| `ready_mask_agrees_with_task_states`, `pending_switch_is_taken_once` | PendSV leaves scheduler state consistent | PendSV |
| `mutex_cycle_under_ticks_leaves_no_residue`, `isr_queue_delivers_exactly_once` | Real interrupt preemption preserves queue/mutex invariants | PendSV + SysTick |
| `general_memory_holds_a_pattern`, `two_allocations_do_not_overlap`, `the_pool_returns_to_full_after_use` | The selected RP2040 heap is writable and balances | linker heap |
| `dma_memory_is_where_dma_can_reach`, `every_allocation_is_dma_capable` | Claims match RP2040 DMA SRAM reachability | DMA allocator |
| all six tests in `selftest_dynobj.rs` | Task, queue, semaphore, event and reaper lifecycles work at 32-bit target addresses | task switching |

`the_reaper_skips_a_task_a_core_is_on` runs on core 0 initially. It does not
claim multicore coverage until the SMP milestone adds a separate contention
case.

## Architecture and SoC equivalents

| ESP32 test | RP2040 test | Evidence required |
|---|---|---|
| three Xtensa window tests | `exception_frame_survives_preemption`, `callee_saved_registers_survive_preemption`, `task_return_trampoline_survives_preemption` | Repeated SysTick/PendSV switches preserve PSP, xPSR Thumb bit and r4-r11 |
| three TIMG tests | `timer_counts_against_systick`, `timer_alarm_fires_once`, `periodic_alarm_keeps_firing` | A RP2040 timer alarm is checked against SysTick, not itself |
| flash erase/IRAM test | `flash_erase_keeps_sram_irq_alive` | SRAM-resident handler runs while XIP is unavailable; SysTick behavior is reported separately |
| ADC channel tests | `adc_known_levels_track`, `every_rp2040_adc_channel_converts` | Ground/reference fixture or board-known levels; floating values never count |
| SPI bus loopback | `spi_bus_loopback_round_trips` | Repeated FIFO and DMA sizes, byte exact |
| UART stream loopback | `uart_bytestream_loopback_round_trips` | Byte exact, including FIFO boundary and back-to-back transfers |

The RP2040 timer test follows the same independent-clock pattern as the
current TIMG test. The flash test is a new SoC implementation, not a rename:
ESP32 cache interrupt masking and RP2040 XIP stalls are different mechanisms.

## Loopback fixtures

| Bus | Initial fixture | What it proves | Status |
|---|---|---|---|
| UART | Physical wire from a spare UART TX pin to RX | Pads, pinmux, UART FIFO and `ByteStream`; does not rely on undocumented loopback behavior | Required; pins must be declared by the Wio board manifest |
| SPI | Physical MOSI-to-MISO wire, with SCK on a declared spare pin | Pads, pinmux, controller FIFO/DMA and `Bus` | Required; Zephyr's SPI loopback pattern likewise treats loopback as a board fixture |
| I2C | I2C0 controller wired to I2C1 target plus SDA/SCL pull-ups | Controller and target state machines, ACK, repeated-start and data integrity | Deferred until an RP2040 target-mode driver exists; a bus scan alone is not a loopback test |

RP2040 UART and SPI registers derive from PrimeCell peripherals, but the first
FlintOS tests use wires. The Pico SDK does not expose UART loopback as a stable
API, and a register-only internal path would fail to test board pin routing.
The Pico SDK I2C example scans an external bus; it does not establish a
self-contained loopback. Therefore I2C must `SKIP` with the missing target
driver/fixture reason until both exist.

## ESP32-only exclusions

| Existing test(s) | ARM disposition | Reason |
|---|---|---|
| `timer_preserves_windowed_context`, `deep_window_recursion_returns_intact`, `call8_windows_survive_preemption` | Replaced | Xtensa register windows and `call8` do not exist on ARMv6-M |
| three `dport_*` tests | Excluded | RP2040 has no ESP32 DPORT erratum |
| `dac_drives_and_adc2_reads_it_back`, `adc2_refuses_a_read_while_the_radio_is_up` | Excluded | RP2040 has neither DAC nor ESP32 ADC2/Wi-Fi arbitration |
| `twai_self_reception_round_trips` | Excluded | RP2040 has no TWAI controller |
| `i2s_dma_loopback_round_trips` | Excluded initially | RP2040 has no native I2S controller; a later PIO-I2S driver needs its own test |
| `reclaimed_memory_is_available` | Excluded | It validates ESP32 ROM/radio reclaimed regions, not the portable allocator |

## Harness acceptance

- Reset discovery keys on USB parent serial, so a COM-port change is allowed.
- Every serial wait and transport operation has a host deadline.
- The judge requires one begin marker, one complete summary, exact line-count
  agreement, at least one pass, and zero failures.
- ARM and ESP32 results publish separate pass/skip inventories. Equal pass
  counts are never used as evidence of equal coverage.
- Physical fixture pin assignments are recorded in the Wio board manifest;
  an undeclared wire produces `SKIP`, not `PASS`.

Reference patterns: Raspberry Pi's Pico SDK `i2c/bus_scan` requires declared
board pins and pull-ups and treats device acknowledgement as the observation;
Zephyr's `tests/drivers/spi/spi_loopback` uses a named physical fixture and
keeps unsupported DMA cases as skips. FlintOS follows those two principles:
fixtures are explicit, and unavailable capabilities stay visible.
