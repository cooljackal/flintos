// SPDX-License-Identifier: Apache-2.0

//! Boot startup — board initialisation, driver setup.
//!
//! Called from FlintMain() before the scheduler starts.
//!
//! The console is a board-owned device, not a kernel one. Each board brings up
//! its own console (`board::console_init`) and hands the kernel a
//! [`ByteStream`](hal::stream::ByteStream) to write to (`board::console`); the
//! kernel calls both blind, so it names no UART driver and the seam is the same
//! on every arch. The one board-owned device that lived in the kernel — a
//! `static mut CONSOLE_UART: Esp32Uart` behind `cfg(soc-esp32)` — is gone.

/// Initialise board-level hardware.
///
/// Must be called before the scheduler starts, and — because log and panic go
/// through the console — the console is the very first thing brought up, before
/// any line is written.
pub fn init() {
    // Bring the console up first. `console_init` returns whether it came up at
    // the board's configured framing; a `false` on a board that has a console
    // means the port rejected the config and fell back to the bootloader's
    // settings (usually 115200 8N1, so still readable) — say so via the raw
    // path, which does not depend on the console being configured.
    if !crate::board::console_init() {
        crate::debug::fault::raw_print(
            "[FLINT] WARNING: the console rejected the board config; \
             it is running at the bootloader's settings\r\n",
        );
    }
    console_write(b"FlintOS booting...\r\n");
}

/// Write bytes to the board's console, if it has one.
///
/// Loops until every byte is queued: the board hands out a non-blocking
/// [`ByteStream`](hal::stream::ByteStream) whose `write` takes what fits and
/// returns the count, so a log line longer than the FIFO spins here until it
/// drains rather than being truncated. A board with no console drops the bytes.
pub fn console_write(data: &[u8]) {
    if let Some(console) = crate::board::console() {
        let mut written = 0;
        while written < data.len() {
            let n = console.write(&data[written..]);
            if n == 0 {
                core::hint::spin_loop();
            } else {
                written += n;
            }
        }
    }
}
