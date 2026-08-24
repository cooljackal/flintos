// SPDX-License-Identifier: Apache-2.0

//! Runs the scheduler on both ESP32 cores and shows where each task ran.
//!
//! Three things have to be true for #20, and each has a task here:
//!
//! - a task pinned to core 1 runs **only** on core 1
//! - a task pinned to core 0 runs **only** on core 0
//! - an unpinned task can run on **either**
//!
//! Each task records which core it observed itself on, every iteration. That is
//! the only evidence that means anything: a pinned task that happens never to
//! be preempted proves nothing, so they all sleep, which forces a switch.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use api::prelude::*;
use kernel::smp::Spinlock;

kernel::flint_app!(main, abi = 2);

/// Per-task, per-core sighting counts. `seen[task][core]`.
static SEEN: Spinlock<[[u32; 2]; 3]> = Spinlock::new([[0; 2]; 3]);

const PINNED_0: usize = 0;
const PINNED_1: usize = 1;
const FLOATING: usize = 2;

/// Set once the second core has joined, so the report waits for it.
static SECOND_CORE_UP: AtomicU32 = AtomicU32::new(0);

/// Times a core set its own DPORT clock bit and then found it already gone.
///
/// Only the other core can have cleared it, and only by writing back a value
/// it read before our write landed. That is the lost update issue #56 is
/// about, and this counter is direct evidence of one. It must stay at zero.
static DPORT_LOST: AtomicU32 = AtomicU32::new(0);
/// Read-modify-writes each core completed, so a zero above means "hammered
/// and survived" rather than "never ran".
static DPORT_OPS: [AtomicU32; 2] = [AtomicU32::new(0), AtomicU32::new(0)];

/// Two peripherals neither the console nor any driver in this image uses.
/// Toggling UART0's would take the console out, which is a memorable way to
/// learn that these have to be unused.
const BIT_CORE0: soc_esp32::dport::ClockBit = soc_esp32::dport::ClockBit::UART1;
const BIT_CORE1: soc_esp32::dport::ClockBit = soc_esp32::dport::ClockBit::UART2;

fn main() {
    // The second core, brought into the scheduler rather than given a private
    // loop. From here it takes traps, ticks and runs tasks like core 0.
    unsafe {
        arch_xtensa::appcpu::prepare(second_core);
        soc_esp32::appcpu::start(arch_xtensa::appcpu::_flint_appcpu_entry);
    }

    // `appcpu::start` only *releases* the second core; it joins the scheduler
    // asynchronously, some microseconds later. A task pinned to core 1 cannot be
    // spawned until that core has marked itself joined, so wait for it here
    // instead of racing the spawn below (which otherwise panics on a fast boot).
    // `main` runs with interrupts masked, so this spins rather than sleeps;
    // core 1 makes progress on its own hardware regardless. Bounded, so a core
    // that never joins fails at the spawn below rather than hanging here.
    for _ in 0..20_000_000u32 {
        if kernel::smp::is_pinnable(1) {
            break;
        }
        core::hint::spin_loop();
    }

    Task::new("dport0", dport_core0)
        .priority(Priority::Background(0))
        .on_core(0)
        .spawn()
        .expect("spawn dport0");
    Task::new("dport1", dport_core1)
        .priority(Priority::Background(0))
        .on_core(1)
        .spawn()
        .expect("spawn dport1");

    Task::new("pin0", pinned_0)
        .priority(Priority::Normal(2))
        .on_core(0)
        .spawn()
        .expect("spawn pin0");
    Task::new("pin1", pinned_1)
        .priority(Priority::Normal(2))
        .on_core(1)
        .spawn()
        .expect("spawn pin1");
    Task::new("float", floating)
        .priority(Priority::Normal(3))
        .spawn()
        .expect("spawn float");
    Task::new("report", report)
        .priority(Priority::Normal(1))
        .spawn()
        .expect("spawn report");
}

/// The second core's entry. Never returns — it becomes that core's idle task.
///
/// In IRAM: this runs before the core reaches anything in flash, and the
/// instruction cache is enabled by `appcpu::start` from the other core.
#[link_section = ".iram1.second_core"]
extern "C" fn second_core() -> ! {
    SECOND_CORE_UP.store(1, Ordering::SeqCst);
    unsafe { kernel::boot::join_scheduler() }
}

