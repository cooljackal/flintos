// SPDX-License-Identifier: Apache-2.0

// Xtensa LX6 Relocatable Exception Vector Table (ESP32)
//
// ┌──────────────────────────────────────────────────────────────────────┐
// │ This banner used to claim the window handlers "use the canonical       │
// │ Xtensa spill/fill sequences". Four of the six did not, and the claim   │
// │ is exactly why nobody checked them for a year. They are now literal    │
// │ transcriptions -- see the note above _WindowOverflow4 -- and the way   │
// │ to verify that is to diff them against Zephyr and NuttX, not to read   │
// │ a sentence in a comment.                                              │
// └──────────────────────────────────────────────────────────────────────┘
//
// VECBASE is set to _vector_table_start in startup.S. Layout (offsets from
// VECBASE), per the ESP32 TRM / esp-idf xtensa_vectors:
//   0x000 WindowOverflow4     0x040 WindowUnderflow4
//   0x080 WindowOverflow8     0x0C0 WindowUnderflow8
//   0x100 WindowOverflow12    0x140 WindowUnderflow12
//   0x180 Level2Interrupt     0x1C0 Level3Interrupt
//   0x200 Level4Interrupt     0x240 Level5Interrupt
//   0x280 DebugException       0x2C0 NMI
//   0x300 KernelException      0x340 UserException
//   0x3C0 DoubleException
//
// NOTE: the reset vector is NOT here — reset is a fixed ROM address; we are
// entered via ENTRY(_start). Offset 0x0 is WindowOverflow4.

// Raw UART0 output helpers for the double-exception dump. UART0 TX FIFO is at
// 0x3FF40000; no FIFO-space check, because by this point the system is dead and
// a dropped character costs less than a hang in the fault handler.
.macro DBL_CH c
    movi a2, 0x3FF40000
    movi a5, \c
    s32i a5, a2, 0
.endm

// Print \val as 8 hex digits. Clobbers a2, a3, a4, a5, a6.
.macro DBL_HEX val
    mov  a3, \val
    movi a4, 8
90: extui a5, a3, 28, 4
    movi a6, 10
    bge  a5, a6, 91f
    addi a5, a5, 48             // '0'
    j    92f
91: addi a5, a5, 87             // 'a' - 10
92: movi a2, 0x3FF40000
    s32i a5, a2, 0
    slli a3, a3, 4
    addi a4, a4, -1
    bnez a4, 90b
.endm

.section .vectors, "ax"
.global _vector_table_start
.global _vector_table_end

.align 1024
_vector_table_start:

// ── Window overflow and underflow ─────────────────────────────────────────
//
// These six are transcribed from the Xtensa ISA reference, and are identical
// instruction for instruction in Zephyr (arch/xtensa/core/window_vectors.S)
// and NuttX (arch/xtensa/src/common/xtensa_window_vector.S). Both were read
// rather than remembered. **Do not paraphrase them.**
//
// The two-base rule is the part that is easy to get wrong, and this file had
// it wrong for call8 and call12 until the radio blob found it:
//
//   * a0-a3 of call[j] are saved in **call[j+1]'s** frame, addressed from the
//     callee's stack pointer, which the hardware hands over in a5 / a9 / a13.
//   * a4-a7 (and a8-a11 for call12) are saved at the top of **call[j]'s own**
//     frame, which is addressed from **call[j-1]'s** stack pointer -- and that
//     has to be *loaded* first, out of the save area at `[a1 - 12]`. It is not
//     a1.
//
// Using a1 as the base for the second group is self-consistent enough to
// survive casual use: every frame's spill lands one frame too low, and the
// matching underflow reads back from the same wrong place. It comes apart in
// two situations. The frame with no callee below it writes beneath the live
// stack pointer, straight into where the next interrupt puts its trap frame;
// and any point where the call widths change breaks the shift.
//
// Which is why nothing noticed for so long. FlintOS's own Rust code overflows
// almost never -- 64 physical registers hold a call chain deeper than the
// kernel ever gets. Espressif's blobs are GCC-compiled, nest deeply, and use
// call8 and call12 throughout, so they overflow constantly. `register_chipv7_phy`
// double-faulted at a `retw` every time it was preempted, and ran perfectly
// with interrupts masked -- because masking removed the trap frame that was
// landing on the misplaced spill.

