// SPDX-License-Identifier: Apache-2.0

//! Panic handling and the postmortem snapshot.
//!
//! # Semantics: a panic halts the system
//!
//! Flint runs in a single protection domain. Every task shares one address
//! space with no MPU enforcement, so a task that panicked may already have
//! written through a bad pointer, left a mutex held, or corrupted a queue
//! another task is about to read. Killing just the offender and carrying on
//! would be a guess about how far the damage spread.
//!
//! So: mask every interrupt, record what happened, say so on the console, and
//! stop. The board stays stopped until it is reset — the watchdogs are disabled
//! during boot, so nothing resets it on its own.
//!
//! The previous behaviour was neither halt nor recover. It ended in a bare
//! `loop {}` with interrupts still unmasked, which parks only the panicking
//! task: the tick kept firing, the scheduler kept running every other task, and
//! the "FLINT PANIC" banner was printed by a system that then carried on as
//! though it had not.

use core::sync::atomic::{AtomicBool, Ordering};

use flint_arch_xtensa::registers;

use crate::scheduler::TaskState;

// Postmortem snapshot region (defined in linker script).
//
// `static mut` because we write it: the region is ours to own, and taking a
// `&` to a linker symbol only to cast it to a larger type is undefined
// behaviour -- the symbol's declared type is a `u32`, not the struct that
// actually lives there. Every access below goes through `addr_of_mut!`, which
// produces a raw pointer without forming a reference at all.
extern "C" {
    static mut _panic_region_start: u32;
}

/// Raw pointer to the snapshot region.
fn region() -> *mut PanicSnapshot {
    core::ptr::addr_of_mut!(_panic_region_start) as *mut PanicSnapshot
}

const PANIC_MAGIC: u32 = 0x464C_494E; // "FLIN"

const NAME_LEN: usize = 24;
const FILE_LEN: usize = 40;
const CAUSE_LEN: usize = 48;

/// Panic snapshot written to the reserved SRAM region, which survives a soft
/// reset so the next boot can report what killed the last one.
#[repr(C)]
struct PanicSnapshot {
    magic: u32,
    tick: u64,
    task_id: u32,
    task_name: [u8; NAME_LEN],
    /// `PS` at the moment of the panic. Its INTLEVEL field says whether this
    /// happened in task context or inside an interrupt handler, which changes
    /// what the rest of the snapshot means.
    ps: u32,
    /// Source line, or 0 when the caller supplied no location.
    line: u32,
    file: [u8; FILE_LEN],
    cause: [u8; CAUSE_LEN],
}

/// Set once `handle_at` has begun. A panic raised while the first is still
/// reporting — from a timer callback, or from the console driver itself — must
/// not overwrite the snapshot: the first one explains the failure, and the
/// second is usually just its consequence.
static PANICKING: AtomicBool = AtomicBool::new(false);

/// Copy `src` into a fixed-size field, truncating rather than failing.
fn fill(dst: &mut [u8], src: &str) {
    let bytes = src.as_bytes();
    let n = bytes.len().min(dst.len());
    dst[..n].copy_from_slice(&bytes[..n]);
}

/// Trim a fixed-size, NUL-padded field back to a string.
fn as_str(buf: &[u8]) -> &str {
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    core::str::from_utf8(&buf[..end]).unwrap_or("?")
}

/// The kernel panic handler. Never returns.
pub fn handle(args: &core::fmt::Arguments<'_>) -> ! {
    handle_at(args, None)
}

