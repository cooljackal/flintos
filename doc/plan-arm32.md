<!-- SPDX-License-Identifier: Apache-2.0 -->

# Plan: a second architecture (ARM32)

FlintOS targets the ESP32 (Xtensa LX6) today. This is the plan for making a
second architecture — ARM32, e.g. a Cortex-M — **additive** rather than a
kernel fork. It came out of an adversarial portability audit (2026-08-20).

## Bottom line

FlintOS is **about half-ready**. The good half is real: the tick, critical
section, and SMP are genuine `hal` traits, and the host stand-ins in
`kernel::arch::host` prove the "swap the arch under the kernel" pattern works.
The other half is that the seam is **by name, not by trait** — the kernel and a
few `hal` types reach into `soc-esp32` and `arch-xtensa` concretes directly, so
adding ARM32 today would mean editing the kernel, not adding a crate.

The keystone, which two independent audit passes landed on separately:
**`TaskContext` is Xtensa's register file living in the portable `hal` crate**
(`hal/src/types.rs:34-67`, `size_of == 112`), and it must become an
`Arch::Context` associated type before a second architecture can exist. Its own
doc comment already says so.

The rule that governs the whole effort: **do the seam refactors while there is
still only one architecture**, validating each step against `make test-host`
and the on-target suite. Do not start `arch-arm32` until the abstractions are
green, or you debug the abstraction and the new port at once.

## Current state

### Clean — keep as the model
- `hal::TickSource`, `hal::CriticalSection`, `hal::MultiCore` are genuine traits
  with a working host implementation.
- The SoC crate correctly owns the ESP32-shaped machinery: `dma_desc` (lldesc),
  `gpio_matrix`, `dport::ClockBit`, `io_mux`, `cpu_clk`, `reset`, `rtc`,
  `intr_map`. None of it leaks into `hal`.
- Logical drivers (`bme280`, `mpu6886`, `ssd1306`, `bmi270`) talk only to
  `api::bus`; already SoC-agnostic. `check-layers` enforces the tiers.
- Board manifests localize every per-board fact (pins, IRQs, base addrs,
  `LOOPBACK_SCRATCH_GPIO`, `ADC_EXTERNAL_HIGH_GPIO`).
- `arch-xtensa` is honestly named (`XtensaTick`, `XtensaSmp`); the leaks are in
  `hal` and `kernel`, not the arch crate. `dport`/`rtc_cntl` were already
  deleted out of `arch-xtensa` — the right instinct, and the direction to follow.

### Where chip/arch specifics leak across the boundary

