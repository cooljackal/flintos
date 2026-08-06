# Flint RTOS — Remediation Plan

Addresses every issue raised in the rev-8 implementation review.

---

> ## Implementation status (as of this change set)
>
> **Done & host-verified** (compiles + `make test-host` green: 49 tests):
> W0.5 registers, W1.5 ABI cleanup, W2.1 CriticalSection, W2.2 critical-section
> wrapping, W2.3 lock-free MPSC queue, W3.1–W3.5 scheduler logic, W4.1–W4.3
> blocking IPC + timeouts, W5.1/W5.3 dispatch seam + phase-0 test, W6.2 interrupt
> routing framework, W6.3 DMA honesty, W7.1 layer boundary (+ `tools/check-layers.sh`,
> passing), W7.2 log/panic polish, W7.3 feature forwarding, W7.4 MPU seam + doc
> rescope.
>
> **Done but HARDWARE-UNVERIFIED** (correct-shape canonical asm; must be brought
> up on a real ESP32 at gates G0/G1 — cannot be built/tested in this environment):
> W0.1 linker map, W0.2 vectors + VECBASE, W0.3 frame size, W0.4 watchdog,
> W1.1 tick re-arm, W1.2 trap-handler switch, W1.3 task init, W1.4 window spill
> (the switch-time spill in `context.S` is a flagged stub needing the canonical
> `xthal_window_spill` sequence), W5.2 trap-frame capture. The asm/Rust ABI of the
> trap entry (`callx4` into `_flint_trap`, PS.EXCM/WOE handling) is the top bring-up risk.
>
> **Not implemented (deferred by dependency):** W6.1 (Layer-1 physical drivers as
> isolated tasks with one-IPC-hop request/reply). This is a sizeable new subsystem,
> not a fix, and there is no value wiring driver tasks until the preemption core
> (W1) is proven on hardware (G1). It is the correct next milestone after G1.

---

**Architecture decision:** *Hybrid (Option A locked)* — build a clean kernel **dispatch
seam** (direct `extern "Rust"` calls — **no** `syscall`-instruction boundary) and the
one-IPC-hop driver model now; treat **hardware MPU enforcement as optional and possibly
permanent-never**. The `MpuManager` trait stays as a clean, inert seam, but the
kernel's correctness, the driver model's soundness, and the security story must **not**
depend on it. Flint may ship — indefinitely — as a single protection domain (one shared
address space, cooperative trust between tasks). Every design choice below assumes "no MPU
ever" as the baseline and treats MPU as a pure-bonus add-on if it ever lands.

> **Consequence for the syscall boundary (W5):** with no MPU, a `syscall` instruction
> provides **no memory protection** — any task can reach kernel memory directly regardless.
> Its remaining value is a stable ABI, portability across arches, and a single trap-based
> context-save point. That is a *much* weaker justification for the heavyweight Xtensa
> trap path. **Locked decision: Option A** — keep direct-call dispatch, do not build the
> `syscall`-instruction boundary. The only surviving trap path is the timer/IRQ
> context-save (W1). See W5.

**Verification target:** real **ESP32-WROVER** hardware via `probe-rs`/`espflash` with
RTT and/or UART console. Every workstream below has a hardware acceptance gate. No
workstream is "done" until its gate passes on a physical board.

> Ordering is dependency-driven: you cannot test preemption until the board boots and the
> tick fires, so boot/memory correctness comes first, then the preemption core, then
> safety, then everything built on top.

---

## Workstream 0 — Boot, memory map, and "it runs at all"

Until the image boots with a correct memory map and a reachable vector table, nothing
else is testable. Do this first.

**Tasks**

