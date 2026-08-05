// SPDX-License-Identifier: Apache-2.0

// Xtensa LX6 Relocatable Exception Vector Table (ESP32)
//
// ┌──────────────────────────────────────────────────────────────────────┐
// │ HARDWARE-UNVERIFIED. Vector offsets follow the ESP32 relocatable       │
// │ layout (VECBASE-relative). The window overflow/underflow handlers use  │
// │ the canonical Xtensa spill/fill sequences. The level-1 trap entry and  │
// │ its call into the Rust handler need on-target bring-up (gates G0/G1).  │
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

.section .vectors, "ax"
.global _vector_table_start
.global _vector_table_end

.align 1024
_vector_table_start:

// ── 0x000  Window Overflow 4 ──────────────────────────────────────────────
.org 0x000
_WindowOverflow4:
    s32e    a0, a5, -16
    s32e    a1, a5, -12
    s32e    a2, a5, -8
    s32e    a3, a5, -4
    rfwo

// ── 0x040  Window Underflow 4 ─────────────────────────────────────────────
.org 0x040
_WindowUnderflow4:
    l32e    a0, a5, -16
    l32e    a1, a5, -12
    l32e    a2, a5, -8
    l32e    a3, a5, -4
    rfwu

// ── 0x080  Window Overflow 8 ──────────────────────────────────────────────
.org 0x080
_WindowOverflow8:
    s32e    a0, a9, -16
    s32e    a1, a9, -12
    s32e    a2, a9, -8
    s32e    a3, a9, -4
    s32e    a4, a1, -32
    s32e    a5, a1, -28
    s32e    a6, a1, -24
    s32e    a7, a1, -20
    rfwo

// ── 0x0C0  Window Underflow 8 ─────────────────────────────────────────────
.org 0x0C0
_WindowUnderflow8:
    l32e    a0, a9, -16
    l32e    a1, a9, -12
    l32e    a2, a9, -8
    l32e    a7, a1, -20
    l32e    a3, a9, -4
    l32e    a4, a1, -32
    l32e    a5, a1, -28
    l32e    a6, a1, -24
    rfwu

// ── 0x100  Window Overflow 12 ─────────────────────────────────────────────
.org 0x100
_WindowOverflow12:
    s32e    a0,  a13, -16
    s32e    a1,  a13, -12
    s32e    a2,  a13, -8
    s32e    a3,  a13, -4
    s32e    a4,  a1, -48
    s32e    a5,  a1, -44
    s32e    a6,  a1, -40
    s32e    a7,  a1, -36
    s32e    a8,  a9, -32
    s32e    a9,  a9, -28
    s32e    a10, a9, -24
    s32e    a11, a9, -20
    rfwo

// ── 0x140  Window Underflow 12 ────────────────────────────────────────────
.org 0x140
_WindowUnderflow12:
    l32e    a0,  a13, -16
    l32e    a1,  a13, -12
    l32e    a2,  a13, -8
    l32e    a11, a13, -20
    l32e    a3,  a13, -4
    l32e    a4,  a1, -48
    l32e    a5,  a1, -44
    l32e    a6,  a1, -40
    l32e    a7,  a1, -36
    l32e    a8,  a9, -32
    l32e    a9,  a9, -28
    l32e    a10, a9, -24
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

// ── Whole-register-file save/restore ─────────────────────────────────────
//
// A task's outer call frames live in the physical register file, not on its
// stack, until a window overflow evicts them. The trap handler is itself
// windowed Rust, so merely servicing an interrupt rotates the window and
// spills some of the interrupted task's frames -- clearing their WINDOWSTART
// bits while their physical registers are reused. Restoring only the current
// window plus the original WINDOWSTART then claims those frames are still in
// registers when they are not, and the task resumes with a corrupted call
// chain. Observed on hardware as a task truncating mid-log-line.
//
// Rather than reproduce the canonical spill sequence, which depends on subtle
// window-exception behaviour, these macros capture the entire 64-register file
// and put it back verbatim. Whatever the handler did to the windows in between
// is then unobservable.
//
// Only the current 16 registers are addressable at a time, so the file is
// walked with `rotw 4`, four rotations returning to the starting WINDOWBASE.
// Every logical register in a freshly rotated window is one that must be
// preserved, so there is no free base register: EXCSAVE1 carries the frame
// pointer across rotations (special registers do not rotate) and EXCSAVE2
// briefly holds the one register displaced to make room for it.

