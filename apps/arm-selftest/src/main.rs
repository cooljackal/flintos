// SPDX-License-Identifier: Apache-2.0

//! Machine-judged RP2040 tests of the real FlintOS scheduler and kernel APIs.

#![no_std]
#![no_main]

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
use api::queue::{Queue, RecvError};
use api::task;
#[cfg(not(feature = "expected-hardfault"))]
use api::timer;
use hal::types::Priority;
#[cfg(not(feature = "expected-hardfault"))]
use portable_atomic::{AtomicU32, Ordering};

kernel::flint_app!(main, abi = 1);

#[cfg(not(feature = "expected-hardfault"))]
static PEER_RUNS: AtomicU32 = AtomicU32::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static CORE1_RUNS: AtomicU32 = AtomicU32::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static WRONG_CORE_RUNS: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static ISR_QUEUE: Queue<u32, 4> = Queue::new();
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static SMP_LOCK: kernel::smp::Spinlock<u32> = kernel::smp::Spinlock::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static CORE0_ACTIVE: AtomicU32 = AtomicU32::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static CORE1_ACTIVE: AtomicU32 = AtomicU32::new(0);
#[cfg(not(feature = "expected-hardfault"))]
static DUPLICATE_RUNS: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static REMOTE_REQUEST: AtomicU32 = AtomicU32::new(0);
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
#[cfg(target_arch = "arm")]
core::arch::global_asm!(
    r#"
    .syntax unified
    .cpu cortex-m0plus
    .thumb
    .global arm_sentinel_yield
    .type arm_sentinel_yield,%function
    .thumb_func
arm_sentinel_yield:
    push {r3-r7,lr}
    mov r4,r8
    mov r5,r9
    mov r6,r10
    mov r7,r11
    push {r4-r7}
    movs r4,#0x44
    movs r5,#0x45
    movs r6,#0x46
    movs r7,#0x47
    movs r0,#0x48
    mov r8,r0
    movs r0,#0x49
    mov r9,r0
    movs r0,#0x4a
    mov r10,r0
    movs r0,#0x4b
    mov r11,r0
    bl _flint_sys_yield
    movs r0,#0
    cmp r4,#0x44
    bne 1f
    cmp r5,#0x45
    bne 1f
    cmp r6,#0x46
    bne 1f
    cmp r7,#0x47
    bne 1f
    mov r1,r8
    cmp r1,#0x48
    bne 1f
    mov r1,r9
    cmp r1,#0x49
    bne 1f
    mov r1,r10
    cmp r1,#0x4a
    bne 1f
    mov r1,r11
    cmp r1,#0x4b
    bne 1f
    movs r0,#1
1:  pop {r4-r7}
    mov r8,r4
    mov r9,r5
    mov r10,r6
    mov r11,r7
    pop {r3-r7,pc}

    .global arm_sentinel_preempt
    .type arm_sentinel_preempt,%function
    .thumb_func
arm_sentinel_preempt:
    push {r3-r7,lr}
    mov r4,r8
    mov r5,r9
    mov r6,r10
    mov r7,r11
    push {r4-r7}
    movs r4,#0x54
    movs r5,#0x55
    movs r6,#0x56
    movs r7,#0x57
    movs r0,#0x58
    mov r8,r0
    movs r0,#0x59
    mov r9,r0
    movs r0,#0x5a
    mov r10,r0
    movs r0,#0x5b
    mov r11,r0
    ldr r0,=120000
2:  subs r0,#1
    bne 2b
    movs r0,#0
    cmp r4,#0x54
    bne 3f
    cmp r5,#0x55
    bne 3f
    cmp r6,#0x56
    bne 3f
    cmp r7,#0x57
    bne 3f
    mov r1,r8
    cmp r1,#0x58
    bne 3f
    mov r1,r9
    cmp r1,#0x59
    bne 3f
    mov r1,r10
    cmp r1,#0x5a
    bne 3f
    mov r1,r11
    cmp r1,#0x5b
    bne 3f
    movs r0,#1
3:  pop {r4-r7}
    mov r8,r4
    mov r9,r5
    mov r10,r6
    mov r11,r7
    pop {r3-r7,pc}

    .global arm_inject_expected_fault
    .type arm_inject_expected_fault,%function
    .thumb_func
arm_inject_expected_fault:
    .global _flint_expected_fault_start
_flint_expected_fault_start:
    udf #0
    .global _flint_expected_fault_end
_flint_expected_fault_end:
    b _flint_expected_fault_end
"#,
    options(raw)
);