1. **W0.1 — Fix the linker memory map.** *(Issue #6)*
   - Recompute `dram_data` length so it does not overlap `panic_region` (0x3FFF0000) or
     `dma_pool` (0x3FFF1000). Cap `dram_data` at `0x3FFB0000–0x3FFEFFFF`.
   - Add an explicit `task_stacks` region (replace the hardcoded `0x3FFC_0000` pool in
     `spawn.rs:77`) and have `spawn.rs` consume `_task_stack_start/_end` symbols instead
     of magic addresses.
   - Align `panic_region` to 8 bytes (so the `u64` in `PanicSnapshot` is legal — #16).
   - Add `ENTRY(_start)` to the linker script.

2. **W0.2 — Install VECBASE and use correct Xtensa vector offsets.** *(Issue #7)*
   - In `startup.S`, `wsr.vecbase` ← `_vector_table_start` before enabling interrupts.
   - Rewrite `vectors.S` to the canonical ESP32 LX6 vector layout: window vectors
     occupy 0x0–0x17F; then the level-2..6 / debug / NMI / kernel-exc / user-exc /
     double-exc vectors at their config-defined offsets. Cross-check against the ESP32
     TRM Table 4-3 and the `xtensa-esp32-none-elf` `specs`.
   - Window overflow/underflow vectors must be **real spill/fill stubs**, not infinite
     loops, because we run with `PS.WOE=1` (see W1.4).

3. **W0.3 — Size the exception entry frame correctly.** *(Issue #8)*
   - `vectors.S` reserves `-64` but writes through offset 0x58. Reserve ≥ 96 bytes
     (round to 16-byte alignment) and keep the `RawTrapFrame` layout in lock-step with
     `hal/types.rs`.

4. **W0.4 — Disable the watchdogs properly.** *(minor — startup.S)*
   - Write the unlock key `0x50D83AA1` to `TIMG0_WDTWPROTECT` before clearing
     `WDTCONFIG0`; repeat for `TIMG1`. Disable the RTC WDT
     (`RTC_CNTL_WDTWPROTECT`/`RTC_CNTL_WDTCONFIG0`) and the super-WDT
     (`RTC_CNTL_SWD_WPROTECT`/`SWD_CONF`). Otherwise the board resets in ~1s.

5. **W0.5 — Fix register/peripheral definitions.** *(minor — registers.rs)*
   - `CCOMPARE0` SR number is **240**, not 236.
   - Model `INTENABLE`/`INTCLEAR`/`INTERRUPT` as **CPU special registers**
     (`rsr`/`wsr`), not DPORT memory addresses. Keep the DPORT interrupt-matrix map
     (`PRO_*_MAP`) separate and correctly named.

**Hardware acceptance gate G0:** flash the image; the UART banner
`"Flint RTOS booting..."` (`startup.rs:23`) prints and the board does **not** reset-loop
for ≥ 30 s. `probe-rs` can halt and read `VECBASE` = `_vector_table_start`.

---

## Workstream 1 — The preemption core

This is the heart of the review (#1–#4). Build it as one coherent unit: a hardware tick
that re-arms, an ISR that switches context on its way out, a context switch that handles
register windows, and new tasks that actually start.

**Tasks**

1. **W1.1 — Re-arm and clear the tick in the ISR.** *(Issue #2)*
   - Make `XtensaTick::tick()` the single owner of timer hardware: read `CCOUNT`, add the
     period, `wsr.ccompare0` (this is what clears the CCOMPARE interrupt). Handle the
     "missed deadline" case (if the new compare is already in the past, advance by whole
     periods).
   - Call `XtensaTick::tick()` from the ISR (it is currently never called). Delete the
     duplicate `TICK_COUNT` so there is **one** authoritative tick counter (keep the
     scheduler's, or move it into the tick source — pick one, #2 note).

2. **W1.2 — Switch context on ISR exit (real preemption).** *(Issue #1)*
   - Restructure the timer path: the assembly entry already saved the interrupted
     context into a `RawTrapFrame`. On the way out, if `pending_switch` is set, copy the
     interrupted context into the current TCB, select `next = schedule()`, and restore
     `next`'s context such that `rfe` resumes the new task. Remove the
     "switch only in the idle loop" model — `switch_if_pending` becomes the ISR-exit
     decision, not an idle-loop poll.
   - The idle task becomes a normal lowest-priority task that just runs `waiti 0`.

3. **W1.3 — Make new tasks start at their entry point.** *(Issue #3)*
   - Either set `ctx.a[0] = entry` in `init_context` (`spawn.rs:102`) **and** make the
     restore path `ret`-based, **or** keep `a[0]=0` and make first-resume use `rfe` with
     the saved PC. Be consistent between the *initial* dispatch and *subsequent*
     dispatches (a switched-out task resumes via the same mechanism).
   - Set up the initial windowed frame so a real `entry`/`retw` task prologue is valid
     (`windowstart`/`windowbase` consistent with `a[0..3]`), and install a task-exit trampoline
     in `a0` so a task function that returns lands in a `task_exit()` that de-schedules
     it instead of returning to garbage.

4. **W1.4 — Handle register windows in the context switch.** *(Issue #4 — the #1 plan risk)*
   - Before saving in `flint_context_switch` (and in the exception entry), force a full
     window spill (`SPILL_ALL_WINDOWS` sequence) so all live frames are flushed to the
     task's own stack. Save/restore `WINDOWBASE`/`WINDOWSTART` consistently with the
     spilled state.
   - Document the chosen approach (spill-to-stack vs. save-all-64-physical-regs) in
     `docs/syscall_abi.md`.

5. **W1.5 — Fix `restore_context` special-register writes.** *(Issue #10)*
   - Remove the bogus `wsr(CCOUNT, task.sar)` line; restore SAR to SAR. Audit every
     `rsr`/`wsr` in `arch/.../syscall.rs` against `registers.rs`.

**Hardware acceptance gate G1:** three demo tasks at different priorities
(`main.rs`) each print on a steady cadence driven by the tick; a deliberately
compute-bound task (busy loop, no sleep/yield) is **preempted** by a higher-priority
task (provable by interleaved UART output). `now_ms()` advances at ~1000/s.

---

## Workstream 2 — Concurrency safety (CriticalSection + sound shared state)

Once interrupts actually preempt (G1), every `static mut` touched from both task and ISR
context is a live data race. Land this immediately after W1.

**Tasks**

1. **W2.1 — Implement `CriticalSection` for Xtensa.** *(Issue #20)*
   - Provide a token type that masks interrupts up to `CRITICAL_SECTION_PRIORITY`
     (`rsil`), restores prior `PS` on drop. Add to `arch/xtensa`.

2. **W2.2 — Wrap all kernel shared state.** *(Issue #20)*
   - Replace the `static mut` + `&'static mut` pattern (scheduler `global()` at
     `scheduler.rs:260`, `QUEUE_WAITERS`, `MUTEX_WAITERS`, `TIMERS`, log ring, interrupt
     tables) with a `CriticalSection`-guarded cell (e.g. a small `SpinLockIrq<T>` /
     `CsCell<T>` built on W2.1). No public `&'static mut` to shared state.
   - This also clears the `static_mut_refs` lint that newer toolchains deny.

3. **W2.3 — Fix the lock-free queue or document SPSC.** *(Issue #21)*
   - `api/queue.rs` `try_send`/`try_recv` are only sound for single-producer/
     single-consumer. Either (a) restrict to SPSC and document it loudly + add a
     `debug_assert` guard, or (b) make slot reservation atomic (`fetch_add` on a
     reservation index with a published/committed flag). Given `send_isr` exists,
     option (b) for the multi-producer case is the safer default.

**Hardware acceptance gate G2:** stress test — a high-rate ISR producer + task consumer +
a second task producer hammering one queue for 60 s with no lost/duplicated messages and
no fault. Run under the trace/log load to exercise the critical sections.

---

## Workstream 3 — Scheduler correctness

With a sound, preempting scheduler, fix its decision logic.

**Tasks**

1. **W3.1 — Real round-robin within a priority band.** *(Issue #11)*
   - In `schedule()`, start the scan after `current` (or maintain a per-priority rotor
     index) so equal-priority tasks share the CPU instead of the lowest index starving
     the rest.

2. **W3.2 — Correct priority inheritance.** *(Issue #12)*
   - Move the `ready_mask` bit when a task's priority changes (`boost_priority`/
     `restore_priority` must update the mask, since `schedule()` matches both the bit and
     `tcb.priority`).
   - Replace the `original_prio == 0` sentinel with `Option<u8>` (0 is a *valid* highest
     priority).
   - On unlock, recompute the owner's effective priority from *remaining* held mutexes
     (track held-mutex set per task) rather than blindly restoring; boost the new owner
     for any still-waiting higher-priority tasks. Add nested/transitive inheritance.

3. **W3.3 — Real idle task; stop clobbering TCB 0.** *(Issue #13)*
   - Reserve a dedicated idle TCB with its own small stack before any user spawn (don't
     relabel the first spawned task in `main.rs:73`). Idle = lowest priority, always
     ready, body `waiti 0`.

4. **W3.4 — Wake-driven preemption.** *(Issue #14)*
   - In `on_tick` (and in any unblock path — queue/mutex/timer), request a switch
     whenever a readied task outranks `current`, not only on quantum expiry.

5. **W3.5 — No silent failures on exhaustion.** *(Issue #15)*
   - `mutex::lock` must not hand out a guard when the mutex table is full — return an
     error / block correctly, never silently "succeed" (`mutex.rs:41`, `api/mutex.rs:79`).
   - `timer::once/every` must return `Option`/`Result` and the API must surface failure
     (`timer.rs:42`).
   - Replace the `panic!` in `QueueWaiters::find_or_create` (`kernel/queue.rs:109`) with a
     graceful error; raise the table sizes or make them board-configurable.

**Hardware acceptance gate G3:** (a) two equal-priority tasks demonstrably time-slice;
(b) classic priority-inversion scenario (low holds mutex, high waits, medium spins) shows
the low task inheriting high's priority and the high task proceeding promptly — provable
via `flint> tasks` priority column / trace.

---

## Workstream 4 — Blocking IPC and timeouts

The headline IPC primitive currently doesn't block (#5). Wire the API to the
already-written kernel waiter machinery and implement timeouts.

**Tasks**

1. **W4.1 — Wire blocking send/recv.** *(Issue #5)*
   - `api/queue.rs` `send`/`recv` must call into the kernel
     (`_flint_sys_queue_send/recv`) on the blocking path instead of falling back to
     `try_*`. Connect to `kernel/queue.rs` `block_send`/`block_recv`/`wake_*` (currently
     dead code). On wake, retry the operation and return the message.
   - Resolve the type-erasure: the kernel waiter table is keyed by queue address; the
     data buffer lives in the typed `Queue<T,N>`. Define the protocol (kernel blocks/wakes
     by address; the API owns the buffer) in `docs/queue_protocol.md`.

2. **W4.2 — Implement timeouts.** *(Issue #5)*
   - Honor `timeout_ms`: `0` = try-once, `u32::MAX` = forever, else register a wake at
     `now + timeout` (reuse the sleep/timer machinery). On timeout, remove the task from
     the queue waiter list and return `Timeout`. Remove the ignored `_timeout_ms`
     (`kernel/queue.rs:118`).

3. **W4.3 — Waiter removal on all exit paths.** *(correctness)*
   - Ensure a task removed from a waiter list (timeout, or being woken) is removed exactly
     once; no stale IDs left in `send_waiters`/`recv_waiters`.

**Hardware acceptance gate G4:** producer/consumer across a `Queue<T,N>` with
`recv(Forever)` truly blocks (consumer shows 0% CPU via stack/HWM + trace until a message
arrives); `recv(Ms(t))` returns `Timeout` after ~t ms with no message.

---

## Workstream 5 — Kernel dispatch seam + IRQ trap path (Option A, locked)

**Decision (locked): Option A.** Flint stays a single protection domain, so a
`syscall`-instruction boundary would buy no isolation. We keep `flint-api` → kernel as
direct `extern "Rust"` calls and do **not** build the full Xtensa `syscall` trap ABI. The
only trap path that remains is the timer/IRQ context-save from W1 — this removes the
heavyweight, double-exception-prone syscall router entirely.

**Tasks**

1. **W5.1 — Clean up the direct-call dispatch seam.** *(Issue #17)*
   - Keep `_flint_sys_*` as the kernel-side dispatch surface, called directly via the
     `extern "Rust"` shims in `flint-api`. Consolidate and document them as the single
     kernel entry surface in `docs/syscall_abi.md` (retitle it "kernel dispatch ABI").
   - Delete the unused `syscall`-instruction plumbing: the `_kernel_entry` vector arm for
     `syscall`, `XtensaSyscallABI::enter`/`return_to_task` as a *syscall* mechanism, and
     the `exccause == 0` arm of `_rust_exception_handler` that pretends to dispatch
     syscalls. Retain `RawTrapFrame`/`SyscallABI` **only** as needed by W1 for IRQ
     context-save, and rename/retarget them so that is their sole, explicit purpose.

2. **W5.2 — Fix the trap-frame capture for the IRQ path only.** *(Issue #9)*
   - #9 (entry stub clobbers `a2`, mis-stores EPC1 into the `a[2]` slot) now matters only
     for the timer/IRQ trap entry, the sole surviving trap path. Fix `vectors.S` so the
     interrupted register state is captured faithfully for W1's context save. There is no
     longer a syscall-argument contract to preserve.

3. **W5.3 — Make the Phase-0 self-tests valid and cheap.** *(minor — phase0_test.rs)*
   - Drop test (a) (syscall-under-window-pressure) — there is no `syscall` instruction
     anymore. Keep and repair test (b): replace `fib(45)` (≈1.8 B calls) with a bounded
     workload (e.g. `fib(25)` or an iterative checksum) that still spans several ticks, and
     assert it matches the uninterrupted result. Gate stays behind `phase0-tests`.

**Hardware acceptance gate G5:** the timer/IRQ trap path saves/restores context correctly
across a windowed call chain (verified result from the W5.3 test); the direct-call
dispatch seam is documented and exercised by the W1/W4 demos. No `syscall`-instruction
path remains in the build.

---

## Workstream 6 — One-IPC-hop driver model + interrupt routing

Per the decision, build the real driver model: Layer 1 as an isolated task reached by one
queue message (#18), with interrupt forwarding (#22). Layer 2/3 remain in-process libs.

**Tasks**

1. **W6.1 — Layer-1 physical drivers as tasks.** *(Issue #18)*
   - Turn `esp32_spi`/`esp32_i2c`/`esp32_uart` physical drivers into tasks that own their
     peripheral and serve a typed request/response queue (`PhysTransfer` message). The
     Layer-2 `Bus` impls (`spi_bus`, etc.) send one queue message and await the reply,
     instead of calling `phys.raw_transfer()` directly (`spi_bus/lib.rs:25`).
   - Keep a `#[cfg]`/feature for "in-process" mode for host unit tests (the current mocks).

2. **W6.2 — Interrupt → driver-queue routing.** *(Issue #22)*
   - `interrupt::dispatch` must forward to the registered `target_queue` (currently
     ignored). Top-half fills an event and `send_isr`s it to the driver task; bottom-half
     runs at the driver's priority.
   - Remove the `&'static mut` aliasing in `next_event` (`interrupt.rs:75`) — use the W2
     guarded-cell pattern or a lock-free slot ring.
   - Fix `clear_pending`: clear via the `INTCLEAR` special register / correct DPORT
     offset, not bit `1<<irq` at the DPORT base (`interrupt.rs:85`).

3. **W6.3 — DMA broker (Phase-3 scope, keep labeled).** *(minor — dma_broker.rs)*
   - Use the linker `_dma_pool_start/_end` symbols instead of the `DMA_POOL_MAX` constant;
     make `submit`/`await_transfer` either real or clearly return "unimplemented" rather
     than silently `Ok`. Acceptable to remain a labeled stub for now, but stop returning
     misleading success.

**Hardware acceptance gate G6:** BME280 reads temperature over the real SPI Layer-1
*task* (one IPC hop, provable in trace: `QUEUE_SEND`→driver→`QUEUE_SEND` back). A GPIO
button IRQ unblocks a driver task via the routed queue.

---

## Workstream 7 — Layer-boundary enforcement, debug polish, and doc alignment

Cleanup and the "make the architecture true" items.

**Tasks**

1. **W7.1 — Enforce the layer dependency boundary.** *(Issue #19)*
   - Decide the real contract: logical/bus crates depend on **`flint-api`** (re-exporting
     the `Bus`/`BusHandle` surface), not `flint-hal` directly. Update every driver
     `Cargo.toml` (e.g. `bme280/Cargo.toml:8`) and the doc template, which currently
     disagree (doc says `flint_api::bus`, code uses `flint_hal::bus`).
   - Add a CI check (a small `xtask`/test) that fails if a `drivers/logical/**` or
     `drivers/bus/**` crate has `flint-arch-*` or a Layer-1 crate in its dependency tree —
     making the "can't import hardware" claim real.

2. **W7.2 — Debug/log/panic polish.** *(Issues #16 + minors)*
   - `PanicSnapshot`: write to the now-8-aligned region (W0.1); actually capture `pc`/`ps`
     from the trap frame instead of hardcoding `0` (`panic.rs:56`).
   - `log::write`/`panic::handle`: slice formatted buffers to written length
     (`&buf[..pos]`) instead of `from_utf8` over NUL padding (`log.rs:60`, `panic.rs:67`).
   - `log::dump()` currently computes then discards (`let _ = (...)` at `log.rs:81`) — make
     `dmesg` actually print the ring buffer.

3. **W7.3 — Gate debug features for true zero-cost release.** *(minor — Cargo features)*
   - Make the `flint-log`/`flint-metrics`/`flint-trace` features actually gate the code
     paths (the `log_info!` calls in `main.rs` are currently unconditional). Verify with
     `cargo bloat` that release (`debug-level-0`) compiles the macros to nothing — the
     plan's zero-tolerance item.

4. **W7.4 — Re-scope the docs to match the hybrid decision.** *(Issues #17/#18/#19 framing)*
   - Update `flint_rtos_plan_rev8.md` to tell the truth: Flint is a **single protection
     domain** (one shared address space, tasks cooperatively trusted). Hardware MPU
     isolation is an **optional, possibly-never** add-on, not a promised phase. Rewrite the
     plan's microkernel/"isolated user-space tasks"/"unauthorized register access faults"
     claims so they don't promise protection the product may never have. The architectural
     value retained is the *clean three-layer driver model and one-IPC-hop IPC*, not memory
     isolation — say so explicitly.
   - Keep `mpu.rs`/`MpuManager` as a clean, inert seam with an `// OPTIONAL (may never
     ship):` banner; remove the nonsensical `raw |= desc.base` encoding
     (`arch/.../mpu.rs:42`). Ensure nothing in the kernel calls into it on the hot path —
     it must be a no-op the rest of the system never depends on.

**Hardware acceptance gate G7:** `dmesg`/`panic` show correct content after a forced
panic and reboot; a release build (`debug-level-0`) contains no log/trace symbols
(`cargo bloat`/`nm`); the layer-boundary CI check fails a deliberately-bad driver.

---

## Optional / may never ship (per decision)

- **Hardware MPU enforcement** (`MpuManager` activation per context switch, fault on
  unauthorized register access, MPU audit doc). This is **not** a committed phase — Flint
  may remain a single protection domain indefinitely. The `MpuManager` trait and a no-op
  impl stay as an inert seam so the option remains open, but **no kernel correctness,
  driver soundness, or security claim may depend on it**. If it is ever built, it is pure
  additive hardening, not a prerequisite for any other workstream.
- **The full `syscall`-instruction trap boundary** is **dropped** (Option A locked, W5).
  Flint uses direct-call dispatch. If a concrete MPU/multi-domain need ever appears, the
  trap boundary can be added then — it is not built now.

## Unaffected future work

- Phases 5+ of the original plan (wizard, filesystem, TCP/IP, ARM port) are unaffected by
  this remediation and proceed once W0–W7 are green.

---

## Issue → Workstream coverage map

| # | Issue | Workstream |
|---|---|---|
| 1 | No preemption (cooperative only) | W1.2 |
| 2 | Tick never re-armed / dead `XtensaTick::tick` | W1.1 |
| 3 | New tasks jump to address 0 | W1.3 |
| 4 | Context switch ignores register windows | W1.4 |
| 5 | Blocking queue/timeouts not wired | W4.1, W4.2 |
| 6 | Linker regions overlap | W0.1 |
| 7 | VECBASE not set / wrong vector offsets | W0.2 |
| 8 | Exception entry frame too small (overflow) | W0.3 |
| 9 | Trap-frame capture corrupted by entry stub (IRQ path) | W5.2 |
| 10 | `restore_context` writes nonsense SRs | W1.5 |
| 11 | Round-robin doesn't rotate | W3.1 |
| 12 | Priority inheritance: mask/sentinel/nesting | W3.2 |
| 13 | Idle task clobbers first spawned TCB | W3.3 |
| 14 | Woken higher-priority sleeper doesn't preempt | W3.4 |
| 15 | Silent failures on exhaustion (mutex/timer/queue panic) | W3.5 |
| 16 | `PanicSnapshot` misaligned u64 | W0.1, W7.2 |
| 17 | No microkernel/syscall boundary | W5.1 (Option A locked: clean direct-call seam; no `syscall` instruction); MPU optional/never |
| 18 | Three-layer is direct calls, not IPC | W6.1 |
| 19 | Layer boundary unenforced + wrong deps | W7.1 |
| 20 | `CriticalSection` unimpl + `static mut` races | W2.1, W2.2 |
| 21 | API queue unsafe for concurrent producers | W2.3 |
| 22 | Interrupt routing doesn't route; `clear_pending` wrong | W6.2 |
| — | CCOMPARE0 number / INTENABLE as address | W0.5 |
| — | Watchdog disable incorrect | W0.4 |
| — | `phase0_test` fib(45) / inconsistent test | W5.3 |
| — | log/panic UTF-8 NUL padding; `dump()` no-op | W7.2 |
| — | DMA broker uses constant / misleading `Ok` | W6.3 |
| — | Debug features not actually gated | W7.3 |
| — | `mpu.rs` nonsensical encoding | W7.4 |

---

## Execution order (critical path)

```
W0 (boot/memory)  →  W1 (preemption core)  →  W2 (concurrency safety)
                                                   │
        ┌──────────────────────────────────────────┤
        ▼                  ▼                        ▼
   W3 (scheduler)     W4 (blocking IPC)        W5 (dispatch seam + IRQ trap)
        └──────────────────┬───────────────────────┘
                           ▼
                   W6 (driver model + IRQ routing)
                           ▼
                   W7 (enforcement + polish + docs)
```

W3, W4, W5 can proceed in parallel once W2 lands. W6 depends on W4 (queues) and W5
(boundary). W7 is last.
