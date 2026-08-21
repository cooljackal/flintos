<!-- SPDX-License-Identifier: Apache-2.0 -->

# Plan: a second architecture (ARM32)

FlintOS targets the ESP32 (Xtensa LX6) today. This is the plan for making a
second architecture — ARMv6-M on the RP2040 first — **additive** rather than a
kernel fork. It came out of adversarial portability audits (2026-08-20 and
2026-08-21) and is grounded by a Wio RP2040 mini board available for testing.

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

Two feasibility gates now come before that refactor. First, prove how the
RP2040 will implement the compare-and-swap and fetch-style atomics FlintOS uses:
ARMv6-M has no native exclusive-load/store primitive, and masking interrupts on
one core does not exclude the other. Second, prove the board's boot2, image
layout, LED and console with a disposable hardware probe. Neither proof may
grow into a competing HAL.

After those gates, do the seam refactors while there is still only one
production architecture, validating each step against the required host,
layer, lint, Xtensa-build and on-target checks. Do not start the real
`arch-armv6m` kernel port until the abstractions are green.

The scope is deliberately split:

- `arch/armv6m`: Cortex-M0+ exception frames, PRIMASK, SysTick, PendSV, WFI.
- `soc/rp2040`: clocks, resets, NVIC integration, SIO, core launch and memory.
- `board-wio-rp2040-mini`: exact flash, crystal, pins, LED and console wiring.
- The ESP8285 Wi-Fi companion and multicore scheduling are later milestones,
  not evidence that the ARM architecture works.

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

Each production phase keeps the ESP32 path building and runs, before commit,
`make test-host && make lint && make check-layers && make check-all`. Changes
that touch target behavior also run the appropriate attached-board suite.

### Phase 0 — feasibility and hardware ground truth (blocking)
1. **Identify the board path.** Record the exact PCB/module revision and
   schematic. With BOOT held during reset, observe USB VID `2e8a` and the
   `RPI-RP2` volume. The CP210x bridge currently visible as COM7 is not the Wio
   until wiring or output proves it.
2. **Pin vendor evidence.** Add the Rust `thumbv6m-none-eabi` target and pin
   authoritative Raspberry Pi RP2040/pico-sdk boot, memory, interrupt and core
   launch sources plus the Seeed schematic. Do not transcribe registers from
   this tree's comments.
3. **Prove atomic feasibility early.** Compile representative `AtomicU8`,
   `AtomicU32` and `AtomicUsize` compare-exchange/fetch operations for
   `thumbv6m-none-eabi`; inspect emitted symbols/code. Choose and document one
   sound backend: RP2040 SIO hardware serialization, a pinned portable-atomic
   implementation with a proven RP2040 backend, or exclude RP2040. It must be
   safe both before and after core 1 starts. No context-switch work begins
   until this decision is tested.
4. **Run a disposable first-light probe.** Using vendor-derived boot2 and
   startup, produce and inspect ELF/BIN/UF2, then flash through ROM UF2. Prove
   reset-to-code, the user LED (expected GP13, including polarity), and a UART
   marker on schematic-confirmed pins. This isolates boot/image/clock/pin
   mistakes from FlintOS scheduler mistakes.

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

### Phase 4 — `arch-armv6m` + `soc-rp2040` + Wio board skeletons
Only after 1–3 are green. If the seam is right, this adds files without editing
kernel logic — the success criterion for the whole effort. Start core 0 only.
Use SysTick for the tick, PendSV at the lowest priority for switching, PSP in
thread mode and MSP in handler mode. ARMv6-M has **PRIMASK but no BASEPRI**:
nested critical sections must save and restore the exact prior PRIMASK state,
and the HAL contract must not promise priority-threshold masking this CPU cannot
provide. Build the initial exception frame in the arch crate, including the
Thumb bit, stack alignment, EXC_RETURN behavior and task-return trampoline.

### Phase 5 — single-core kernel proof on the Wio
Bring up `hello`, not the ESP32-heavy `demo`, in this order: raw boot marker,
tick, cooperative PendSV switch, preemptive two-task switch, sleep/WFI, heap and
stack guards, interrupt-driven queue traffic, then fault capture. Split the
target harness into portable and ESP32-specific assertions; a reduced test
count must not be reported as parity.

Measured acceptance on the Wio:

- Repeated reset reaches the same boot marker and memory-map assertions.
- Tick rate is checked against an independent host clock or logic analyzer.
- Callee-saved registers, PSP, xPSR and task return survive repeated preemption.
- Nested critical sections suppress and then restore tick delivery.
- Yield, sleep, timeout, queue and heap tests pass under interrupt load.
- An injected fault produces a bounded diagnostic and a known halt/reset.

### Phase 6 — RP2040 SMP, then optional board peripherals
Only after a single-core soak: implement the documented ROM FIFO launch for
core 1, per-core vector/stack/idle state, cross-core reschedule, shared time
ownership and the chosen atomic/lock backend. Stress that one task never runs
on two cores and queues/scheduler locks make bounded progress under contention.
Wi-Fi, native USB console and other peripherals follow separately.

## Sequencing at a glance
- **First, cheap, high-leverage:** Phase 1.1 (`DmaReach`) and 1.2
  (`InterruptController`).
- **Before both:** Phase 0 atomics and first-light are stop/go gates, not port
  milestones that can be deferred.
- **The gate:** Phase 2.1 (`Arch::Context`) — everything ARM32 waits on it;
  schedule early despite being the largest single item.
- **Hard rule:** do not start Phase 4 until Phase 0 is proved and Phases 1–3 are
  green.

## Build and test acceptance

- `cargo tree` for `thumbv6m-none-eabi` contains no `arch-xtensa`, `soc-esp32`
  or `esp32-*` crates; unsupported architecture/SoC pairs fail clearly.
- `hello` builds for both ESP32/Xtensa and Wio/RP2040; ELF inspection confirms
  boot2, vectors, reset and PendSV symbols land in the intended regions.
- The Makefile selects target, linker, image format, flash and monitor from an
  explicit architecture/SoC/board tuple and never silently flashes a family.
- `make check-all` grows to compile both production targets before ARM support
  can be called covered.
- Hardware claims are labeled measured. Compile-only and host-only results are
  never presented as proof of interrupt, timing, context or multicore behavior.

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
