<!-- SPDX-License-Identifier: Apache-2.0 -->

# Plan: finish the portability seam

FlintOS already runs its abstraction spine on two architectures — Xtensa/ESP32
and ARMv6-M/RP2040 both compile, and the CPU-context, tick, SMP, DMA-reach,
SoC-facts, critical-section and bus/driver contracts are genuine `hal` traits
with real implementations on each. This plan is the **finish line**, not a new
start: a handful of kernel subsystems still name `esp32_*` crates by hand, one
SMP method is defaulted, the `Signal` enum leaks vendor names, driver timeouts
are clock-dependent, and the build can flash only ESP32. Each item below is
**additive and independently shippable**; none is a rewrite.

## Bottom line

The goal is that 99.99% of the code speaks only in abstractions
(`kernel`/`task`/`bus`/`Signal`/device traits) and the irreducible
architecture-specific parts live behind those traits in `arch-*`/`soc-*`
crates. After this plan, `grep esp32_ kernel/src` outside `selftest_*` and the
SoC adapter files returns nothing.

## Already portable — do not touch

| Layer | State |
|---|---|
| CPU/context (`Architecture::Context`) | associated type; real Xtensa/ARMv6-M/host impls |
| SMP identity, tick, DMA-reach, SoC facts, critical section | real impls on both SoCs |
| Logical drivers (bme280, bmi270, ssd1306, mpu6886, ws2812) | `api::bus` only, `forbid(unsafe_code)` |
| Physical drivers | implement generic traits; no chip type leaks into `hal`/`api` |
| Build skeleton | ARM linker script, `CARGO_CFG_TARGET_ARCH` branching, board→(arch,soc) tuple |

## Phase A — close known leaks (small, high-symbolism)

| Item | Now | After |
|---|---|---|
| `Signal` enum (`hal/src/pinmux.rs`) | `RmtOut`/`LedcHs`/`TwaiTx`/`TwaiRx` vendor names; not `#[non_exhaustive]` | `PulseOut`/`PwmOut`/`CanTx`/`CanRx`; `#[non_exhaustive]` |
| `XtensaSmp::request_reschedule` (`arch/xtensa/src/smp.rs`) | defaulted → `false`; other core waits for its next tick | real, over `soc_esp32::crosscore::raise` |
| Boot flash-park (`kernel/src/boot.rs:454`) | names `soc_esp32::crosscore`/`intr_map` directly | shares the cross-core primitive above |

The `Signal` rename must not collide with the `pinmux::Signal` the app prelude
already re-exports (`api::prelude`); the cross-core reason type is named
`CrossCore`/`Reason`, never `Signal`.

## Phase B — the bulk: subsystems still naming ESP32 (new `hal` traits)

Each becomes a `hal` trait, implemented in `soc-esp32`/`arch-xtensa`, invoked
generically from the kernel. Ordered by priority.

| Kernel module | Names now | New trait | Priority |
|---|---|---|---|
| `watchdog.rs` | `esp32_wdt`, **unconditional (no cfg)** | `hal::watchdog::Watchdog` | highest |
| `alarm.rs` | `esp32_timg::lact` one-shot compare | `hal::timer::OneShotAlarm` | high |
| `clock.rs` | `esp32_timg` free-running counter | `hal::timer::FreeRunningCounter` | high |
| `power.rs` | `soc_esp32::sleep` light/deep | `hal::power::{LightSleep, DeepSleep}` | medium |
| `boot.rs` + `nvs.rs` | `esp32_flash` hooks/park/region | `hal::flash::{SafeInterrupts, Region}` | medium |
| `interrupt_controller.rs` | kernel-internal trait (works) | promote into `hal` | cleanup |

## Phase C — clock-independent timeouts

`soc_esp32::poll::until(max_spins)` centralizes the spin-wait, but spin counts
mean different wall-clock durations at different CPU clocks. Convert to
`until_us(deadline_us)` backed by `Architecture::cycle_count()` (already exists)
and the monotonic clock. Migrate the six callers; keep the count-based API as a
thin shim during transition.

## Phase D — build tuple completion

| Item | Now | After |
|---|---|---|
| linker/ROM select (`tools/build/src/lib.rs`) | per-arch only; `esp32.rom.ld` hardcoded | per-(arch,soc); ROM optional |
| `.cargo/config.toml` | only `[target.xtensa-esp32-none-elf]` | add `[target.thumbv6m-none-eabi]` + runner |
| Makefile flash | `espflash --chip esp32` hardcoded | dispatch tool+flags on `BOARD → (arch,soc)` |
| radio blobs (`tools/build/src/blobs.rs`) | `.blobs/esp32` hardcoded | per-SoC; skip if none |

## Not a wrapper — a feasibility gate

ARMv6-M has no native compare-and-swap and PRIMASK masks only the local core,
so the spinlock/atomics backend on RP2040 is a stop/go decision, not portability
plumbing. It stays a gate (per `doc/plan-arm32.md` Phase 0), out of this plan's
trait work.

## Acceptance

- `grep -rE 'esp32_|soc_esp32|arch_xtensa' kernel/src` matches only
  `selftest_*`, the SoC adapter files (`board.rs`, `interrupt_controller.rs`),
  and intentional re-exports.
- `make test-host && make lint && make check-layers && make check-all` green
  after every phase.
- Each phase keeps the ESP32 target building; hardware-affecting changes run the
  attached-board suite before commit.
