// SPDX-License-Identifier: Apache-2.0

//! Machine-observable completion for ARM on-target tests.
//!
//! Passing returns to ROM BOOTSEL, which a host can identify as USB
//! `2e8a:0003`. Failure never enters BOOTSEL; the LED repeats the failure code
//! for a human while the host reaches its deadline. This avoids calling a
//! silent board a pass merely because serial output was unavailable.

const RESETS_CLR: *mut u32 = 0x4000_F000 as *mut u32;
const RESETS_DONE: *const u32 = 0x4000_C008 as *const u32;
const IO_BANK0_GPIO13_CTRL: *mut u32 = (crate::IO_BANK0_BASE + 13 * 8 + 4) as *mut u32;
const SIO_GPIO_OUT_SET: *mut u32 = (crate::SIO_BASE + 0x14) as *mut u32;
const SIO_GPIO_OUT_CLR: *mut u32 = (crate::SIO_BASE + 0x18) as *mut u32;
const SIO_GPIO_OE_SET: *mut u32 = (crate::SIO_BASE + 0x24) as *mut u32;
const LED: u32 = 1 << 13;

/// Last target-test state retained for SWD diagnosis (`0x100 + failure code`).
#[no_mangle]
pub static mut FLINT_RP2040_TEST_STATUS: u32 = 0;
static mut EXPECTED_FAULT_ARM: u32 = 0;
const EXPECTED_FAULT_MAGIC: u32 = 0xface_0087;

/// Arm the one deliberate HardFault in the ARM target suite.
///
/// # Safety
/// Call immediately before the injected fault with interrupts masked or from
/// the sole running test task. Any intervening fault is deliberately rejected.
pub unsafe fn arm_expected_fault() {
    unsafe { core::ptr::write_volatile(&raw mut EXPECTED_FAULT_ARM, EXPECTED_FAULT_MAGIC) };
}

/// Consume and validate the deliberate-fault arm token.
pub fn take_expected_fault_arm() -> bool {
    let armed = unsafe { core::ptr::read_volatile(&raw const EXPECTED_FAULT_ARM) };
    unsafe { core::ptr::write_volatile(&raw mut EXPECTED_FAULT_ARM, 0) };
    armed == EXPECTED_FAULT_MAGIC
}

fn led_init() {
    const RESET_IO_BANK0: u32 = 1 << 5;
    const RESET_PADS_BANK0: u32 = 1 << 8;
    const GPIO_FUNC_SIO: u32 = 5;
    let mask = RESET_IO_BANK0 | RESET_PADS_BANK0;
    unsafe {
        RESETS_CLR.write_volatile(mask);
        while RESETS_DONE.read_volatile() & mask != mask {}
        IO_BANK0_GPIO13_CTRL.write_volatile(GPIO_FUNC_SIO);
        SIO_GPIO_OE_SET.write_volatile(LED);
        SIO_GPIO_OUT_CLR.write_volatile(LED);
    }
}

fn delay() {
    for _ in 0..300_000 {
        core::hint::spin_loop();
    }
}

fn pulse(code: u8) {
    for _ in 0..code.max(1) {
        unsafe { SIO_GPIO_OUT_SET.write_volatile(LED) };
        delay();
        unsafe { SIO_GPIO_OUT_CLR.write_volatile(LED) };
        delay();
    }
}

/// Report a passing suite and enter ROM USB BOOTSEL through a full watchdog reset.
///
/// # Safety
/// Terminates normal execution. A hardware reset stops both cores before the
/// pre-RAM recovery handler calls ROM, avoiding a live dual-core ROM handoff.
/// Call only after all test result writes are complete.
pub unsafe fn pass_to_bootsel() -> ! {
    unsafe { core::ptr::write_volatile(&raw mut FLINT_RP2040_TEST_STATUS, 0x600d) };
    led_init();
    pulse(2);
    unsafe { crate::watchdog::arm(100, false) };
    loop {
        core::hint::spin_loop();
    }
}

/// Publish a passing status for an attached SWD harness and remain readable.
///
/// The harness consumes the status and then uses the watchdog registers to
/// return the target to BOOTSEL, so this path needs no unreliable live ROM call.
///
/// # Safety
/// Terminates application execution and exposes the final result through a
/// volatile global. Call only after all test output has drained.
pub unsafe fn pass_live() -> ! {
    unsafe { core::ptr::write_volatile(&raw mut FLINT_RP2040_TEST_STATUS, 0x600d) };
    loop {
        core::hint::spin_loop();
    }
}

/// Halt with a repeating, one-to-15 pulse failure code and stay out of
/// BOOTSEL so the host reports a bounded failure.
pub fn fail(code: u8) -> ! {
    unsafe {
        core::ptr::write_volatile(&raw mut FLINT_RP2040_TEST_STATUS, 0x100 + u32::from(code))
    };
    led_init();
    loop {
        pulse(code.clamp(1, 15));
        delay();
        delay();
        delay();
    }
}

/// Terminal status for the self-test HardFault vector.
///
/// The host observes failure by the deliberate absence of BOOTSEL; pulse 15
/// distinguishes the injected fault from ordinary assertion codes on a bench.
pub fn hard_fault() -> ! {
    fail(15)
}