// ── 0x000  Window Overflow 4 ──────────────────────────────────────────────
//
// call[j] did a call4. a0-a3 are the registers to save; a5 is call[j+1]'s
// stack pointer. There is no second group, which is why this one was right.
.org 0x000
_WindowOverflow4:
    s32e    a0, a5, -16             // save a0 to call[j+1]'s stack frame
    s32e    a1, a5, -12
    s32e    a2, a5, -8
    s32e    a3, a5, -4
    rfwo                            // rotates back to call[i] position

// ── 0x040  Window Underflow 4 ─────────────────────────────────────────────
.org 0x040
_WindowUnderflow4:
    l32e    a0, a5, -16             // restore a0 from call[i+1]'s stack frame
    l32e    a1, a5, -12
    l32e    a2, a5, -8
    l32e    a3, a5, -4
    rfwu

// ── 0x080  Window Overflow 8 ──────────────────────────────────────────────
//
// a0-a7 are the registers to save; a8-a15 must be preserved; a9 is call[j+1]'s
// stack pointer.
.org 0x080
_WindowOverflow8:
    s32e    a0, a9, -16             // save a0 to call[j+1]'s stack frame
    l32e    a0, a1, -12             // a0 <- call[j-1]'s sp; finds the end of
                                    //       call[j]'s own frame
    s32e    a1, a9, -12
    s32e    a2, a9, -8
    s32e    a3, a9, -4
    s32e    a4, a0, -32             // save a4 to call[j]'s stack frame
    s32e    a5, a0, -28
    s32e    a6, a0, -24
    s32e    a7, a0, -20
    rfwo

// ── 0x0C0  Window Underflow 8 ─────────────────────────────────────────────
//
// a0-a7 must be reloaded; a8-a15 are call[i+1]'s a0-a7 and must be preserved;
// a9 is call[i+1]'s stack pointer.
//
// a1 is restored before it is used as the source for call[i-1]'s sp, and a7
// serves as the scratch for that pointer before being loaded with its own
// value last. The order is load-bearing; it is not an arbitrary listing.
.org 0x0C0
_WindowUnderflow8:
    l32e    a0, a9, -16             // restore a0 from call[i+1]'s stack frame
    l32e    a1, a9, -12
    l32e    a2, a9, -8
    l32e    a7, a1, -12             // a7 <- call[i-1]'s sp
    l32e    a3, a9, -4
    l32e    a4, a7, -32             // restore a4 from call[i]'s stack frame
    l32e    a5, a7, -28
    l32e    a6, a7, -24
    l32e    a7, a7, -20             // a7 last: it is the base until now
    rfwu

// ── 0x100  Window Overflow 12 ─────────────────────────────────────────────
//
// a0-a11 are the registers to save; a12-a15 must be preserved; a13 is
// call[j+1]'s stack pointer. The second group is eight registers, so it
// reaches to -48 rather than -32.
.org 0x100
_WindowOverflow12:
    s32e    a0,  a13, -16           // save a0 to call[j+1]'s stack frame
    l32e    a0,  a1,  -12           // a0 <- call[j-1]'s sp
    s32e    a1,  a13, -12
    s32e    a2,  a13, -8
    s32e    a3,  a13, -4
    s32e    a4,  a0,  -48           // save a4 to the end of call[j]'s frame
    s32e    a5,  a0,  -44
    s32e    a6,  a0,  -40
    s32e    a7,  a0,  -36
    s32e    a8,  a0,  -32
    s32e    a9,  a0,  -28
    s32e    a10, a0,  -24
    s32e    a11, a0,  -20
    rfwo

// ── 0x140  Window Underflow 12 ────────────────────────────────────────────
//
// a0-a11 must be reloaded; a12-a15 are call[i+1]'s a0-a3; a13 is call[i+1]'s
// stack pointer. a11 is the scratch for call[i-1]'s sp, and is loaded with its
// own value last.
.org 0x140
_WindowUnderflow12:
    l32e    a0,  a13, -16           // restore a0 from call[i+1]'s stack frame
    l32e    a1,  a13, -12
    l32e    a2,  a13, -8
    l32e    a11, a1,  -12           // a11 <- call[i-1]'s sp
    l32e    a3,  a13, -4
    l32e    a4,  a11, -48           // restore a4 from the end of call[i]'s frame
    l32e    a5,  a11, -44
    l32e    a6,  a11, -40
    l32e    a7,  a11, -36
    l32e    a8,  a11, -32
    l32e    a9,  a11, -28
    l32e    a10, a11, -24
    l32e    a11, a11, -20           // a11 last: it is the base until now
    rfwu

