<!-- SPDX-License-Identifier: Apache-2.0 -->

# Flint RTOS — Review Findings

Consolidated output of an adversarial review of the entire tree, conducted
before the first public release. Six independent reviewers covered the
scheduler and context switch, boot and memory map, synchronisation and IPC,
the driver stack, release packaging, and empirical build verification.

Every finding below was confirmed by reading the source. Where a reviewer's
claim did not survive verification, it is recorded in
[Withdrawn claims](#withdrawn-claims) rather than deleted.

**Baseline commit:** the state of the tree at first import.

---

## Verified sound

Worth stating explicitly, because a list of defects gives a distorted picture:

- The workspace builds. All 14 crates compile, and `cargo +esp build
  --target xtensa-esp32-none-elf` links a genuine Xtensa ELF.
- 49/49 host unit tests pass, matching the claimed count exactly.
- `tools/check-layers.sh` passes — the three-layer boundary holds.
- The window overflow/underflow vectors are correct canonical spill/fill
  stubs (`s32e`/`l32e` + `rfwo`/`rfwu`) at the right VECBASE offsets.
- `_flint_trap` is genuinely windowed (`entry a1, 0x110` confirmed by
  disassembly), so the `callx4` into it is ABI-appropriate.
- Watchdog disable sequences (TIMG0/TIMG1/RTC, key `0x50D83AA1`) are correct.
  The Super-WDT the internal plan demanded does not exist on classic ESP32.
- `registers.rs` uses assembler mnemonics rather than raw SR numbers, which
  structurally avoids a whole class of bug.
- `ENTRY(_start)`, the app descriptor placement, and the IRAM/DROM regions
  match the real ESP32 map.

---

## P0 — cannot run correctly on any ESP32

| # | Item | Location | Effect |
|---|---|---|---|
| 1 | `a2` destroyed on every trap; computed SP restored into it instead | `vectors.S:165,183,249` | ABI's primary argument/return register corrupted on every tick |
| 2 | `WINDOWBASE`/`WINDOWSTART` restored *before* the `a1`-relative loads | `vectors.S:216` | Window rotation re-maps `a1`; the entire restore reads garbage |
| 3 | No window spill in the live switch path — `flint_spill_all_windows` is a no-op **and is never called** | `context.S:35` | Any task interrupted more than one call deep is corrupted |
| 4 | UART `init()` writes data-bits into `PARITY`/`PARITY_EN` and stop-bits into `BIT_NUM` | `esp32_uart:93` | Console becomes 6 data bits + odd parity; boot banner prints as garbage |
| 5 | `UART_CLKDIV=0x10` is really `INT_CLR`; baud never programmed; spurious `/16` in the divisor | `esp32_uart:14,115` | Requested baud rate silently ignored |
| 6 | `dma_pool` sits outside the DMA-capable window; `task_stacks`/`panic_region` extend past the `0x3FFDC200` ROM-reserved boundary | `flint32.ld:31` | DMA buffers unreachable by DMA; stacks collide with ROM data |

## P1 — blocks a credible public release

| # | Item | Location |
|---|---|---|
| 7 | `CPU_HZ` hardcoded to 240 MHz; nothing in the boot path configures the clock | `tick.rs:16` |
| 8 | `PS.WOE` never explicitly set; correctness inherited from ROM state | `startup.S:23` |
| 9 | GPIO `ENABLE`=0x10 (really `OUT1`), `IN`=0x1C (really `SDIO_SELECT`) | `esp32_gpio:14` |
| 10 | SPI `CLOCK`/`USER`/`USER1`/`PIN`/`SLAVE` offsets wrong; `SPI_USR` is bit 18, not bit 0 | `esp32_spi:13` |
| 11 | SPI/I²C never route pins — no IO_MUX or GPIO-matrix configuration | `esp32_spi:76`, `esp32_i2c:105` |
| 12 | No DPORT peripheral clock/reset enable → SPI2/3, I²C0/1, UART1/2 are dead silicon | all physical drivers |
| 13 | BME280 reads the **pressure** register as temperature, with no compensation math | `bme280:60` |
| 14 | Board manifest `irq` values wrong (real: GPIO 22, SPI2 30, UART0 34, I²C0 49); SPI2 base paired with VSPI's pins | `esp32_wrover.rs:30` |
| 15 | Non-volatile `*val = ...` MMIO writes in UART init | `esp32_uart:93` |
| 16 | No README, LICENSE, CI, or toolchain pin | repo root |
| 17 | Demo tasks hardcoded into `FlintMain`; no board selection mechanism | `main.rs:33`, `board.rs:6` |
| 18 | Internal planning documents shipped as user-facing docs | `doc/` |

## P2 — real bugs, acceptable in a clearly-labelled preview

| # | Item | Location |
|---|---|---|
| 19 | Recursive `lock()` enqueues the task as its own waiter → permanent self-deadlock | `mutex.rs:68` |
| 20 | `unlock()` never checks ownership — any task can release any mutex | `mutex.rs:104` |
| 21 | `try_recv` wins the `head` CAS before reading the payload → ISR producer can overwrite a claimed slot | `queue.rs:85` |
| 22 | `wake_one_*` pops the list head without checking state → lost wakeup plus stolen delivery | `kernel/queue.rs:156` |
| 23 | `process_timers` holds `&mut TIMERS` across the callback → aliasing UB if the callback re-registers | `timer.rs:82` |
| 24 | `MutexGuard` is unintentionally `Send` | `api/mutex.rs:45` |
| 25 | Blocking APIs callable from ISR context; they block the *interrupted* task | `timer.rs`, `interrupt.rs` |
| 26 | Panic handler neither masks interrupts nor halts; other tasks keep running | `debug/panic.rs:23` |
| 27 | `dma_broker::alloc` unchecked `u32` overflow → aliased handles | `dma_broker.rs:57` |
| 28 | Queue timeout re-arms on every retry → unbounded total wait | `api/queue.rs:121` |
| 29 | Mutex table exhaustion (16 slots, leaked on panic) → user `lock()` spins forever | `mutex.rs:12` |
| 30 | Priority boost applied before the waiter-capacity check | `mutex.rs:79` |
| 31 | `request_switch()` touches `global()` outside any critical section | `scheduler.rs:383` |
| 32 | `IDLE_PRIORITY` (47) collides with the legal `Background(15)` | `scheduler.rs:29` |
| 33 | `spawn` silently truncates oversized stack requests | `spawn.rs:57` |
| 34 | SPI RX unpacks one byte per 32-bit `SPI_W` word | `esp32_spi:66` |
| 35 | Unbounded spin-waits with no timeout in UART/SPI/I²C | all three |
| 36 | Makefile is Windows-only; `Cargo.lock` gitignored for a binary workspace | `Makefile`, `.gitignore` |

## P3 — cleanup

Missing `license`/`repository` on all crates; no `[workspace.package]` or
`[profile.release]`; directory/package name mismatches (`esp32_uart` vs
`esp32-uart`, `board` vs `flint-board`); unused `flint-arch-xtensa` dependency
in all four physical drivers; `irom_seg` overstated by ~832 KB; `build.rs`
missing `rerun-if-changed` for the linker script; stale "32 priority levels"
doc comment against `NUM_PRIORITIES = 48`; `RawTrapFrame` vestigial; SSD1306
`print_temp` draws a bar rather than digits and is I²C-only despite claiming
SPI; IRQ double-registration silently ignored; 44 dead-code warnings.

---

## Withdrawn claims

Recorded so they are not re-raised:

- **"`_flint_trap` is compiled call0, so `callx4` is an ABI mismatch."**
  Refuted by disassembly — the function begins `entry a1, 0x110`, a windowed
  prologue. The call is ABI-appropriate.
- **"Window overflow/underflow vectors are infinite loops."** They are correct
  canonical spill/fill stubs.
- **"`startup.S` fails to disable the Super Watchdog."** The Super-WDT is an
  ESP32-S2/S3/C3 feature; classic ESP32 has no such registers. The internal
  plan document was wrong, not the code.

---

## Method

Findings were produced by six parallel reviewers and then verified against the
source by hand; register-level claims were checked against the ESP32 Technical
Reference Manual and Espressif's own headers, and ABI claims were settled by
disassembling the linked ELF. Reviewer output was treated as a hypothesis to
test, not as a result to report.