#[cfg(not(feature = "expected-hardfault"))]
#[allow(dead_code)]
unsafe extern "C" {
    fn arm_sentinel_yield() -> u32;
    fn arm_sentinel_preempt() -> u32;
}

#[cfg(feature = "expected-hardfault")]
unsafe extern "C" {
    fn arm_inject_expected_fault() -> !;
}

fn main() {
    #[cfg(feature = "expected-hardfault")]
    task::spawn("fault", inject_hardfault, Priority::Normal(1), 2048).expect("fault task");
    #[cfg(not(feature = "expected-hardfault"))]
    {
        task::spawn_on(0, "peer", peer, Priority::Normal(2), 2048).expect("peer task");
        task::spawn_on(1, "core1", core1_peer, Priority::Normal(2), 2048).expect("core-1 task");
        task::spawn_on(0, "tests", tests, Priority::Normal(2), 4096).expect("test task");
    }
}

#[cfg(not(feature = "expected-hardfault"))]
fn fail(code: u8) -> ! {
    soc_rp2040::test_status::fail(code)
}

#[cfg(not(feature = "expected-hardfault"))]
fn peer() {
    loop {
        if CORE0_ACTIVE.swap(1, Ordering::AcqRel) != 0 {
            DUPLICATE_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        if kernel::smp::current_core().0 != 0 {
            WRONG_CORE_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        SMP_LOCK.with(|count| *count = count.wrapping_add(1));
        PEER_RUNS.fetch_add(1, Ordering::Relaxed);
        CORE0_ACTIVE.store(0, Ordering::Release);
        task::sleep_ms(100);
    }
}

#[cfg(not(feature = "expected-hardfault"))]
fn core1_peer() {
    #[cfg(not(feature = "minimal"))]
    loop {
        if CORE1_ACTIVE.swap(1, Ordering::AcqRel) != 0 {
            DUPLICATE_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        if kernel::smp::current_core().0 != 1 {
            WRONG_CORE_RUNS.fetch_add(1, Ordering::Relaxed);
        }
        SMP_LOCK.with(|count| *count = count.wrapping_add(1));
        CORE1_RUNS.fetch_add(1, Ordering::Relaxed);
        #[cfg(not(feature = "minimal"))]
        {
            let request = REMOTE_REQUEST.load(Ordering::Acquire);
            if request != 0 {
                kernel::scheduler::with(|sched| sched.unblock(request - 1));
                REMOTE_REQUEST.store(0, Ordering::Release);
            }
        }
        CORE1_ACTIVE.store(0, Ordering::Release);
        let poll_start = hardware_timer_us();
        while hardware_timer_us().wrapping_sub(poll_start) < 1_000 {
            core::hint::spin_loop();
        }
        task::yield_now();
    }
}

#[cfg(all(
    target_arch = "arm",
    not(feature = "expected-hardfault"),
    not(feature = "minimal")
))]
fn hardware_timer_us() -> u32 {
    soc_rp2040::timer_us()
}

#[cfg(all(
    not(target_arch = "arm"),
    not(feature = "expected-hardfault"),
    not(feature = "minimal")
))]
fn hardware_timer_us() -> u32 {
    0
}

