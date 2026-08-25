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
/// Tries until every byte is queued: the board hands out a non-blocking
/// [`ByteStream`](hal::stream::ByteStream) whose `write` takes what fits and
/// returns the count, so a log line longer than the FIFO can drain without
/// truncation. A permanently stalled console is bounded and drops the tail;
/// a board with no console drops the whole write.
pub fn console_write(data: &[u8]) {
    if let Some(console) = crate::board::console() {
        write_bounded(console, data, 1_000_000);
    }
}

fn write_bounded(console: &dyn hal::stream::ByteStream, data: &[u8], idle_limit: usize) -> usize {
    let mut written = 0;
    let mut idle_polls = 0;
    while written < data.len() {
        let n = console.write(&data[written..]);
        if n == 0 {
            idle_polls += 1;
            if idle_polls == idle_limit {
                break;
            }
            core::hint::spin_loop();
        } else {
            written += n;
            idle_polls = 0;
        }
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use hal::stream::{ByteStream, StreamErrors};
    use std::sync::Mutex;

    struct FakeStream {
        writes: Mutex<Vec<u8>>,
        chunk: usize,
    }

    impl ByteStream for FakeStream {
        fn write(&self, data: &[u8]) -> usize {
            let n = data.len().min(self.chunk);
            self.writes.lock().unwrap().extend_from_slice(&data[..n]);
            n
        }

        fn read(&self, _buf: &mut [u8]) -> usize {
            0
        }

        fn errors(&self) -> StreamErrors {
            StreamErrors::default()
        }
    }

    #[test]
    fn bounded_write_finishes_across_partial_writes() {
        let console = FakeStream {
            writes: Mutex::new(Vec::new()),
            chunk: 2,
        };

        assert_eq!(write_bounded(&console, b"flint", 3), 5);
        assert_eq!(&*console.writes.lock().unwrap(), b"flint");
    }

    #[test]
    fn bounded_write_drops_after_idle_limit() {
        let console = FakeStream {
            writes: Mutex::new(Vec::new()),
            chunk: 0,
        };

        assert_eq!(write_bounded(&console, b"blocked", 3), 0);
        assert!(console.writes.lock().unwrap().is_empty());
    }
}