.macro FLINT_SAVE_WINDOW off
    rotw 4
    wsr.excsave2 a0             // park a0; it becomes the base register
    rsr.excsave1 a0             // a0 = frame pointer
    s32i a1,  a0, \off + 0x04
    s32i a2,  a0, \off + 0x08
    s32i a3,  a0, \off + 0x0C
    s32i a4,  a0, \off + 0x10
    s32i a5,  a0, \off + 0x14
    s32i a6,  a0, \off + 0x18
    s32i a7,  a0, \off + 0x1C
    s32i a8,  a0, \off + 0x20
    s32i a9,  a0, \off + 0x24
    s32i a10, a0, \off + 0x28
    s32i a11, a0, \off + 0x2C
    s32i a12, a0, \off + 0x30
    s32i a13, a0, \off + 0x34
    s32i a14, a0, \off + 0x38
    s32i a15, a0, \off + 0x3C
    rsr.excsave2 a1             // a1 already saved above, so reuse as scratch
    s32i a1,  a0, \off + 0x00   // ...to store the real a0
.endm

.macro FLINT_RESTORE_WINDOW off
    rotw 4
    rsr.excsave1 a0             // a0 = frame pointer; a0 itself is loaded last
    l32i a1,  a0, \off + 0x04
    l32i a2,  a0, \off + 0x08
    l32i a3,  a0, \off + 0x0C
    l32i a4,  a0, \off + 0x10
    l32i a5,  a0, \off + 0x14
    l32i a6,  a0, \off + 0x18
    l32i a7,  a0, \off + 0x1C
    l32i a8,  a0, \off + 0x20
    l32i a9,  a0, \off + 0x24
    l32i a10, a0, \off + 0x28
    l32i a11, a0, \off + 0x2C
    l32i a12, a0, \off + 0x30
    l32i a13, a0, \off + 0x34
    l32i a14, a0, \off + 0x38
    l32i a15, a0, \off + 0x3C
    l32i a0,  a0, \off + 0x00   // base register consumed by its own load
.endm