/// The kernel panic handler, with the source location where the caller has one.
///
/// `#[panic_handler]` passes `PanicInfo::location()` through here. Kernel code
/// that calls [`handle`] directly names itself in the message instead.
pub fn handle_at(
    args: &core::fmt::Arguments<'_>,
    location: Option<&core::panic::Location<'_>>,
) -> ! {
    use core::fmt::Write;

    // First, before any scheduler state is read and before the panic region is
    // touched. Everything below does both, and a tick landing in the middle
    // would interleave a context switch with a half-written snapshot.
    //
    // The returned PS is the one the panic happened under, which is worth
    // recording: its INTLEVEL distinguishes a panic in a task from one inside
    // an interrupt handler.
    let ps = unsafe { registers::set_intlevel_15() };

    // A nested panic gets the console line but not the snapshot: first wins.
    let first = !PANICKING.swap(true, Ordering::Relaxed);

    // Interrupts are masked, so the scheduler is ours; `scheduler::with` would
    // take a critical section we are already inside.
    let sched = crate::scheduler::global();
    let current = sched.current;
    let tick = sched.ticks();
    let task_name = sched.tasks[current as usize].as_ref().map_or("", |t| t.name);

    // Take the panicking task out of the run set. Nothing will schedule after
    // this -- interrupts stay masked forever -- but a TCB still claiming to be
    // Running would mislead anyone reading memory through a debugger.
    if let Some(tcb) = &mut sched.tasks[current as usize] {
        tcb.state = TaskState::Faulted;
    }
    sched.recompute_ready_mask();

    let mut msg = [0u8; CAUSE_LEN];
    let msg_len = {
        let mut w = crate::debug::log::BufWriter { buf: &mut msg, pos: 0 };
        let _ = write!(w, "{}", args);
        w.pos
    };

    if first {
        unsafe {
            let mut snapshot = PanicSnapshot {
                magic: PANIC_MAGIC,
                tick,
                task_id: current,
                task_name: [0; NAME_LEN],
                ps,
                line: location.map_or(0, |l| l.line()),
                file: [0; FILE_LEN],
                cause: msg,
            };
            fill(&mut snapshot.task_name, task_name);
            if let Some(loc) = location {
                fill(&mut snapshot.file, loc.file());
            }
            region().write_volatile(snapshot);
        }
    }

    // Console last. The console driver is the most likely thing to fault a
    // second time, and the snapshot is what survives if it does.
    let mut console = crate::debug::console::Console;
    let _ = write!(console, "\r\n╔══ FLINT PANIC ════════════════════╗\r\n");
    let _ = write!(console, "  Uptime: {} ms\r\n", tick);
    let _ = write!(console, "  Task:   {} (id {})\r\n", task_name, current);
    if let Some(loc) = location {
        let _ = write!(console, "  Where:  {}:{}\r\n", loc.file(), loc.line());
    }
    let _ = write!(
        console,
        "  Cause:  {}\r\n",
        core::str::from_utf8(&msg[..msg_len]).unwrap_or("?")
    );
    if !first {
        let _ = write!(console, "  (nested panic; the snapshot holds the first)\r\n");
    }
    let _ = write!(console, "  System halted. Reset to continue.\r\n");
    let _ = write!(console, "╚════════════════════════════════════╝\r\n");

    halt()
}

/// Park the CPU with every maskable interrupt masked.
///
/// `waiti 15` rather than a spin loop: there is nothing to poll for, and a
/// halted board that is not also drawing full power is a kinder thing to leave
/// on a bench while someone reads the console.
fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("waiti 15") };
    }
}

// ── Reading the snapshot back ───────────────────────────────────────────────

/// Whether a panic snapshot from a previous boot is present.
pub fn has_snapshot() -> bool {
    unsafe { core::ptr::read_volatile(core::ptr::addr_of!(_panic_region_start)) == PANIC_MAGIC }
}

/// Print the previous boot's panic, if there was one, then clear it.
///
/// Called once during boot, after the console is up. Without this the snapshot
/// was written and never read — the region existed, the magic was set, and
/// nothing in the tree ever looked. Clearing the magic is what makes this "the
/// previous boot" rather than a message that reappears on every boot until the
/// SRAM happens to be overwritten.
pub fn report_previous() {
    if !has_snapshot() {
        return;
    }

    // Copy it out rather than holding a reference into the region: the fields
    // are read several times below, and a snapshot that changed underneath a
    // half-printed report would be worse than no report.
    let snap = unsafe { region().read_volatile() };

    use core::fmt::Write;
    let mut console = crate::debug::console::Console;
    let _ = write!(console, "\r\n╔══ PREVIOUS BOOT PANICKED ═════════╗\r\n");
    let _ = write!(console, "  Uptime: {} ms\r\n", snap.tick);
    let _ = write!(
        console,
        "  Task:   {} (id {})\r\n",
        as_str(&snap.task_name),
        snap.task_id
    );
    if snap.line != 0 {
        let _ = write!(console, "  Where:  {}:{}\r\n", as_str(&snap.file), snap.line);
    }
    let _ = write!(console, "  Cause:  {}\r\n", as_str(&snap.cause));
    let _ = write!(console, "  PS:     {:#010x}\r\n", snap.ps);
    let _ = write!(console, "╚════════════════════════════════════╝\r\n");

    // Consume it, so the next boot reports only a fresh failure.
    unsafe { core::ptr::write_volatile(core::ptr::addr_of_mut!(_panic_region_start), 0) };
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_fits_the_reserved_region() {
        // panic_region is 4 KiB in flint32.ld. Nowhere near it, but a struct
        // that outgrew the region would corrupt whatever follows with no
        // diagnostic -- which is precisely the failure this region exists to
        // diagnose.
        assert!(core::mem::size_of::<PanicSnapshot>() <= 4096);
    }

    #[test]
    fn magic_spells_flin() {
        assert_eq!(PANIC_MAGIC.to_be_bytes(), *b"FLIN");
    }

    #[test]
    fn fields_truncate_rather_than_overflow() {
        let mut buf = [0u8; 8];
        fill(&mut buf, "a-name-far-longer-than-eight");
        assert_eq!(&buf, b"a-name-f");
    }

    #[test]
    fn as_str_stops_at_the_padding() {
        let mut buf = [0u8; 8];
        fill(&mut buf, "sensor");
        assert_eq!(as_str(&buf), "sensor");
    }

    #[test]
    fn as_str_handles_a_completely_full_field() {
        // No NUL to find: the string uses every byte.
        let mut buf = [0u8; 6];
        fill(&mut buf, "sensor");
        assert_eq!(as_str(&buf), "sensor");
    }
}