#[cfg(not(feature = "expected-hardfault"))]
fn tests() {
    // Leave enough time after UF2 boot for the host to observe BOOTSEL vanish
    // and arm its fresh-return judge; this delay is not part of any timing assertion.
    task::sleep_ms(2_000);
    if kernel::smp::current_core().0 != 0 || CORE1_RUNS.load(Ordering::Relaxed) == 0 {
        fail(13);
    }
    // Register-frame sentinels are covered by the single-core acceptance
    // suite. This image keeps both cores live and tests SMP-owned behavior.
    if WRONG_CORE_RUNS.load(Ordering::Relaxed) != 0 || CORE1_RUNS.load(Ordering::Relaxed) == 0 {
        fail(13);
    }
    let sleep_start = timer::now_ms();
    task::sleep_ms(15);
    let slept = timer::now_ms().wrapping_sub(sleep_start);
    if !(15..=500).contains(&slept) {
        fail(2);
    }

    #[cfg(feature = "minimal")]
    unsafe {
        soc_rp2040::test_status::pass_to_bootsel()
    }

    #[cfg(not(feature = "minimal"))]
    run_extended_tests();
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
fn run_extended_tests() -> ! {
    let timeout_start = timer::now_ms();
    let timeout_result = api::queue::recv(&ISR_QUEUE, 5);
    let timeout_elapsed = timer::now_ms().wrapping_sub(timeout_start);
    if timeout_result != Err(RecvError::Timeout) || !(5..=500).contains(&timeout_elapsed) {
        fail(3);
    }

    let outer = unsafe { kernel::arch::cs_enter() };
    let inner = unsafe { kernel::arch::cs_enter() };
    let masked_tick = timer::now_ms();
    let hardware_start = hardware_timer_us();
    while hardware_timer_us().wrapping_sub(hardware_start) < 5_000 {
        core::hint::spin_loop();
    }
    if timer::now_ms() != masked_tick {
        fail(4);
    }
    unsafe { kernel::arch::cs_exit(inner) };
    if timer::now_ms() != masked_tick {
        fail(5);
    }
    unsafe { kernel::arch::cs_exit(outer) };
    task::sleep_ms(2);
    if timer::now_ms() <= masked_tick {
        fail(6);
    }

    let (stack_base, stack_size) = kernel::scheduler::with(|sched| {
        let current = sched.current();
        let tcb = sched.tasks[current as usize].as_ref().expect("current TCB");
        (tcb.stack_base, tcb.stack_size)
    });
    if !kernel::spawn::stack_guard_intact(stack_base, stack_size) {
        fail(7);
    }

    let before = kernel::heap::free_bytes(kernel::heap::Caps::Internal);
    let ptr = unsafe { kernel::heap::alloc(257, 16) };
    if ptr.is_null() || ptr as usize & 15 != 0 {
        fail(8);
    }
    unsafe {
        ptr.write_volatile(0xa5);
        ptr.add(256).write_volatile(0x5a);
        if ptr.read_volatile() != 0xa5 || ptr.add(256).read_volatile() != 0x5a {
            fail(8);
        }
        kernel::heap::free(ptr, kernel::heap::Caps::Internal);
    }
    if kernel::heap::free_bytes(kernel::heap::Caps::Internal) != before {
        fail(8);
    }

    // Repeatedly block core 0 and require a task pinned to core 1 to wake it.
    // The raw peripheral timer remains independent of the scheduler tick, so
    // the upper bound also catches a stopped/doubled shared timebase.
    const SOAK_ITERATIONS: u32 = 256;
    let lock_before = SMP_LOCK.with(|count| *count);
    let core0_before = PEER_RUNS.load(Ordering::Relaxed);
    let core1_before = CORE1_RUNS.load(Ordering::Relaxed);
    let soak_start = hardware_timer_us();
    let mut worst_wake_us = 0;
    let test_task = kernel::scheduler::with(|sched| sched.current());
    for _ in 1..=SOAK_ITERATIONS {
        let wake_start = hardware_timer_us();
        REMOTE_REQUEST.store(test_task + 1, Ordering::Release);
        task::sleep_ms(500);
        if REMOTE_REQUEST.load(Ordering::Acquire) != 0 {
            fail(14);
        }
        worst_wake_us = worst_wake_us.max(hardware_timer_us().wrapping_sub(wake_start));
        SMP_LOCK.with(|count| {
            // Keep the protected interval non-empty so the two pinned tasks
            // exercise real SIO-lock contention, not merely serialized calls.
            let next = count.wrapping_add(1);
            core::hint::black_box(next);
            *count = next;
        });
        // Give the lower-priority core-0 peer a deterministic window. A
        // yield need not select a lower-priority task, while sleep must.
        task::sleep_ms(1);
    }
    let soak_us = hardware_timer_us().wrapping_sub(soak_start);
    if worst_wake_us > 500_000 || soak_us > 30_000_000 {
        fail(15);
    }
    if DUPLICATE_RUNS.load(Ordering::Relaxed) != 0 || WRONG_CORE_RUNS.load(Ordering::Relaxed) != 0 {
        fail(16);
    }
    if PEER_RUNS.load(Ordering::Relaxed) == core0_before
        || CORE1_RUNS.load(Ordering::Relaxed) < core1_before.wrapping_add(SOAK_ITERATIONS)
    {
        fail(17);
    }
    let lock_after = SMP_LOCK.with(|count| *count);
    if lock_after.wrapping_sub(lock_before) < SOAK_ITERATIONS * 2 {
        fail(18);
    }
    unsafe { soc_rp2040::test_status::pass_to_bootsel() }
}

#[cfg(feature = "expected-hardfault")]
fn inject_hardfault() {
    task::sleep_ms(750);
    unsafe {
        soc_rp2040::test_status::arm_expected_fault();
        arm_inject_expected_fault()
    }
}
