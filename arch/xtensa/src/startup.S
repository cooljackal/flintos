// SPDX-License-Identifier: Apache-2.0

// ESP32 Boot Startup
//
// Reached as the image entry point (ENTRY(_start) in the linker script) after
// the ROM/2nd-stage bootloader has loaded our segments into IRAM/DRAM, set up
// the SPI flash cache, and left us a usable initial SP.
//
// Responsibilities: relocate the vector table (VECBASE), disable the boot
// watchdogs, zero BSS, copy .data, set the kernel stack, jump to FlintMain.
//
// Registers at entry: a0 = ROM return (clobberable), a1 = initial SP.

.section .text.startup, "ax"
.global call_start_cpu0
.global _start
.type   call_start_cpu0, @function
.type   _start, @function

.literal_position

_start:
call_start_cpu0:
    // ── Mask interrupts during init ──────────────────────────────────────
    rsil a2, 15

    // ── Relocate the exception vector table to our table (W0.2) ──────────
    // The ROM leaves VECBASE pointing at ROM vectors; point it at ours so
    // the window overflow/underflow and interrupt vectors in vectors.S are
    // the ones the CPU dispatches through.
    movi a2, _vector_table_start
    wsr.vecbase a2
    rsync

    // ── Disable the boot watchdogs (W0.4) ───────────────────────────────
    // Each watchdog requires writing its unlock key to WPROTECT before the
    // config register accepts a write. Without this the chip resets in ~1s.

    // TIMG0 WDT: WPROTECT=0x3FF5F064 (key 0x50D83AA1), CONFIG0=0x3FF5F048
    movi a2, 0x3FF5F064
    movi a3, 0x50D83AA1
    s32i a3, a2, 0           // unlock TIMG0 WDT
    movi a2, 0x3FF5F048
    movi a3, 0
    s32i a3, a2, 0           // clear CONFIG0 (WDT_EN=0)

    // TIMG1 WDT: WPROTECT=0x3FF60064, CONFIG0=0x3FF60048
    movi a2, 0x3FF60064
    movi a3, 0x50D83AA1
    s32i a3, a2, 0           // unlock TIMG1 WDT
    movi a2, 0x3FF60048
    movi a3, 0
    s32i a3, a2, 0           // clear CONFIG0

    // RTC WDT: WPROTECT=0x3FF480A4 (key 0x50D83AA1), CONFIG0=0x3FF4808C
    movi a2, 0x3FF480A4
    movi a3, 0x50D83AA1
    s32i a3, a2, 0           // unlock RTC WDT
    movi a2, 0x3FF4808C
    movi a3, 0
    s32i a3, a2, 0           // clear RTC WDT CONFIG0

    // ── Zero the BSS section ─────────────────────────────────────────────
    // (.data is loaded into DRAM by the ESP-IDF bootloader; only BSS, which is
    //  NOLOAD, must be cleared here.)
    .extern _sbss
    .extern _ebss
    movi a3, _sbss
    movi a4, _ebss
    bgeu a3, a4, .L_bss_done
    movi a2, 0
.L_bss_loop:
    s32i a2, a3, 0
    addi a3, a3, 4
    bltu a3, a4, .L_bss_loop
.L_bss_done:

    // ── Establish the register-window state ──────────────────────────────
    //
    // Every Rust function built for this target begins with `entry`, which is
    // a windowed instruction and requires PS.WOE=1. Nothing here ever set PS
    // beyond the INTLEVEL field, so the entire kernel was relying on whatever
    // window state the ROM bootloader happened to leave behind. That is an
    // inherited invariant, not an established one, and it silently governs
    // whether any Rust code runs at all.
    //
    // Start from a clean single live window (WINDOWBASE=0, WINDOWSTART=1) so
    // the window exception handlers have an unambiguous starting point, then
    // enable WOE.
    movi a2, 0
    wsr.windowbase a2
    movi a2, 1
    wsr.windowstart a2
    rsync

    // PS = WOE | INTLEVEL(15); EXCM=0 so window exceptions can dispatch,
    // UM=0 so we run privileged, CALLINC=0 to match the call0 below.
    movi a2, 0x4000F
    wsr.ps a2
    rsync

    // ── Kernel/boot stack ────────────────────────────────────────────────
    //
    // Set AFTER the window rotation above, not before. Writing WINDOWBASE
    // re-maps which physical register the name `a1` refers to, so a stack
    // pointer loaded beforehand is simply left behind in the old window --
    // execution then continues on whatever stack the ROM bootloader was using
    // (observed on hardware as SP=0x3ffe3bb4, in ROM's SRAM1 area, rather than
    // _kernel_stack_end at 0x3ffb3710).
    .extern _kernel_stack_end
    movi a1, _kernel_stack_end

    // ── Enter the Rust kernel (interrupts still masked; FlintMain unmasks).
    .extern FlintMain
    call0 FlintMain

.L_halt:
    j .L_halt
