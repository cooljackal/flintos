<!-- SPDX-License-Identifier: Apache-2.0 -->

# FlintOS — Review Findings

Consolidated output of an adversarial review of the entire tree, conducted
before the first public release. Six independent reviewers covered the
scheduler and context switch, boot and memory map, synchronisation and IPC,
the driver stack, release packaging, and empirical build verification.

Every finding below was confirmed by reading the source. Where a reviewer's
claim did not survive verification, it is recorded in
[Withdrawn claims](#withdrawn-claims) rather than deleted.

**Baseline commit:** the state of the tree at first import.

> ## Remediation status
>
> **Fixed:** P0 items 1, 2, 4, 5, 6 · P1 items 7–15 · P2 items 19–36.
> Host tests went from 49 to 99 in the process.
>
> **Open at the time of the review — tracked as GitHub issues, which are the
> source of truth.** This document is the historical record of the review; it is
> not updated as work lands, so check the
> [issue list](https://github.com/cooljackal/flintos/issues) for what is
> actually still open. Notably #1 and #15 are closed: FlintOS boots, schedules
> and preempts on an ESP32-PICO.
>
> | Issue | Item |
> |---|---|
> | [#1](https://github.com/cooljackal/flintos/issues/1) | P0 — register-window spill on context switch |
> | [#2](https://github.com/cooljackal/flintos/issues/2) | P1 — I²C GPIO-matrix pin routing |
> | [#3](https://github.com/cooljackal/flintos/issues/3) | P1 — demo tasks hardcoded in `FlintMain` |
> | [#4](https://github.com/cooljackal/flintos/issues/4) | P1 — multi-board support; M5Stack Atom manifest |
> | [#5](https://github.com/cooljackal/flintos/issues/5) | P1 — replace internal planning docs with user docs |
> | [#6](https://github.com/cooljackal/flintos/issues/6) | P1 — program the CPU clock instead of assuming it |
> | [#7](https://github.com/cooljackal/flintos/issues/7) | P2 — panic handler does not mask interrupts or halt |
> | [#8](https://github.com/cooljackal/flintos/issues/8) | P2 — `request_switch()` outside a critical section |
> | [#9](https://github.com/cooljackal/flintos/issues/9) | P2 — `IDLE_PRIORITY` collides with `Background(15)` |
> | [#10](https://github.com/cooljackal/flintos/issues/10) | P2 — `spawn()` silently truncates stacks |
> | [#11](https://github.com/cooljackal/flintos/issues/11)–[#14](https://github.com/cooljackal/flintos/issues/14) | P3 — naming, stale docs, `RawTrapFrame`, build warnings |
> | [#15](https://github.com/cooljackal/flintos/issues/15) | Hardware bring-up gates G0 and G1 |
>
> **Discovered during remediation, not in the original review:** the tree did
> not compile for Xtensa at all. `global_asm!` routes the assembly through
> LLVM's integrated assembler, which rejects the windowed instructions the
> exception vectors are built from (`s32e`, `l32e`, `rfwo`, `rfwu`), and no
> target-feature flag makes it accept them. The original "builds cleanly"
> verdict came from a stale artifact cargo considered fresh. The assembly is
> now built with `xtensa-esp32-elf-gcc` from `build.rs`.
>
> Several further defects surfaced only when register values were checked
> against Espressif's headers rather than reasoned about: the I²C command
> opcodes were each encoded one higher than the hardware expects, `MS_MODE`
> was written to the wrong bit, and `wait_done` polled bit 0 rather than
> `TRANS_COMPLETE` at bit 7. Classic ESP32 also turns out to have no
> IO_MUX-native I²C pins at all, so that driver now refuses every
> configuration rather than pretending to work.

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

---

# Second review — 2026-08-22

Scope: duplication, dead code, magic numbers, and design/best-practice
refactors across the whole tree. Five parallel finders (kernel/api,
arch/soc/hal, physical drivers, logical/bus drivers, radio/lib), each output
then handed to a skeptic prompted to refute it. 24 findings survived; several
false positives were killed in verification (e.g. the `api::Deadline` vs
`kernel::deadline_for` "duplication" — the two use different forever sentinels
and add strategies, so it is not shared code).

None of these block anything. Two are latent correctness traps, not just
cleanups — flagged ⚠.

## Medium

| # | Kind | Location | Finding | Fix |
|---|---|---|---|---|
| 1 ⚠ | design | `drivers/physical/esp32/gpio` `lib.rs:129` | `set_mode` silently ignores pull and open-drain: `InputPullUp/Down` act as plain `Input`, `OutputOpenDrain` as plain `Output` — the rest is dropped with `let _ = mode;` | Program the pull/open-drain bits, or reject the mode with `InvalidConfig` rather than accepting a config it won't honor |
| 2 | dead-code | `kernel/scheduler.rs:723` | `pub fn request_switch_all` fully documented, zero callers, not re-exported, not an ABI symbol | Remove it, or wire it into the cross-core fan-out it was written for |
| 3 | dead-code | `arch/xtensa/registers.rs:13` | Cluster of `PS_*` bitfield consts + `read_windowbase/windowstart` referenced nowhere | Delete (this file already pruned `write_ps`/`restore_ps` the same way) |
| 4 | magic-number | `arch/armv6m/smp.rs:10`, `tick.rs:57` | RP2040 SIO CPUID `0xd000_0000` hard-coded raw, while `critical_section.rs` names `SIO_CPUID` and soc names `SIO_BASE` | One shared named const |
| 5 | duplication | `soc/esp32/dma.rs:74` + `soc/rp2040/lib.rs` | DMA `reachable` range-check boilerplate near-identical across both SoCs | `hal::dma::range_within(addr, len, low, high)`; only the window bounds differ |
| 6 | duplication | `drivers/physical/esp32/adc2/lib.rs:51` | adc2 duplicates adc wholesale: `Attenuation`, `FULL_SCALE`, SAR power-up sequence, `read`/`read_averaged` | Extract an `esp32-adc-core` both build on |
| 7 | duplication | `drivers/physical/esp32/spi/slave.rs:61` + `dma.rs` | `SPI_SYNC_RESET`/`SPI_TRANS_DONE`/`SPI_CK_I_EDGE` re-declared in both sibling modules | Define once in `lib.rs` as `pub(crate)` |
| 8 | duplication | `drivers/physical/esp32/touch/lib.rs:194` | Reimplements local `read`/`write`/`modify` (its `modify` is byte-for-byte `soc_esp32::reg::modify`); adc/adc2/dac also open-code RMW | Use `soc_esp32::reg` as i2s/mcpwm/pcnt do |
| 9 | design | `drivers/logical/ssd1306/lib.rs:129` | `clear()` sends the 1024-byte GRAM fill one byte per bus transaction, each re-sending address + `0x40` | Batch into `max_transfer`-sized data writes |
| 10 | dead-code | `drivers/bus/spi-bus/lib.rs:20` | `config: BusConfig` field written in `new()`, never read (masked by `#[allow(dead_code)]`) | Drop the field, or consult it in `set_speed`/`transfer` |
| 11 | dead-code | `lib/wpa/keydata.rs:104` | `KDE_HDR` const unused; `let _ = KDE_HDR;` discards it and its comment implies a cursor advance that `i = body_end` already does | Delete the const, the discard, and the comment |

## Low

| # | Kind | Location | Finding | Fix |
|---|---|---|---|---|
| 12 ⚠ | design | `drivers/physical/esp32/ledc/lib.rs:211` | `SIG_OUT_EN & !CONF0_IDLE_LV` binds tighter than the surrounding `\|`, so the `& !CONF0_IDLE_LV` is an inert no-op; idle-low works only because it's a whole-register write | Drop the misleading clause, or parenthesize the intent |
| 13 | duplication | `kernel/selftest.rs:233` | GPIO-gated check/skip block copy-pasted ~7×, test-name string duplicated across both arms so they can drift | `check_or_skip(name, gpio, reason, closure)` helper naming the test once |
| 14 | duplication | `arch/armv6m/tick.rs:26` | SysTick reload computation written twice, `0x00ff_ffff` repeated | `fn reload_value()` + named `SYST_RELOAD_MAX` |
| 15 | magic-number | `arch/xtensa/appcpu.rs:28` | APP-CPU stack size `4096` repeated in the struct, static, and top-of-stack math | `const APPCPU_STACK_BYTES` |
| 16 | dead-code | `kernel/smp.rs:217` | `scheduling_cores` used only by a test; doc comment is a stray duplicate | Delete or `#[cfg(test)]` |
| 17 | dead-code | `drivers/physical/esp32/i2c/lib.rs:78` | `I2C_SDA_SAMPLE` (0x34) is a redundant alias of `I2C_SDA_SAMPLE_REG`, used only by its own test | Delete, point the test at `_REG` |
| 18 | magic-number | `drivers/physical/esp32/twai/lib.rs:120` | LOM mode bit inline `1 << 1` while siblings `MODE_RM/STM/AFM` are named | `const MODE_LOM` |
| 19 | dead-code | `drivers/logical/ws2812/lib.rs:63` | `Timing::reset_us` (80) populated but never read; `finish()` takes no timing, so the latch duration is never communicated | Pass it to the emitter, or remove it |
| 20 | duplication | `drivers/logical/bmi270/lib.rs:94` + `mpu6886/lib.rs:139` | `read_reg` byte-for-byte identical | Shared single-register-read helper |
| 21 | magic-number | `drivers/logical/bme280/lib.rs:164` | `write_reg(REG_RESET, 0xB6)` — bare soft-reset word, unlike annotated neighbours | Name it |
| 22 | duplication | `radio/esp32/adapter.rs:1558` | `log_c_str` and `c_str_bounded` are two near-identical bounded C-string readers (also `nvs::c_str`, `tasks::store_name`) | One shared bounded-C-string helper |
| 23 | magic-number | `radio/esp32/phy_init.rs:114` | Six `limit(...)` calls hardcode floor `40` and ceilings `78/72/66/60/56/52`, duplicating `TX_POWER_FLOOR`/`TX_POWER_CEILINGS` defined just above | Reference the named constants |
| 24 | duplication | `lib/crypto/sha1.rs:53` | `Sha1::update`/`finish` duplicate ~50 lines of block-buffering/padding from `Sha256`; only the compress fn + digest length differ | Shared buffer/padding helper or trait |

## Remediation

Fixed 1–23 across five commits (one per area), verified with `make test-host`,
`make lint`, `make check-layers` and `make check-all`. Two were handled with a
deliberate deviation from the literal suggestion:

- **#6** — a sibling `esp32-adc-core` crate would break the Layer-1 dependency
  rule, so the shared `Attenuation` and `FULL_SCALE` moved to `soc_esp32::sar`
  instead. The SAR power-up and read sequences were **not** merged: ADC1, ADC2
  and the radio-shared SAR differ in load-bearing ways and carry hardware-only
  bugs, so that merge waits for on-target verification.
- **#24** — declined. SHA-1 and SHA-256 are kept self-contained and auditable
  against their FIPS specs; a shared generic hashing framework would add
  abstraction to security-critical code for a low-severity gain.

The GPIO (#1) and other register-sequence changes are compile-verified only;
`make test-target` on a DevKitC should run before they are relied on.
