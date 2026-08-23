// SPDX-License-Identifier: Apache-2.0

use core::fmt::Write;
use api::debug::log::Level;

/// Log entry in the ring buffer.
struct LogEntry {
    tick: u64,
    level: Level,
    /// Who wrote the line: the running task's name straight from its TCB, or
    /// one of the placeholders from [`origin_name`]. A `&'static str` is two
    /// words (pointer + length) against the `u32` index it replaced, and it
    /// stays valid after the task is deleted, which the index did not -- a
    /// dumped ring used to look the slot up again and could name the wrong
    /// task, or `?`, once the id had been reused.
    task: &'static str,
    msg: [u8; 64],
    /// Valid bytes in `msg`.
    len: u8,
}

/// Name printed for a line with no scheduler to ask.
const NAME_UNKNOWN: &str = "?";
/// Name printed from trap context (a top-half ISR or a timer callback).
const NAME_ISR: &str = "isr";
/// Name printed before the first task has been switched in.
const NAME_BOOT: &str = "boot";

/// Resolve who is logging, given what the scheduler said (or `None` if it
/// could not be asked). Trap context wins: the task the scheduler names is
/// the one that was *interrupted*, not the one doing the logging.
fn origin_name(in_interrupt: bool, sched: Option<Option<&'static str>>) -> &'static str {
    if in_interrupt {
        return NAME_ISR;
    }
    match sched {
        None => NAME_UNKNOWN,
        Some(None) => NAME_BOOT,
        Some(Some("")) => NAME_UNKNOWN,
        Some(Some(name)) => name,
    }
}

/// The console line. One place, so the live path and [`dump`] cannot drift
/// and a host test can pin the format.
fn write_line(
    out: &mut impl Write,
    tick: u64,
    task: &str,
    level: Level,
    msg: &[u8],
) -> core::fmt::Result {
    write!(
        out,
        "[{:5}][{}] {} {}\r\n",
        tick,
        task,
        level_str(level),
        core::str::from_utf8(msg).unwrap_or("?"),
    )
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
    let sched = crate::scheduler::try_with(|s| (s.ticks(), s.current_name()));
    let tick = sched.map_or(0, |(t, _)| t);
    let task = origin_name(crate::interrupt::in_interrupt(), sched.map(|(_, n)| n));

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
    let _ = write_line(&mut console, tick, task, level, &buf[..len]);
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
                let _ = write_line(
                    &mut console,
                    entry.tick,
                    entry.task,
                    entry.level,
                    &entry.msg[..entry.len as usize],
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

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct S(std::string::String);
    impl Write for S {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            self.0.push_str(s);
            Ok(())
        }
    }

    fn line(tick: u64, task: &str, level: Level, msg: &[u8]) -> std::string::String {
        let mut out = S(std::string::String::new());
        write_line(&mut out, tick, task, level, msg).unwrap();
        out.0
    }

    #[test]
    fn line_carries_the_task_name_not_its_index() {
        assert_eq!(line(505, "sensor", Level::Info, b"n=2"), "[  505][sensor] INFO  n=2\r\n");
        assert_eq!(line(7, "isr", Level::Error, b"x"), "[    7][isr] ERROR x\r\n");
        assert_eq!(line(0, "boot", Level::Warn, b""), "[    0][boot] WARN  \r\n");
    }

    #[test]
    fn malformed_utf8_prints_a_placeholder_rather_than_failing() {
        assert_eq!(line(1, "t", Level::Debug, &[0xff, 0xfe]), "[    1][t] DEBUG ?\r\n");
    }

    #[test]
    fn origin_prefers_trap_context_then_scheduler_then_boot() {
        assert_eq!(origin_name(true, Some(Some("sensor"))), "isr");
        assert_eq!(origin_name(false, Some(Some("sensor"))), "sensor");
        assert_eq!(origin_name(false, Some(None)), "boot");
        assert_eq!(origin_name(false, Some(Some(""))), "?");
        assert_eq!(origin_name(false, None), "?");
    }

    #[test]
    fn entry_size_is_what_the_commit_message_claims() {
        // Two words of name replace one u32: 96 bytes on a 64-bit host, 88 on
        // a 32-bit target (was 80 on both). Fails loudly if a field is added
        // without re-measuring.
        let word = core::mem::size_of::<usize>();
        assert_eq!(core::mem::size_of::<LogEntry>(), if word == 8 { 96 } else { 88 });
        // The level enum's niche keeps `Option<LogEntry>` free.
        assert_eq!(core::mem::size_of::<Option<LogEntry>>(), core::mem::size_of::<LogEntry>());
    }
}
