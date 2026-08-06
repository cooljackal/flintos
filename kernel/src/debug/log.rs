// SPDX-License-Identifier: Apache-2.0

use core::fmt::Write;
use flint_api::debug::log::Level;

/// Log entry in the ring buffer.
struct LogEntry {
    tick: u64,
    level: Level,
    task: u32,
    msg: [u8; 64],
    /// Valid bytes in `msg`.
    len: u8,
}

/// Kernel log ring buffer.
const RING_BUF_SIZE: usize = 32;

static mut RING_BUF: [Option<LogEntry>; RING_BUF_SIZE] = [const { None }; RING_BUF_SIZE];
static mut RING_HEAD: usize = 0;
static mut RING_COUNT: usize = 0;
static mut RING_TAIL: usize = 0;

/// Write a log message.  Called from the syscall entry.
pub fn write(level: Level, args: &core::fmt::Arguments<'_>) {
    // Format the message into a byte buffer, tracking how many bytes were
    // actually written so we never decode trailing NUL padding (plan W7.2).
    let mut buf = [0u8; 64];
    let len;
    {
        let mut writer = BufWriter { buf: &mut buf, pos: 0 };
        let _ = write!(writer, "{}", args);
        len = writer.pos;
    }

    let (tick, task) = crate::scheduler::with(|s| (s.ticks(), s.current));

    // Store in the ring buffer (under a critical section — shared with readers).
    flint_arch_xtensa::cs_with(|| unsafe {
        let ring = &mut *core::ptr::addr_of_mut!(RING_BUF);
        ring[RING_TAIL] = Some(LogEntry { tick, level, task, msg: buf, len: len as u8 });
        RING_TAIL = (RING_TAIL + 1) % RING_BUF_SIZE;
        if RING_COUNT < RING_BUF_SIZE {
            RING_COUNT += 1;
        } else {
            RING_HEAD = (RING_HEAD + 1) % RING_BUF_SIZE;
        }
    });

    // Also output to the UART console.
    let mut console = crate::debug::console::Console;
    let _ = write!(console, "[{:5}][task:{}] {} {}\r\n",
        tick,
        task,
        level_str(level),
        core::str::from_utf8(&buf[..len]).unwrap_or("?"),
    );
}

fn level_str(level: Level) -> &'static str {
    match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN ",
        Level::Info => "INFO ",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    }
}

/// Dump the ring buffer to the console.
///
/// Nothing calls this yet -- it is waiting on a shell -- but it is the only
/// read path out of `RING_BUF`, which every log line writes to. Delete it and
/// the ring buffer becomes write-only storage that costs RAM and returns
/// nothing, so the honest cleanup would be to delete the ring as well. It earns
/// its place the first time a panic handler or a `dmesg` command needs the last
/// N lines that did *not* make it out of the UART.
pub fn dump() {
    let mut console = crate::debug::console::Console;
    flint_arch_xtensa::cs_with(|| unsafe {
        let ring = &*core::ptr::addr_of!(RING_BUF);
        for i in 0..RING_COUNT {
            let idx = (RING_HEAD + i) % RING_BUF_SIZE;
            if let Some(entry) = &ring[idx] {
                let task_name = crate::scheduler::global().tasks[entry.task as usize]
                    .as_ref()
                    .map_or("?", |t| t.name);
                let text = core::str::from_utf8(&entry.msg[..entry.len as usize]).unwrap_or("?");
                let _ = write!(
                    console,
                    "[{:5}][{}] {} {}\r\n",
                    entry.tick,
                    task_name,
                    level_str(entry.level),
                    text
                );
            }
        }
    });
}

/// Helper: write a formatted byte string.
pub struct BufWriter<'a> {
    pub buf: &'a mut [u8],
    pub pos: usize,
}

impl Write for BufWriter<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let bytes = s.as_bytes();
        let remaining = self.buf.len().saturating_sub(self.pos);
        let n = bytes.len().min(remaining);
        self.buf[self.pos..self.pos + n].copy_from_slice(&bytes[..n]);
        self.pos += n;
        Ok(())
    }
}
