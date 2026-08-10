// SPDX-License-Identifier: Apache-2.0

use core::fmt::Write;
use api::debug::log::Level;

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

/// The ring and its three indices, behind a lock that excludes the other core.
///
/// Four bare `static mut`s under `cs_with`, which masks the calling core only.
/// Both cores log, so two concurrent writers could advance `tail` to the same
/// slot and lose a line, or leave `count` disagreeing with `head`.
///
/// **Lock order: scheduler, then this.** `write` reads the tick through
/// `scheduler::try_with` *before* taking this lock, and `mutex::log_error`
/// logs while holding the scheduler — so the scheduler is always the outer
/// one. Nothing takes the scheduler while holding this.
struct LogRing {
    buf: [Option<LogEntry>; RING_BUF_SIZE],
    head: usize,
    count: usize,
    tail: usize,
}

static RING: crate::smp::Spinlock<LogRing> = crate::smp::Spinlock::new(LogRing {
    buf: [const { None }; RING_BUF_SIZE],
    head: 0,
    count: 0,
    tail: 0,
});

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

    // `try_with`, not `with`. Logging is reachable from inside the scheduler
    // lock -- `mutex::log_error` reports a full waiter list while holding it --
    // and taking the lock again is reentrancy, which now panics rather than
    // silently aliasing two `&mut` to the scheduler as it used to.
    //
    // A log line missing its tick and task is worth far more than a kernel that
    // deadlocks trying to stamp one.
    let (tick, task) = crate::scheduler::try_with(|s| (s.ticks(), s.current()))
        .unwrap_or((0, u32::MAX));

    // Store in the ring buffer, under the lock it shares with readers.
    RING.with(|r| {
        let tail = r.tail;
        r.buf[tail] = Some(LogEntry { tick, level, task, msg: buf, len: len as u8 });
        r.tail = (tail + 1) % RING_BUF_SIZE;
        if r.count < RING_BUF_SIZE {
            r.count += 1;
        } else {
            r.head = (r.head + 1) % RING_BUF_SIZE;
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
    RING.with(|r| {
        for i in 0..r.count {
            let idx = (r.head + i) % RING_BUF_SIZE;
            if let Some(entry) = &r.buf[idx] {
                // Same reasoning: draining the ring can be reached from a
                // context that already holds the scheduler.
                let task_name = crate::scheduler::try_with(|s| {
                    s.tasks
                        .get(entry.task as usize)
                        .and_then(|t| t.as_ref())
                        .map_or("?", |t| t.name)
                })
                .unwrap_or("?");
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