/// Hammer DPORT from core 0: set our bit, then check it survived.
///
/// The check is the test. Core 1 is doing its own read-modify-writes on the
/// same register at the same time, and if either implementation reads a stale
/// value it writes our bit away — which we see on the very next read.
///
/// **No yield in the loop, and Background priority.** The first version
/// yielded every iteration and detected nothing: a lost update needs both
/// cores inside the few-instruction read-write window at once, and a task that
/// spends most of its time in the scheduler is almost never there. Verified by
/// deleting the lock and confirming this reports losses — a concurrency test
/// that has never been seen to fail is not evidence of anything.
fn dport_core0() {
    hammer(BIT_CORE0, 0)
}

fn dport_core1() {
    hammer(BIT_CORE1, 1)
}

/// Clear, set, then check the bit is still set.
///
/// The clear is what makes this detect anything. The first version only ever
/// *set* its bit, so the bit was set in every read either core could take —
/// a stale value still had it set, and there was nothing to lose. Clearing
/// first opens a window where the other core can read the bit clear and write
/// that back after we set it, which is exactly a lost update.
fn hammer(bit: soc_esp32::dport::ClockBit, core: usize) -> ! {
    loop {
        unsafe {
            soc_esp32::dport::disable(bit);
            soc_esp32::dport::enable(bit);
            if soc_esp32::dport::read(soc_esp32::dport::PERIP_CLK_EN) & bit.mask() == 0 {
                DPORT_LOST.fetch_add(1, Ordering::Relaxed);
            }
        }
        DPORT_OPS[core].fetch_add(1, Ordering::Relaxed);
    }
}

fn record(which: usize) {
    let core = kernel::smp::current_core().0 as usize;
    SEEN.with(|s| s[which][core] += 1);
}

fn pinned_0() {
    loop {
        record(PINNED_0);
        sleep_ms(7);
    }
}

fn pinned_1() {
    loop {
        record(PINNED_1);
        sleep_ms(7);
    }
}

fn floating() {
    loop {
        record(FLOATING);
        sleep_ms(5);
    }
}

/// Report what actually happened, and judge it.
fn report() {
    sleep_ms(300);
    log_info!("second core up: {}", SECOND_CORE_UP.load(Ordering::SeqCst) == 1);

    for round in 1..=6u32 {
        sleep_ms(1000);
        let seen = SEEN.with(|s| *s);

        for (name, i) in [("pin0", PINNED_0), ("pin1", PINNED_1), ("float", FLOATING)] {
            log_info!("{name} round {round}: core0 {} core1 {}", seen[i][0], seen[i][1]);
        }

        // A pinned task on the wrong core is the failure this whole change is
        // about. Report it as an error rather than leaving it to be read out
        // of the numbers.
        if seen[PINNED_0][1] != 0 {
            log_error!("pin0 ran on core 1 {} times", seen[PINNED_0][1]);
        }
        if seen[PINNED_1][0] != 0 {
            log_error!("pin1 ran on core 0 {} times", seen[PINNED_1][0]);
        }
        // And a pinned task that never ran at all is just as wrong, quietly.
        if seen[PINNED_1][1] == 0 {
            log_error!("pin1 never ran on core 1");
        }
        if seen[FLOATING][0] != 0 && seen[FLOATING][1] != 0 {
            log_info!("float ran on both cores");
        }

        // Issue #56: the same DPORT register, read-modify-written from both
        // cores at once.
        let (ops0, ops1) = (
            DPORT_OPS[0].load(Ordering::Relaxed),
            DPORT_OPS[1].load(Ordering::Relaxed),
        );
        let lost = DPORT_LOST.load(Ordering::Relaxed);
        log_info!("dport round {round}: core0 {ops0} ops, core1 {ops1} ops, lost {lost}");
        if lost != 0 {
            log_error!("dport lost {lost} updates — the lock is not holding");
        }
        if ops0 == 0 || ops1 == 0 {
            log_error!("dport was not hammered from both cores; result means nothing");
        }
    }
    loop {
        sleep_ms(1000);
    }
}
