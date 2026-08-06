// SPDX-License-Identifier: Apache-2.0

//! Phase 0 standalone tests (plan W5.3).
//!
//! Option A removed the `syscall` instruction, so the old "syscall under
//! register-window pressure" test is gone. What remains is the timer-interrupt
//! context test: run a windowed, recursive computation that spans several ticks
//! and verify the timer ISR's save/restore did not corrupt it.
//!
//! Gated behind the `phase0-tests` feature; run during bring-up at gate G5.

#[cfg(feature = "phase0-tests")]
use arch_xtensa::registers;

/// Run all Phase 0 tests. Panics on failure (printed by the panic handler).
pub fn run() {
    #[cfg(feature = "phase0-tests")]
    test_timer_interrupt_preserves_context();
}

// ── Timer interrupt context save/restore ───────────────────────────────────

#[cfg(feature = "phase0-tests")]
fn test_timer_interrupt_preserves_context() {
    // Bounded workload (was fib(45) ≈ 1.8B calls — far too slow for a boot
    // self-test). fib(25) = 75025 still builds many register-window frames and
    // spans multiple 1 ms ticks at 240 MHz.
    const EXPECTED: u32 = 75025; // fib(25)
    let result = fib(25);
    assert_eq!(result, EXPECTED, "context corrupted by timer interrupt");

    // Success indicator for a logic-analyzer / debugger.
    unsafe { registers::set_ccompare0(0xB000_0001) };
}

#[cfg(feature = "phase0-tests")]
fn fib(n: u32) -> u32 {
    match n {
        0 => 0,
        1 => 1,
        n => fib(n - 1) + fib(n - 2),
    }
}