| # | Coupling | Evidence | Layer |
|---|---|---|---|
| A | `TaskContext` is the Xtensa register frame in portable `hal` | `hal/src/types.rs:34-67`, `:91` `size_of==112` | hal |
| B | `arch.rs` re-exports named `arch_xtensa` symbols, not a trait | `kernel/src/arch.rs:31-38` | kernel |
| C | Trap handler decodes Xtensa exceptions inline at the switch point | `kernel/src/switch.rs:83,86,90,129-131,149-150` | kernel |
| D | `init_context` hand-builds an Xtensa windowed frame | `kernel/src/spawn.rs:239-273`; stack geometry `:24-30` | kernel |
| E | `request_switch` triggers the Xtensa software interrupt directly | `kernel/src/scheduler.rs:617-619` → `arch::registers::request_switch` | kernel |
| F | `wait_for_interrupt`/`wait_masked` are raw `waiti` asm in the kernel | `kernel/src/arch.rs:45-54` | kernel |
| G | `is_dma_capable` names `soc_esp32::dma::reachable` (unconditional dep) | `kernel/src/heap.rs:240`, `kernel/Cargo.toml:52` | kernel→soc |
| H | Interrupt routing calls the ESP32 matrix by name | `kernel/src/interrupt.rs:303` `soc_esp32::intr_map::route` | kernel→soc |
| I | Boot path names `soc_esp32::{cpu_clk,reset,rtc,crosscore,intr_map}` | `kernel/src/boot.rs:130,243-247,317,516-517` | kernel→soc |
| J | CPU-clock measurement (kernel-resident, #62) still names `read_ccount`/`rtc` | `kernel/src/boot.rs:315-349` | kernel |
| K | ESP32 flash-cache-off path is Xtensa `INTENABLE` math in neutral kernel | `kernel/src/interrupt.rs:372-426`, `switch.rs:77-81` | kernel (really SoC) |
| L | `Signal` enum freezes Espressif vocabulary into portable `hal` | `hal/src/pinmux.rs:34-67` (`RmtOut`,`LedcHs`,`TwaiTx`,`I2sTxData`…) | hal |
| M | Build wiring hardcodes the Xtensa linker script and target | `tools/build/src/lib.rs:27,51,68`; `.cargo/config.toml` | build |
| N | Driver poll timeouts are raw spin counts, meaningless on a new clock/core | every driver's poll loop | drivers |

## Target-state layering

`hal` stays contracts-only but gains the missing traits; `soc-esp32`/`arch-xtensa`
*implement* them instead of being called by name; `soc-arm32`/`arch-arm32`
implement the same traits. The kernel selects a set via `cfg`/features in one
place, the way it already selects a board.

New `hal` contracts:

| Trait | Replaces the by-name call | ESP32 impl | ARM32 impl |
|---|---|---|---|
| `Arch` (with `type Context`, `init_context`, `request_switch`, `wait_*`, trap entry, optional `cycle_counter`) | A, B, C, D, E, F, J | `arch-xtensa` | `arch-arm32` |
| `DmaReach` (`is_dma_capable`) | G | `soc_esp32::dma::reachable` | Cortex-M SRAM window |
| `InterruptController` (`route`/`enable`/`mask`) | H | `soc_esp32::intr_map` (source→CPU matrix) | NVIC (vectored) |
| `ClockGate` (`enable`/`disable`) | driver `dport::enable(ClockBit)` | `dport` | RCC/`AHBENR` |
| `CpuClock` + `ResetCause` | I | `soc_esp32::{cpu_clk,reset}` | chip RCC / reset regs |
| `Signal` extensibility (`#[non_exhaustive]` or a trait) | L | ESP32 signal map | AF-number model |

## Phased plan

Each phase keeps the ESP32 path building and passing `make test-host && make
check-layers` (and, for driver-touching steps, the on-target suite).

### Phase 1 — HAL contracts (each step independently shippable, low regression risk)
1. **`DmaReach` trait.** Sever the one *unconditional* kernel→SoC link
   (`heap.rs:240`). Smallest, highest-symbolism.
2. **`InterruptController` trait.** Wrap `intr_map::route` (`interrupt.rs:303`,
   `boot.rs:516`). Highest payoff — matrix-vs-NVIC is where a naïve port breaks
   hardest.
3. **`ClockGate` trait.** Abstract `dport::enable(ClockBit)`; hand drivers the
   gate instead of naming `dport`. Driver-by-driver, parallelizable.
4. **`CpuClock` + `ResetCause` traits.** Repoint `boot.rs:130,243-247`.
5. **`Signal` extensibility.** Do before ARM32 driver work so RMT/LEDC/TWAI/I2S
   names don't freeze into the portable enum.

### Phase 2 — kernel↔arch (the keystone; do while one arch exists)
1. **`TaskContext` → `Arch::Context` associated type** (A). Move the struct into
   `arch-xtensa`; `hal` keeps only neutral types; TCB/scheduler/`spawn`/`switch`
   name `A::Context`. Keep the host stand-in as a third impl.
2. **Introduce `Arch`; route `request_switch`, `wait_*`, `init_context`
   through it** (B, E, F, D). Turns `kernel::arch` into a one-line selection
   point; moves the Xtensa stack-geometry constants (`spawn.rs:24-30`) into the
   arch.
3. **Neutralize `switch::_flint_trap`** (C). Arch returns a neutral `TrapCause`
   (`Tick`/`SwitchRequest`/`Irq(n)`/`Fault{..}`) the kernel switches on, so
   PendSV can substitute without editing `switch.rs`.
4. **`Arch::cycle_counter` hook (#62) + gate the flash-cache-off path (K) behind
   a SoC feature.** Lets `measure_cpu_hz` compile for any arch, and stops the
   ESP32 `INTENABLE` mechanism compiling on ARM32. **Fold the driver
   timeout-as-duration fix (N) in here** — once there is a portable cycle
   counter, the shared poll helper expresses bounds in µs instead of iteration
   counts.

### Phase 3 — build wiring & kernel Cargo restructure
- Select linker script + target by `CARGO_CFG_TARGET_ARCH` in `tools/build`
  (M); per-arch `.cargo/config` profile.
- Move `soc-esp32` and the esp32 driver deps in `kernel/Cargo.toml` behind a
  **SoC-selection feature**, mirroring the board-selection pattern. This is what
  finally makes ARM32 additive.

### Phase 4 — `arch-arm32` + `soc-arm32` skeletons
Only after 1–3 are green. If the seam is right, this adds files without editing
kernel logic — the success criterion for the whole effort. SysTick tick,
PRIMASK/BASEPRI critical section, PendSV switch, RCC clock gate, NVIC
controller, alternate-function pinmux, stream/channel DMA (no lldesc, no GPIO
matrix).

## Sequencing at a glance
- **First, cheap, high-leverage:** Phase 1.1 (`DmaReach`) and 1.2
  (`InterruptController`).
- **The gate:** Phase 2.1 (`Arch::Context`) — everything ARM32 waits on it;
  schedule early despite being the largest single item.
- **Hard rule:** do not start Phase 4 until Phases 1–3 are green.

## Related issues
- **#62** CPU-clock measurement — folds into Phase 2.4 (needs the `cycle_counter`
  hook; the measurement was already relocated into the kernel).
- **#63** interrupt route/register/enable dance — *mostly resolved* already by
  `kernel::interrupt::connect`; verify before re-opening.
- **#64** ESP32 drivers hand-roll register helpers — the pre-Phase-0 cleanup
  (route drivers through `soc_esp32::reg`) is the ESP32-side down payment;
  `ClockGate` (Phase 1.3) is the portable follow-through.

## Note on the Phase-0 cleanup
The dead-code, duplication, and magic-number cleanup that preceded this plan is
ESP32-local and does not itself port anything — but it removes copies that would
otherwise be duplicated *per architecture*. The shared `soc_esp32::reg` and
`poll` helpers, and the single documented timeout constant, are the things a
second SoC crate reuses instead of re-hand-rolling. `spi-bus` and `uart-bus`
were deliberately **not** removed despite having no consumers: they are slated
to become on-target-tested Layer-2 abstractions rather than deleted.