.align 4
.global _flint_trap_entry
.type   _flint_trap_entry, @function
_flint_trap_entry:
    // 288-byte frame: 96 bytes of specials plus the current window, then 192
    // for the other 48 registers. Must match size_of::<TaskContext>(), which
    // is asserted in flint-hal/src/types.rs. `addi` only reaches -128, so the
    // adjustment is split.
    addmi a1, a1, -256
    addi  a1, a1, -32

    // a2 is the only scratch we have before the frame is populated, so the
    // interrupted a2 must go into its slot BEFORE anything else uses it.
    //
    // The previous revision started with `rsr.epc1 a2`, destroying a2 on the
    // very first instruction, and later stored the computed stack pointer into
    // the a[2] slot. Every task resumed from any interrupt therefore came back
    // with a2 holding its own SP -- and a2 is the first argument and the return
    // value register in the Xtensa windowed ABI.
    s32i a2,  a1, 0x20          // a[2], real value

    // Save special registers.
    rsr.epc1   a2
    s32i a2,  a1, 0x00          // pc
    rsr.ps     a2
    s32i a2,  a1, 0x04          // ps
    rsr.sar    a2
    s32i a2,  a1, 0x08
    rsr.lbeg   a2
    s32i a2,  a1, 0x0C
    rsr.lend   a2
    s32i a2,  a1, 0x10
    rsr.lcount a2
    s32i a2,  a1, 0x14

    // Save current-window ARs. a1 here is the frame base; the interrupted
    // SP was frame_base + 96.
    s32i a0,  a1, 0x18
    addmi a2, a1, 256
    addi  a2, a2, 32            // a2 = a1 + 288, the interrupted SP
    s32i a2,  a1, 0x1C          // a[1] = interrupted SP
    // a[2] at 0x20 already holds the interrupted a2, saved on entry above.
    // Do not overwrite it with the computed SP.
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
    s32i a2, a1, 0x5C

    // Capture the other 48 registers. WINDOWBASE is recorded above first,
    // because `rotw` changes it; four rotations bring it back around, so after
    // the last one a1 is once again the frame pointer.
    wsr.excsave1 a1
    FLINT_SAVE_WINDOW 0x60
    FLINT_SAVE_WINDOW 0xA0
    FLINT_SAVE_WINDOW 0xE0
    rotw 4

    // ── Leave exception mode before calling into Rust ─────────────────────
    //
    // Hardware sets PS.EXCM on exception entry, and while EXCM is set a window
    // overflow cannot be serviced normally -- it escalates directly to the
    // double exception vector. `_flint_trap` is a windowed function, so the
    // `callx4` below overflows a window as soon as the handler nests a call.
    //
    // This is not hypothetical: on the first ever interrupt taken on real
    // hardware, the board printed "DBL" from the double exception vector at
    // exactly this point.
    //
    // Clear EXCM so window exceptions dispatch through their normal vectors,
    // and raise INTLEVEL to 3 (the ESP32's EXCM level) so no further interrupt
    // can nest while this one is being serviced. Everything the exception left
    // in EPC1 and EXCCAUSE has already been copied into the frame above, so it
    // is safe for a window exception to overwrite those now.
    movi a2, 0x40003            // PS: WOE=1, INTLEVEL=3, EXCM=0, UM=0
    wsr.ps a2
    rsync

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
    l32i a2, a1, 0x00
    wsr.epc1 a2
    rsync

    // WINDOWBASE goes back before any register is restored, not after. It
    // selects which physical registers the names a0-a15 refer to, so the
    // resuming task's registers have to be written into the window *it* was
    // using -- which is generally not the one the trap arrived on.
    //
    // EXCSAVE1 carries the frame pointer across the rotation, since special
    // registers do not rotate.
    wsr.excsave1 a1
    l32i a2, a1, 0x58
    wsr.windowbase a2
    rsync

    // The other 48 registers, in the same order they were saved. The final
    // rotation brings WINDOWBASE back to the resuming task's own window.
    FLINT_RESTORE_WINDOW 0x60
    FLINT_RESTORE_WINDOW 0xA0
    FLINT_RESTORE_WINDOW 0xE0
    rotw 4

    rsr.excsave1 a0             // a0 = frame pointer until the very last load

    // WINDOWSTART after WINDOWBASE. Unlike WINDOWBASE this does not remap any
    // register, so it is safe to write with the frame pointer still live.
    l32i a1, a0, 0x5C
    wsr.windowstart a1

    // PS with EXCM forced set. PS.EXCM masks interrupts, keeping the remaining
    // loads atomic with respect to the tick; `rfe` clears EXCM as it returns.
    // Writing the task's PS verbatim here would re-enable interrupts mid-
    // restore, letting a tick land on a half-restored register file.
    l32i a1, a0, 0x04
    movi a2, 0x10               // PS.EXCM
    or   a1, a1, a2
    wsr.ps a1
    rsync

    // Now the current window's registers. a0 stays the base until it loads
    // itself; a1 (the stack pointer) goes second to last.
    l32i a2,  a0, 0x20
    l32i a3,  a0, 0x24
    l32i a4,  a0, 0x28
    l32i a5,  a0, 0x2C
    l32i a6,  a0, 0x30
    l32i a7,  a0, 0x34
    l32i a8,  a0, 0x38
    l32i a9,  a0, 0x3C
    l32i a10, a0, 0x40
    l32i a11, a0, 0x44
    l32i a12, a0, 0x48
    l32i a13, a0, 0x4C
    l32i a14, a0, 0x50
    l32i a15, a0, 0x54
    l32i a1,  a0, 0x1C          // SP
    l32i a0,  a0, 0x18          // base consumed by its own load
    rfe

.extern _flint_trap

// ── Double exception (unrecoverable) ─────────────────────────────────────
// Emit "DBL\n" to UART0 (ROM-configured) so this silent loop is observable.
// FIFO is empty at fault time, so no fullness check is needed.
.align 4
.global _double_exception
_double_exception:
    movi a2, 0x3FF40000     // UART0 TX FIFO
    movi a3, 0x44           // 'D'
    s32i a3, a2, 0
    movi a3, 0x42           // 'B'
    s32i a3, a2, 0
    movi a3, 0x4C           // 'L'
    s32i a3, a2, 0
    movi a3, 0x0A           // '\n'
    s32i a3, a2, 0
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
