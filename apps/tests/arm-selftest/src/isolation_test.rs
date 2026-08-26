// SPDX-License-Identifier: Apache-2.0
//! Protected controller plus tiny user-only payloads. Fault recovery exists
//! only in kernel/isolation-test; each allowed fault matches a protected
//! one-shot task/PC/address manifest. An arbitrary HardFault never passes.

use core::ptr::{read_volatile, write_volatile};
use hal::{Priority, TaskId};
use kernel::scheduler::{Affinity, TaskState};
use portable_atomic::{AtomicU32, Ordering};

#[no_mangle]
static ISOLATION_RESULTS: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];
#[no_mangle]
static ISOLATION_NONCE: AtomicU32 = AtomicU32::new(0);
static EXPECTED: [AtomicU32; 5] = [const { AtomicU32::new(0) }; 5];
static KERNEL_SENTINEL: AtomicU32 = AtomicU32::new(0xa551_3819);

core::arch::global_asm!(
    r#"
.syntax unified
.thumb
.section .user.text.probe, "ax", %progbits
.thumb_func
.global isolation_user_probe
isolation_user_probe:
    push {r4-r7, lr}
    movs r0, #5
    svc 1
    mov r4, r0
1:  ldr r1, [r4]
    cmp r1, #0
    beq 1b
    /* An unprivileged MSR cannot promote the task. */
    movs r0, #2
    msr control, r0
    isb
    mrs r0, control
    str r0, [r4, #20]
    movs r0, #2
    svc 1
    str r0, [r4, #24]
    movs r0, #3
    svc 1
    str r0, [r4, #28]
    /* Even SVC zero must enter the restricted dispatcher, not boot. */
    movs r0, #255
    svc 0
    adds r0, #1
    str r0, [r4, #32]
    ldr r5, [r4, #4]
    ldr r0, [r4, #8]
    movs r1, #99
    cmp r5, #0
    beq isolation_read_pc
    cmp r5, #1
    beq isolation_write_pc
    blx r0
    b isolation_resume
.global isolation_read_pc
isolation_read_pc:
    ldr r1, [r0]
    b isolation_resume
.global isolation_write_pc
isolation_write_pc:
    str r1, [r0]
.global isolation_resume
isolation_resume:
    movs r0, #1
    str r0, [r4, #12]
    pop {r4-r7, pc}
"#,
    options(raw)
);

unsafe extern "C" {
    fn isolation_user_probe();
    static isolation_read_pc: u8;
    static isolation_write_pc: u8;
    static isolation_resume: u8;
}

#[link_section = ".user.text.entry"]
fn user_probe() {
    unsafe { isolation_user_probe() };
}

#[link_section = ".user.text.worker"]
fn user_worker() {
    let data = api::isolated::data_base().cast::<u32>();
    let id = api::isolated::current_id();
    let core = api::isolated::current_core();
    // Publish all peers before any worker can finish at the faster clock.
    while unsafe { read_volatile(data.add(2)) } == 0 {
        api::isolated::yield_now();
    }
    for i in 1..=200u32 {
        // Volatile private memory and syscalls on both sides of each switch.
        unsafe {
            write_volatile(data, i);
        }
        for _ in 0..3000 {
            core::hint::spin_loop();
        }
        api::isolated::yield_now();
        if api::isolated::current_id() != id
            || api::isolated::current_core() != core
            || unsafe { read_volatile(data) } != i
        {
            unsafe {
                write_volatile(data.add(1), 0xbad);
            }
            api::isolated::exit();
        }
    }
    unsafe {
        write_volatile(data.add(1), 0x600d);
    }
}

#[no_mangle]
unsafe extern "C" fn _flint_isolation_test_fault(id: u32, frame: *mut u32) -> bool {
    if EXPECTED[0].load(Ordering::Acquire) != id + 1 {
        return false;
    }
    let pc = unsafe { frame.add(6).read() };
    let address = unsafe { frame.read() };
    if pc != EXPECTED[1].load(Ordering::Relaxed)
        || address != EXPECTED[2].load(Ordering::Relaxed)
        || EXPECTED[4].load(Ordering::Relaxed) != 0
    {
        return false;
    }
    unsafe {
        frame.add(6).write(EXPECTED[3].load(Ordering::Relaxed));
    }
    EXPECTED[4].store(1, Ordering::Release);
    EXPECTED[0].store(0, Ordering::Release);
    true
}

fn fail(code: u32) -> ! {
    ISOLATION_RESULTS[0].store(0xbad1_3900 | code, Ordering::Release);
    loop {
        api::task::sleep_ms(100);
    }
}
fn memory(id: TaskId) -> hal::isolation::TaskMemory {
    kernel::scheduler::with(|s| s.tasks[id.0 as usize].as_ref().unwrap().isolation.unwrap())
}
fn spawn(entry: fn(), core: u8, stack: u32) -> TaskId {
    kernel::isolation::spawn(
        "user",
        entry,
        // Separate per-core priority rings: the scheduler's round-robin cursor
        // is shared across cores, so a remote dispatch can otherwise make a
        // yield select the same local task. Count actual switches, not SVCs.
        Priority::Normal(2 + core),
        stack,
        256,
        Affinity::Core(hal::smp::CoreId(core)),
    )
    .unwrap_or_else(|_| fail(2))
}
fn wait_exit(id: TaskId) {
    let deadline = api::timer::now_ms() + 20_000;
    loop {
        if kernel::scheduler::with(|s| {
            s.tasks[id.0 as usize].as_ref().unwrap().state == TaskState::Suspended
                && !s.is_current_anywhere(id.0)
        }) {
            return;
        }
        if api::timer::now_ms() > deadline {
            ISOLATION_RESULTS[6].store(id.0, Ordering::Relaxed);
            let d = memory(id).data().unwrap().base() as *const u32;
            ISOLATION_RESULTS[7].store(unsafe { read_volatile(d) }, Ordering::Relaxed);
            fail(3);
        }
        api::task::sleep_ms(10);
    }
}

pub fn run() {
    ISOLATION_RESULTS[0].store(0x1390_0001, Ordering::Release);
    let deadline = api::timer::now_ms() + 60_000;
    while ISOLATION_NONCE.load(Ordering::Acquire) == 0 {
        if api::timer::now_ms() > deadline {
            fail(1);
        }
        api::task::sleep_ms(1);
    }
    ISOLATION_RESULTS[1].store(ISOLATION_NONCE.load(Ordering::Acquire), Ordering::Release);
    // Ordinary kernel code and non-power-of-two stacks must be refused before
    // allocating a TCB or making any task runnable.
    for (entry, stack, data) in [
        (run as fn(), 1024, 256),
        (user_probe, 1025, 256),
        (user_probe, 1024, 255),
    ] {
        if kernel::isolation::spawn(
            "invalid",
            entry,
            Priority::Normal(2),
            stack,
            data,
            Affinity::Any,
        )
        .is_ok()
        {
            fail(4);
        }
        ISOLATION_RESULTS[3].fetch_add(1, Ordering::Relaxed);
    }
    let workers = [
        spawn(user_worker, 0, 1024),
        spawn(user_worker, 0, 1024),
        spawn(user_worker, 1, 1024),
        spawn(user_worker, 1, 1024),
    ];
    for id in workers {
        let data = memory(id).data().unwrap().base() as *mut u32;
        unsafe { write_volatile(data.add(2), 1) };
    }
    for id in workers {
        wait_exit(id);
    }
    for id in workers {
        let d = memory(id).data().unwrap().base() as *const u32;
        if unsafe { read_volatile(d) != 200 || read_volatile(d.add(1)) != 0x600d } {
            fail(5);
        }
        ISOLATION_RESULTS[4].fetch_add(200, Ordering::Relaxed);
    }
    let other = memory(workers[0]);
    for core in 0..2u8 {
        // Each task uses only 1K+256 bytes; all cases fit the static pool.
        // Deny kernel/data/stack, flash aliases, DMA and SIO, MPU control, and
        // execution outside the RX grant. Compare readable sentinels after.
        for case in 0..12 {
            let id = spawn(user_probe, core, 1024);
            let m = memory(id);
            let data = m.data().unwrap().base() as *mut u32;
            let (mode, target) = match case {
                0 => (0, &KERNEL_SENTINEL as *const _ as u32),
                1 => (1, &KERNEL_SENTINEL as *const _ as u32),
                2 => (1, other.data().unwrap().base()),
                3 => (1, other.stack().usable().start + 64),
                4 => (1, m.stack().base()),
                5 => (2, m.data().unwrap().base() | 1),
                6 => (1, m.code().base()),
                7 => (0, 0xd000_0000), // SIO CPUID, no side effects if broken
                8 => (0, 0x5000_0000), // DMA, no authority granted
                9 => (0, 0xe000_ed94), // MPU control
                10 => (0, 0x1100_0000 + (m.code().base() - 0x1000_0000)), // XIP alias
                _ => (2, run as *const () as u32), // privileged code is not executable
            };
            let pc = match mode {
                0 => core::ptr::addr_of!(isolation_read_pc) as u32,
                1 => core::ptr::addr_of!(isolation_write_pc) as u32,
                _ => target & !1,
            };
            EXPECTED[1].store(pc, Ordering::Relaxed);
            EXPECTED[2].store(target, Ordering::Relaxed);
            EXPECTED[3].store(
                core::ptr::addr_of!(isolation_resume) as u32,
                Ordering::Relaxed,
            );
            EXPECTED[4].store(0, Ordering::Relaxed);
            EXPECTED[0].store(id.0 + 1, Ordering::Release);
            unsafe {
                write_volatile(data.add(1), mode);
                write_volatile(data.add(2), target);
                core::arch::asm!("dmb", options(nostack));
                write_volatile(data, 1);
            }
            wait_exit(id);
            if EXPECTED[4].load(Ordering::Acquire) != 1 {
                fail(6);
            }
            unsafe {
                if read_volatile(data.add(3)) != 1
                    || read_volatile(data.add(5)) != 3
                    || read_volatile(data.add(6)) != id.0
                    || read_volatile(data.add(7)) != u32::from(core)
                    || read_volatile(data.add(8)) != 0
                {
                    fail(7);
                }
            }
            if KERNEL_SENTINEL.load(Ordering::Relaxed) != 0xa551_3819 {
                fail(8);
            }
            ISOLATION_RESULTS[2].fetch_add(1, Ordering::Relaxed);
        }
        ISOLATION_RESULTS[5].fetch_add(1, Ordering::Relaxed);
    }
    for core in 0..2 {
        let count = kernel::isolation::ISOLATION_ACTIVATIONS[core].load(Ordering::Relaxed);
        ISOLATION_RESULTS[6 + core].store(count, Ordering::Relaxed);
    }
    if (0..2).any(|core| ISOLATION_RESULTS[6 + core].load(Ordering::Relaxed) < 200) {
        fail(9);
    }
    api::log_info!("[FLINT] MPU faults=24 rejected=3 iterations=800 cores=2");
    ISOLATION_RESULTS[0].store(0x1390_600d, Ordering::Release);
    loop {
        api::task::sleep_ms(100);
    }
}
