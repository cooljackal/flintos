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
#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
static ISR_QUEUE: Queue<u32, 4> = Queue::new();

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
        task::spawn("peer", peer, Priority::Normal(2), 2048).expect("peer task");
        task::spawn("tests", tests, Priority::Normal(2), 4096).expect("test task");
    }
}

#[cfg(not(feature = "expected-hardfault"))]
fn fail(code: u8) -> ! {
    soc_rp2040::test_status::fail(code)
}

#[cfg(not(feature = "expected-hardfault"))]
fn peer() {
    loop {
        PEER_RUNS.fetch_add(1, Ordering::Relaxed);
        task::yield_now();
    }
}

#[cfg(all(not(feature = "expected-hardfault"), not(feature = "minimal")))]
fn timer_isr() {
    if ISR_QUEUE.send_isr(0x51_52_53_54).is_err() {
        fail(9);
    }
}

#[cfg(target_arch = "arm")]
unsafe fn stop_tick() {
    unsafe { kernel::arch::Tick::stop() };
}

#[cfg(not(target_arch = "arm"))]
unsafe fn stop_tick() {}

#[cfg(target_arch = "arm")]
unsafe fn start_tick() {
    unsafe { kernel::arch::Tick::start() };
}

#[cfg(not(target_arch = "arm"))]
unsafe fn start_tick() {}

#[cfg(target_arch = "arm")]
fn hardware_timer_us() -> u32 {
    soc_rp2040::timer_us()
}

#[cfg(not(target_arch = "arm"))]
fn hardware_timer_us() -> u32 {
    0
}

#[cfg(not(feature = "expected-hardfault"))]
fn tests() {
    // Leave enough time after UF2 boot for the host to observe BOOTSEL vanish
    // and arm its fresh-return judge; this delay is not part of any timing assertion.
    task::sleep_ms(750);
    for _ in 0..32 {
        unsafe { stop_tick() };
        let before = PEER_RUNS.load(Ordering::Relaxed);
        let registers_ok = unsafe { arm_sentinel_yield() } == 1;
        let peer_advanced = PEER_RUNS.load(Ordering::Relaxed) != before;
        unsafe { start_tick() };
        if !registers_ok {
            fail(1);
        }
        if !peer_advanced {
            fail(12);
        }
    }
    let preempt_before = PEER_RUNS.load(Ordering::Relaxed);
    if unsafe { arm_sentinel_preempt() } != 1 || PEER_RUNS.load(Ordering::Relaxed) == preempt_before
    {
        fail(11);
    }
    let peer_before = PEER_RUNS.load(Ordering::Relaxed);
    let sleep_start = timer::now_ms();
    task::sleep_ms(15);
    let slept = timer::now_ms().wrapping_sub(sleep_start);
    if !(15..=100).contains(&slept) || PEER_RUNS.load(Ordering::Relaxed) == peer_before {
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
    if timeout_result != Err(RecvError::Timeout) || !(5..=100).contains(&timeout_elapsed) {
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

    let _timer = timer::once_ms(3, timer_isr);
    match api::queue::recv(&ISR_QUEUE, 20) {
        Ok(0x51_52_53_54) => {}
        _ => fail(10),
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
