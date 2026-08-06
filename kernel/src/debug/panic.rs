// SPDX-License-Identifier: Apache-2.0

// Postmortem snapshot region (defined in linker script).
extern "C" {
    static _panic_region_start: u32;
    static _panic_region_end: u32;
}

const PANIC_MAGIC: u32 = 0x464C_494E; // "FLIN"

/// Panic snapshot structure written to the reserved SRAM region.
#[repr(C)]
struct PanicSnapshot {
    magic: u32,
    tick: u64,
    task_id: u32,
    task_name: [u8; 24],
    pc: u32,
    ps: u32,
    cause: [u8; 48],
}

/// The kernel panic handler.
/// Called from the flint-api flint_panic! macro.
pub fn handle(args: &core::fmt::Arguments<'_>) -> ! {
    use core::fmt::Write;

    let sched = crate::scheduler::global();
    let current = sched.current;
    let task_tick = sched.ticks();
    let task_name = sched.tasks[current as usize]
        .as_ref()
        .map_or("", |t| t.name);

    let mut msg = [0u8; 48];
    let msg_len;
    {
        let mut w = crate::debug::log::BufWriter {
            buf: &mut msg,
            pos: 0,
        };
        let _ = write!(w, "{}", args);
        msg_len = w.pos;
    }

    // Write snapshot to panic SRAM region.
    unsafe {
        let region = &_panic_region_start as *const u32 as *mut PanicSnapshot;
        region.write(PanicSnapshot {
            magic: PANIC_MAGIC,
            tick: task_tick,
            task_id: current,
            task_name: {
                let mut name = [0u8; 24];
                let bytes = task_name.as_bytes();
                let n = bytes.len().min(24);
                name[..n].copy_from_slice(&bytes[..n]);
                name
            },
            pc: 0,
            ps: 0,
            cause: msg,
        });
    }

    // Dump postmortem to UART console.
    let mut console = crate::debug::console::Console;
    let _ = write!(console, "\r\n╔══ FLINT PANIC ════════════════════╗\r\n");
    let _ = write!(console, "  Uptime: {}ms\r\n", task_tick);
    let _ = write!(console, "  Task: {}\r\n", task_name);
    let _ = write!(console, "  Cause: {}\r\n", core::str::from_utf8(&msg[..msg_len]).unwrap_or("?"));
    let _ = write!(console, "╚════════════════════════════════════╝\r\n");

    loop {}
}

/// Check if a panic snapshot exists from a previous boot.
pub fn has_snapshot() -> bool {
    unsafe {
        let region = &_panic_region_start as *const u32 as *const PanicSnapshot;
        (*region).magic == PANIC_MAGIC
    }
}
