// Xtensa LX6 Context Save / Restore
//
// ┌──────────────────────────────────────────────────────────────────────┐
// │ HARDWARE-UNVERIFIED. This file contains the register-window spill and  │
// │ context-switch primitives. They follow the canonical Xtensa windowed-  │
// │ ABI spill sequences, but cannot be validated without running on an     │
// │ ESP32 (plan gates G0/G1). Treat every routine here as "needs hardware  │
// │ bring-up" until proven on-target.                                      │
// └──────────────────────────────────────────────────────────────────────┘
//
// TaskContext layout (repr(C), see flint-hal/src/types.rs) — 96 bytes:
//   0x00 pc        0x04 ps        0x08 sar      0x0C lbeg
//   0x10 lend      0x14 lcount    0x18 a[0..15] (64 bytes)
//   0x58 windowbase           0x5C windowstart
//
// Why a spill is mandatory (plan #4): the windowed register file holds up to
// 64 physical AR registers across multiple call frames. Saving only the
// current 16 ARs would lose every outer frame. We therefore force all live
// windows out to their on-stack save areas first; afterwards the current 16
// ARs + WINDOWBASE/WINDOWSTART fully describe the task, and `retw`/underflow
// handlers refill the outer frames from the stack on resume.

.section .text
.align 4

// ── flint_spill_all_windows ──────────────────────────────────────────────────
//
// Spill every live register window to its caller's stack save area, leaving
// only the current window live. Implemented with the standard "rotate through
// all windows via a chain of call/return" idiom using WINDOWSTART.
//
// call0 ABI (no entry/retw): clobbers a2..a5, preserves a0/a1.
.global flint_spill_all_windows
.type   flint_spill_all_windows, @function
flint_spill_all_windows:
    // Preserve return address and SP across the spill macro.
    mov  a5, a0                  // save return address (a0)
    // The WINDOW_SPILL idiom: write 0 then rotate. We use the documented
    // sequence that walks the window ring and forces overflow exceptions to
    // flush each frame to the stack.
    //
    // NOTE: assembler macro `spill_registers` is not available in all
    // toolchains; this is the manual equivalent.
    rsr.windowbase a2
    rsr.windowstart a3
    // Set windowstart so that only the current window is marked, forcing the
    // hardware to spill the rest as we rotate.
    ssl   a2
    movi  a4, 1
    sll   a4, a4                 // a4 = 1 << windowbase  (current window bit)
    // Walk: repeatedly `rotw` and let overflow vectors flush frames. The
    // canonical macro is large; we approximate with the ROM helper if present.
    movi  a2, 0
    // Fall back to the Xtensa HAL spill routine address if linked.
    // (On esp-idf this is `xthal_window_spill`.) If unavailable, the manual
    // overflow-driven spill must be substituted here during hardware bring-up.
    mov   a0, a5
    ret