// ── 0x180  Level-2 Interrupt ──────────────────────────────────────────────
.org 0x180
    j _flint_unhandled

// ── 0x1C0  Level-3 Interrupt ──────────────────────────────────────────────
.org 0x1C0
    j _flint_unhandled

// ── 0x200  Level-4 Interrupt ──────────────────────────────────────────────
.org 0x200
    j _flint_unhandled

// ── 0x240  Level-5 Interrupt ──────────────────────────────────────────────
.org 0x240
    j _flint_unhandled

// ── 0x280  Debug Exception ────────────────────────────────────────────────
.org 0x280
    j _flint_unhandled

// ── 0x2C0  NMI ────────────────────────────────────────────────────────────
.org 0x2C0
    j _flint_unhandled

// ── 0x300  Kernel Exception ───────────────────────────────────────────────
.org 0x300
    j _flint_trap_entry

// ── 0x340  User Exception (also level-1 interrupts) ───────────────────────
.org 0x340
    j _flint_trap_entry

// ── 0x3C0  Double Exception (unrecoverable) ───────────────────────────────
.org 0x3C0
    j _double_exception

_vector_table_end:

// ─────────────────────────────────────────────────────────────────────────
//  Trap entry — exceptions and level-1 interrupts
//
//  Builds a 96-byte frame matching RawTrapFrame on the interrupted stack,
//  saves the live window, calls the Rust handler, then restores and `rfe`s.
//  The Rust handler (_flint_trap) decides tick/dispatch/preemption.
//
//  These handlers MUST live in IRAM next to the vectors: the vector slots reach
//  them with a `j` instruction, whose range cannot span IRAM→flash (IROM).
// ─────────────────────────────────────────────────────────────────────────
.section .iram.text, "ax"

