/* SPDX-License-Identifier: Apache-2.0 */

/*
 * APP CPU entry trampoline.
 *
 * The second core arrives here with essentially nothing set up. It has no
 * stack pointer, no window state, and PS is whatever reset left. Rust cannot
 * run in that condition -- the first windowed call would spill registers
 * through an undefined `a1` -- so this establishes the minimum and then calls
 * in.
 *
 * What it does NOT do, deliberately:
 *
 *   - install a vector table. VECBASE is left alone, so any exception on this
 *     core goes wherever reset pointed it. The code it calls must not fault.
 *     Giving the APP CPU its own vectors means giving it its own trap handler
 *     and its own idea of "current task", which is the SMP work this is not.
 *   - enable interrupts. INTLEVEL is set to 15, masking everything. With no
 *     vector table an interrupt would be fatal, and nothing routes one here.
 *
 * Both are why `appcpu::start` documents the started core as being unable to
 * touch the kernel.
 */

/*
 * IRAM, not flash.
 *
 * The APP CPU comes out of reset with its instruction cache uninitialised, so
 * it cannot fetch anything mapped from flash. A trampoline in `.text` -- which
 * the linker script puts in irom -- gives an illegal-instruction exception on
 * the first fetch, on a core with no vector table to report it. That is
 * exactly what happened the first time.
 *
 * `.iram1` is what flint32.ld maps into iram_seg. `.literal_position` keeps
 * the `movi` literal pool here too, rather than in a `.literal` section that
 * would land back in flash.
 */
    .section .iram1.appcpu, "ax"
    .literal_position
    .align 4
    .global _flint_appcpu_entry
    .type _flint_appcpu_entry, @function

_flint_appcpu_entry:
    /*
     * Stack first. `_flint_appcpu_stack_top` is provided by Rust as the top of
     * a static array -- see appcpu_stack.rs. Nothing may touch memory through
     * a1 before this.
     */
    movi    a1, _flint_appcpu_stack_top
    l32i    a1, a1, 0

    /*
     * PS: WOE=1 so windowed calls work, INTLEVEL=15 so nothing interrupts,
     * EXCM=0, UM=0. 0x40000 is WOE (bit 18); 0xF is INTLEVEL.
     */
    movi    a2, 0x4000F
    wsr.ps  a2
    rsync

    /* A zero return address: this function never returns. */
    movi    a0, 0

    /*
     * `call4`, for the same reason `_start` uses it -- see the long note in
     * startup.S. `call0` would leave `flint_appcpu_main` running in this
     * window, and Rust emits `callx8`, which makes the bottom-most window
     * eight registers wide; WindowOverflow8 then dereferences `[a1 - 12]` for
     * a caller that does not exist. Four registers wide, and only
     * WindowOverflow4 can ever spill it.
     */
    call4   flint_appcpu_main

    /*
     * `flint_appcpu_main` is `-> !` and must not return. If it does anyway,
     * park rather than fall into whatever follows in .text.
     */
1:  waiti   0
    j       1b

    .size _flint_appcpu_entry, . - _flint_appcpu_entry