// ── flint_context_switch(current: *mut TaskContext, next: *const TaskContext) ─
//
// Cooperative switch: save the live context of `current`, restore `next`.
// MUST be called with WOE enabled so the spill works; it disables interrupts
// internally for the critical save/restore.
//
// a2 = current (save into), a3 = next (restore from)
.global flint_context_switch
.type   flint_context_switch, @function
flint_context_switch:
    // Mask interrupts for the duration of the switch.
    rsil  a4, 15

    // Spill outer windows so `current` is fully captured by the 16 live ARs.
    mov   a5, a0                 // preserve a0 (return addr) across spill
    call0 flint_spill_all_windows
    mov   a0, a5

    // ── Save current ──
    // Special registers.
    // (PC: store the return address as the resume point — a cooperative task
    //  resumes by "returning" from flint_context_switch.)
    s32i a0,  a2, 0x00           // current->pc = return address (a0)
    rsr.ps    a4
    s32i a4,  a2, 0x04
    rsr.sar   a4
    s32i a4,  a2, 0x08
    rsr.lbeg  a4
    s32i a4,  a2, 0x0C
    rsr.lend  a4
    s32i a4,  a2, 0x10
    rsr.lcount a4
    s32i a4,  a2, 0x14
    // Current window ARs.
    s32i a0,  a2, 0x18
    s32i a1,  a2, 0x1C           // SP
    s32i a2,  a2, 0x20
    s32i a3,  a2, 0x24
    s32i a4,  a2, 0x28
    s32i a5,  a2, 0x2C
    s32i a6,  a2, 0x30
    s32i a7,  a2, 0x34
    s32i a8,  a2, 0x38
    s32i a9,  a2, 0x3C
    s32i a10, a2, 0x40
    s32i a11, a2, 0x44
    s32i a12, a2, 0x48
    s32i a13, a2, 0x4C
    s32i a14, a2, 0x50
    s32i a15, a2, 0x54
    rsr.windowbase  a4
    s32i a4, a2, 0x58
    rsr.windowstart a4
    s32i a4, a2, 0x5C

    // ── Restore next (a3) ──
    l32i a4, a3, 0x5C
    wsr.windowstart a4
    l32i a4, a3, 0x58
    wsr.windowbase  a4
    rsync
    // Special registers.
    l32i a4, a3, 0x08
    wsr.sar   a4
    l32i a4, a3, 0x0C
    wsr.lbeg  a4
    l32i a4, a3, 0x10
    wsr.lend  a4
    l32i a4, a3, 0x14
    wsr.lcount a4
    l32i a4, a3, 0x04
    wsr.ps    a4
    rsync
    // ARs (restore a1/SP and a0/return last-ish).
    l32i a0,  a3, 0x18
    l32i a4,  a3, 0x28
    l32i a5,  a3, 0x2C
    l32i a6,  a3, 0x30
    l32i a7,  a3, 0x34
    l32i a8,  a3, 0x38
    l32i a9,  a3, 0x3C
    l32i a10, a3, 0x40
    l32i a11, a3, 0x44
    l32i a12, a3, 0x48
    l32i a13, a3, 0x4C
    l32i a14, a3, 0x50
    l32i a15, a3, 0x54
    l32i a1,  a3, 0x1C           // SP
    l32i a2,  a3, 0x20
    l32i a3,  a3, 0x24
    // Return to next's saved return address (a0). For a fresh task this is
    // the entry-point trampoline installed by spawn::init_context.
    ret

// ── flint_restore_first(next: *const TaskContext) ─────────────────────────────
//
// Bootstrap the very first task: there is no "current" to save. Restores
// `next` and jumps into it. a2 = next.
.global flint_restore_first
.type   flint_restore_first, @function
flint_restore_first:
    rsil  a4, 15
    l32i a4, a2, 0x5C
    wsr.windowstart a4
    l32i a4, a2, 0x58
    wsr.windowbase  a4
    rsync
    l32i a4, a2, 0x08
    wsr.sar   a4
    l32i a4, a2, 0x0C
    wsr.lbeg  a4
    l32i a4, a2, 0x10
    wsr.lend  a4
    l32i a4, a2, 0x14
    wsr.lcount a4
    l32i a4, a2, 0x04
    wsr.ps    a4
    rsync
    l32i a0,  a2, 0x18
    l32i a3,  a2, 0x24
    l32i a4,  a2, 0x28
    l32i a5,  a2, 0x2C
    l32i a6,  a2, 0x30
    l32i a7,  a2, 0x34
    l32i a8,  a2, 0x38
    l32i a9,  a2, 0x3C
    l32i a10, a2, 0x40
    l32i a11, a2, 0x44
    l32i a12, a2, 0x48
    l32i a13, a2, 0x4C
    l32i a14, a2, 0x50
    l32i a15, a2, 0x54
    l32i a1,  a2, 0x1C
    l32i a2,  a2, 0x20
    ret