.align 4
.global _flint_trap_entry
.type   _flint_trap_entry, @function
_flint_trap_entry:
    // ── Phase 1: specials, without touching a1 ────────────────────────────
    //
    // a1 must keep the interrupted task's real stack pointer until the
    // register windows have been spilled, and that constraint drives the whole
    // shape of this prologue.
    //
    // A window overflow writes a caller's a0-a3 to `[sp - 16]` of the frame
    // *below* it, and the matching underflow reads them back from the same
    // place when the task returns. Both addresses come from a1 in the window
    // being spilled. If a1 has been moved down to make room for a trap frame,
    // the spill lands 128 bytes away from where the underflow will look, and
    // the task returns into whatever was already in that memory. On hardware
    // that surfaced as a stack pointer of 0x3f401da7 -- a rodata address --
    // which then faulted this very instruction sequence and escalated to a
    // double exception.
    //
    // So the specials are written through a scratch frame pointer while a1 is
    // left alone, and a1 only moves after the spill. This is the same ordering
    // constraint Zephyr and NuttX satisfy by spilling before their exception
    // frame is allocated.
    //
    // EXCSAVE1/EXCSAVE2 hold the two registers we need as scratch. They do not
    // rotate with the window, and nothing between here and the reload below
    // touches them: the window overflow and underflow vectors use only s32e /
    // l32e against the frame registers.
    wsr.excsave1 a2
    wsr.excsave2 a3

    // -128 = size_of::<TaskContext>() + the 16-byte ABI save area. Asserted
    // in lib.rs (TRAP_STACK_BYTES) so a change to TaskContext fails the build
    // here rather than quietly writing two fields into the save area.
    addi a3, a1, -128           // frame base, 16 bytes clear of the ABI save area

    rsr.epc1   a2
    s32i a2,  a3, 0x00          // pc
    rsr.ps     a2
    s32i a2,  a3, 0x04          // ps
    rsr.sar    a2
    s32i a2,  a3, 0x08
    rsr.lbeg   a2
    s32i a2,  a3, 0x0C
    rsr.lend   a2
    s32i a2,  a3, 0x10
    rsr.lcount a2
    s32i a2,  a3, 0x14
    // SCOMPARE1: the comparand of an in-flight `s32c1i` retry loop. A handler
    // that performs an atomic read-modify-write clobbers it, and the
    // interrupted loop then spins against the wrong value forever. NuttX and
    // Zephyr both save it here; see `hal::TaskContext::scompare1`.
    rsr.scompare1 a2
    s32i a2,  a3, 0x60

    // ── Leave exception mode ──────────────────────────────────────────────
    //
    // Hardware sets PS.EXCM on exception entry, and while EXCM is set a window
    // overflow cannot be serviced normally -- it escalates directly to the
    // double exception vector. The spill below depends on those overflows
    // dispatching, and so does the `callx4` into the Rust handler further on.
    //
    // Raise INTLEVEL to 3 (the ESP32's EXCM level) so nothing can nest while
    // this trap is being serviced. EPC1 is already in the frame, so the window
    // exceptions are free to overwrite it.
    movi a2, 0x40003            // PS: WOE=1, INTLEVEL=3, EXCM=0, UM=0
    wsr.ps a2
    rsync

    rsr.excsave1 a2
    rsr.excsave2 a3             // register file pristine again, a1 untouched

    // ── Spill every live window to its owner's stack ──────────────────────
    //
    // The canonical sequence (Zephyr SPILL_ALL_WINDOWS, NuttX/FreeRTOS
    // SPILL_REGISTERS). `and a12, a12, a12` is an identity write whose only
    // purpose is to reference a register belonging to an older live window,
    // which raises the overflow that writes that window to memory; `rotw`
    // steps WINDOWBASE around the ring so every window is touched in turn.
    // 3+3+3+3+4 = 16 slots, a full trip around the 64-register file, so
    // WINDOWBASE ends where it started and nothing is destroyed.
    //
    // Afterwards WINDOWSTART is exactly `1 << WINDOWBASE`, which is why the
    // values snapshotted below can be restored verbatim on the way out.
    and a12, a12, a12
    rotw 3
    and a12, a12, a12
    rotw 3
    and a12, a12, a12
    rotw 3
    and a12, a12, a12
    rotw 3
    and a12, a12, a12
    rotw 4

    // ── Phase 2: the frame proper ─────────────────────────────────────────
    addi a1, a1, -128           // 112-byte frame + the 16-byte ABI save area above it

    s32i a0,  a1, 0x18
    addi a0,  a1, 128           // the interrupted SP
    s32i a0,  a1, 0x1C          // a[1]
    s32i a2,  a1, 0x20
    s32i a3,  a1, 0x24
    s32i a4,  a1, 0x28
    s32i a5,  a1, 0x2C
    s32i a6,  a1, 0x30
    s32i a7,  a1, 0x34
    s32i a8,  a1, 0x38
    s32i a9,  a1, 0x3C
    s32i a10, a1, 0x40
    s32i a11, a1, 0x44
    s32i a12, a1, 0x48
    s32i a13, a1, 0x4C
    s32i a14, a1, 0x50
    s32i a15, a1, 0x54
    rsr.windowbase  a2
    s32i a2, a1, 0x58
    rsr.windowstart a2
    s32i a2, a1, 0x5C           // == 1 << windowbase, courtesy of the spill

    // Call the Rust handler with the frame pointer.
    // ABI NOTE: _flint_trap is a windowed extern "C" fn. We clear a0 to
    // terminate the call chain (so a spill walking the chain stops here) and
    // pass the frame in a6 (→ callee a2 under call4 rotation); the return
    // value comes back in a6.
    movi a0, 0
    mov  a6, a1
    movi a3, _flint_trap
    callx4 a3                   // result (resume frame ptr) returned in a6

    mov  a1, a6                 // a1 = resume frame ptr

    // ── Restore, in an order the register windows permit ──────────────────
    //
    // Ordering here is not cosmetic. Writing WINDOWBASE re-maps which physical
    // registers the names a0-a15 refer to, so a1 stops being the frame pointer
    // the moment the write takes effect. The previous revision restored
    // WINDOWSTART/WINDOWBASE first and then continued issuing a1-relative
    // loads, which read from whatever the rotated a1 happened to contain --
    // every restore after the rotation was reading garbage.
    //
    // So: scalar special registers first, while the mapping is still the one
    // we entered with; then the window mapping; then the address registers.

    l32i a2, a1, 0x08
    wsr.sar a2
    l32i a2, a1, 0x0C
    wsr.lbeg a2
    l32i a2, a1, 0x10
    wsr.lend a2
    l32i a2, a1, 0x14
    wsr.lcount a2
    l32i a2, a1, 0x60
    wsr.scompare1 a2
    l32i a2, a1, 0x00
    wsr.epc1 a2
    rsync

    // WINDOWBASE goes back before any register is restored: it selects which
    // physical registers the names a0-a15 refer to, so the resuming task's
    // registers must be written into the window *it* was using. EXCSAVE1
    // carries the frame pointer across the rotation, since special registers
    // do not rotate.
    wsr.excsave1 a1

    // Both go back verbatim. The saved WINDOWSTART was read after the entry
    // spill, so it already says "only this window is live" -- every outer frame
    // is in memory, and the task's next `retw` takes an underflow and refills
    // from its own stack. Restoring the *pre-spill* value would be wrong: it
    // claims frames are in the register file that the handler has since
    // overwritten.
    l32i a2, a1, 0x58           // windowbase
    l32i a3, a1, 0x5C           // windowstart
    wsr.windowstart a3
    wsr.windowbase  a2
    rsync
    rsr.excsave1 a1             // frame pointer, in the restored window

    l32i a0,  a1, 0x18
    l32i a4,  a1, 0x28
    l32i a5,  a1, 0x2C
    l32i a6,  a1, 0x30
    l32i a7,  a1, 0x34
    l32i a8,  a1, 0x38
    l32i a9,  a1, 0x3C
    l32i a10, a1, 0x40
    l32i a11, a1, 0x44
    l32i a12, a1, 0x48
    l32i a13, a1, 0x4C
    l32i a14, a1, 0x50
    l32i a15, a1, 0x54

    // PS last, with EXCM forced set so interrupts stay masked across the final
    // loads; `rfe` clears EXCM as it returns.
    l32i a2, a1, 0x04
    movi a3, 0x10               // PS.EXCM
    or   a2, a2, a3
    wsr.ps a2
    rsync

    l32i a3,  a1, 0x24
    l32i a2,  a1, 0x20
    l32i a1,  a1, 0x1C          // SP last
    rfe

