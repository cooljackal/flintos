// SPDX-License-Identifier: Apache-2.0

//! Raw UART0 fault reporter (bring-up diagnostics).
//!
//! Writes directly to the UART0 TX FIFO that the ROM/bootloader already
//! configured (115200 8N1), bypassing the `Console`/`CONSOLE_UART` path. This
//! lets us report faults and boot markers even before — or instead of — our own
//! UART init, so an early-boot crash is no longer a silent hang.

const UART0_FIFO: *mut u32 = 0x3FF4_0000 as *mut u32;
const UART0_STATUS: *const u32 = 0x3FF4_001C as *const u32;

#[inline]
unsafe fn raw_putc(b: u8) {
    // Wait until the TX FIFO has room (count is bits 16..23 of STATUS).
    while ((core::ptr::read_volatile(UART0_STATUS) >> 16) & 0xFF) >= 120 {}
    core::ptr::write_volatile(UART0_FIFO, b as u32);
}

unsafe fn raw_puts(s: &str) {
    for &b in s.as_bytes() {
        raw_putc(b);
    }
}

unsafe fn raw_hex_inner(v: u32) {
    raw_puts("0x");
    for i in (0..8).rev() {
        let nib = ((v >> (i * 4)) & 0xF) as u8;
        raw_putc(if nib < 10 { b'0' + nib } else { b'a' + nib - 10 });
    }
}

unsafe fn raw_dec_inner(v: u32) {
    if v == 0 {
        raw_putc(b'0');
        return;
    }
    // Max u32 is 10 decimal digits (4294967295).
    let mut digits = [0u8; 10];
    let mut n = v;
    let mut i = 0;
    while n > 0 {
        digits[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }
    while i > 0 {
        i -= 1;
        raw_putc(digits[i]);
    }
}

/// Print a raw marker string to UART0 (boot-progress bisection).
pub fn raw_print(s: &str) {
    unsafe { raw_puts(s) }
}

/// Print `v` as `0xNNNNNNNN` (8 hex digits, no allocation, no `core::fmt`) to
/// UART0. For boot diagnostics printed before the logging subsystem exists.
pub fn raw_hex(v: u32) {
    unsafe { raw_hex_inner(v) }
}

/// Print `v` in decimal (no allocation, no `core::fmt`) to UART0. For boot
/// diagnostics printed before the logging subsystem exists.
pub fn raw_dec(v: u32) {
    unsafe { raw_dec_inner(v) }
}

/// Report a CPU exception over raw UART0 and halt. Called from the trap handler
/// when a non-interrupt exception reaches it.
///
/// `a0` and `a1` are the faulting window's return address and stack pointer.
/// They matter more than they look: `epc` alone is useless the moment the fault
/// is a jump through a null or garbage pointer, because the address it names is
/// not code and `addr2line` has nothing to say about it. `a0` is the call site,
/// which is the question actually being asked — "who called this?".
///
/// This was added the first time `epc=0x00000036` appeared, which named an
/// address in nobody's function and left a blob-sized haystack.
pub fn raw_uart_fault(
    tag: &str,
    cause: u32,
    epc: u32,
    ps: u32,
    vaddr: u32,
    a0: u32,
    a1: u32,
) -> ! {
    unsafe {
        raw_puts("\r\n[FLINT FAULT] ");
        raw_puts(tag);
        raw_puts(" cause=");
        raw_hex(cause);
        raw_puts(" epc=");
        raw_hex(epc);
        raw_puts(" ps=");
        raw_hex(ps);
        raw_puts(" vaddr=");
        raw_hex(vaddr);
        // `a0` carries the window increment in its top two bits on a windowed
        // return; the address is the bottom 30, ORed with the caller's region.
        // Printed raw rather than decoded, because decoding it needs the
        // caller's PC, which is what we are trying to find.
        raw_puts("\r\n[FLINT FAULT] a0=");
        raw_hex(a0);
        raw_puts(" sp=");
        raw_hex(a1);
        raw_puts(" task=");
        raw_dec(crate::dynobj::current_task());
        raw_puts("\r\n");
    }
    loop {
        crate::arch::wait_for_interrupt();
    }
}
