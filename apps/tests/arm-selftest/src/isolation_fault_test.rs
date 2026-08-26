// SPDX-License-Identifier: Apache-2.0
//! Production fault path: deliberately undefined instruction, NOT an MPU
//! violation. No expected-fault recovery hook is linked in this variant.
use portable_atomic::{AtomicU32, Ordering};

#[no_mangle]
static ISOLATION_RESULTS: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];
#[no_mangle]
static ISOLATION_NONCE: AtomicU32 = AtomicU32::new(0);
#[no_mangle]
static ISOLATION_FAULT_CORE: AtomicU32 = AtomicU32::new(0);

core::arch::global_asm!(
    r#"
.syntax unified
.thumb
.section .user.text.fault, "ax", %progbits
.thumb_func
.global isolation_unexpected
isolation_unexpected:
    movs r0, #5
    svc 1
    mov r1, r0
1:  ldr r0, [r1]
    cmp r0, #0
    beq 1b
.global ISOLATION_UNDEFINED_PC
ISOLATION_UNDEFINED_PC:
    udf #7
2:  b 2b
"#,
    options(raw)
);

unsafe extern "C" {
    fn isolation_unexpected();
}
#[link_section = ".user.text.fault_entry"]
fn user_fault() {
    unsafe { isolation_unexpected() };
}

pub fn run() {
    ISOLATION_RESULTS[0].store(0x139f_0001, Ordering::Release);
    // A fresh boot waits here, including the reboot after the fault. No
    // automatic repeated crashes; the host must authorize one new nonce.
    let deadline = api::timer::now_ms() + 60_000;
    while ISOLATION_NONCE.load(Ordering::Acquire) == 0 {
        if api::timer::now_ms() > deadline {
            ISOLATION_RESULTS[0].store(0xbad1_39ff, Ordering::Release);
            loop {
                api::task::sleep_ms(100);
            }
        }
        api::task::sleep_ms(10);
    }
    let nonce = ISOLATION_NONCE.load(Ordering::Acquire);
    let core = ISOLATION_FAULT_CORE.load(Ordering::Acquire);
    assert!(core < 2, "fault test core must exist");
    let id = kernel::isolation::spawn(
        "undefined",
        user_fault,
        hal::Priority::Normal(2),
        1024,
        256,
        kernel::scheduler::Affinity::Core(hal::smp::CoreId(core as u8)),
    )
    .expect("isolated fault task");
    let data = kernel::scheduler::with(|s| {
        s.tasks[id.0 as usize]
            .as_ref()
            .unwrap()
            .isolation
            .unwrap()
            .data()
            .unwrap()
            .base()
    });
    unsafe { (data as *mut u32).write_volatile(nonce) };
    loop {
        api::task::sleep_ms(100);
    }
}