.extern _flint_trap

// ── Double exception (unrecoverable) ─────────────────────────────────────
// Emit "DBL\n" to UART0 (ROM-configured) so this silent loop is observable.
// FIFO is empty at fault time, so no fullness check is needed.
.align 4
.global _double_exception
_double_exception:
    // A double exception means a second exception hit while PS.EXCM was set --
    // most often a window overflow or underflow spilling through a bad base
    // register. "DBL" alone says nothing about where, so dump the registers
    // that identify it. Nothing here is recoverable, so every register is fair
    // game and no state is preserved.
    //
    // Format:  DBL <exccause> <epc1> <depc> <excvaddr>
    DBL_CH 68                   // 'D'
    DBL_CH 66                   // 'B'
    DBL_CH 76                   // 'L'
    DBL_CH 32
    rsr.exccause a7
    DBL_HEX a7
    DBL_CH 32
    rsr.epc1 a7
    DBL_HEX a7
    DBL_CH 32
    rsr.depc a7
    DBL_HEX a7
    DBL_CH 32
    rsr.excvaddr a7
    DBL_HEX a7
    DBL_CH 13
    DBL_CH 10
.Ldbl_hang:
    j .Ldbl_hang

// ── Unhandled higher-level interrupt / exception ─────────────────────────
// Emit "UNH\n" similarly.
.align 4
.global _flint_unhandled
_flint_unhandled:
    movi a2, 0x3FF40000
    movi a3, 0x55           // 'U'
    s32i a3, a2, 0
    movi a3, 0x4E           // 'N'
    s32i a3, a2, 0
    movi a3, 0x48           // 'H'
    s32i a3, a2, 0
    movi a3, 0x0A           // '\n'
    s32i a3, a2, 0
.Lunh_hang:
    j .Lunh_hang
