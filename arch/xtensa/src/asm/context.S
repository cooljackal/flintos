// SPDX-License-Identifier: Apache-2.0

// Xtensa LX6 first-dispatch trampoline.
//
// This file used to also carry flint_spill_all_windows, flint_context_switch
// and flint_restore_first -- a cooperative switch path written before the trap
// handler existed. Nothing ever called them: switching happens entirely
// through the trap return in vectors.S, which saves the interrupted frame and
// returns a different one. They were deleted rather than kept as a spare
// mechanism, because flint_spill_all_windows spilled nothing -- it computed
// WINDOWBASE and WINDOWSTART and returned, with no `rotw` anywhere in it -- so
// wiring the switch path up later would have silently corrupted any task more
// than one call frame deep. A stub that looks like a working primitive is
// worse than no primitive.
//
// The real spill lives in vectors.S, runs on every trap, and is the canonical
// `and a12, a12, a12` / `rotw` sequence.

.section .text
.align 4

// ── _flint_task_start ────────────────────────────────────────────────────────
//
// First-dispatch trampoline for a newly spawned task.
//
// A task used to be entered by pointing the restored PC straight at its entry
// function and hand-building the window state around it: PS.CALLINC preset to
// 1, WINDOWSTART/WINDOWBASE synthesised, and the return address placed in a0.
// That last part is wrong. In a real `call4` the return address lands in the
// *caller's* a4, which becomes the callee's a0 only after `entry` rotates the
// window; a0 in the caller's frame is the caller's own return address. The
// synthesised frame therefore presented a window layout the hardware never
// produces, and the task's first `entry` never retired.
//
// This trampoline is deliberately written without an `entry` of its own, so it
// runs directly in whatever window `rfe` restored, using only the stack pointer
// the context supplies. It then reaches the task through a genuine `callx4`, so
// the hardware sets CALLINC, the return address and the window state itself.
//
// On entry (from the restored task context):
//   a0 = 0            terminates the register-window spill chain
//   a1 = stack pointer
//   a3 = task entry function
.global _flint_task_start
.type   _flint_task_start, @function
_flint_task_start:
    movi a0, 0                  // no caller: stop any spill walking past here
    callx4 a3                   // real windowed call into the task

    // The task returned. Hand off to the kernel's task-exit path, which
    // de-schedules it; that never returns, so the loop below is a backstop.
    .extern flint_task_exit
    movi a3, flint_task_exit
    callx4 a3
.Ltask_start_hang:
    j .Ltask_start_hang
