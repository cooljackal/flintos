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
pub fn raw_uart_fault(tag: &str, cause: u32, epc: u32, ps: u32, vaddr: u32) -> ! {
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
        raw_puts("\r\n");
    }
    loop {
        crate::arch::wait_for_interrupt();
    }
}
