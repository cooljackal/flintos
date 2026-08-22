// SPDX-License-Identifier: Apache-2.0

//! Light-sleep self-test. Included by [`crate::selftest`].
//!
//! Only light sleep is tested here, and only briefly. Light sleep pauses the
//! CPU and returns, so it can be checked from inside the running suite; deep
//! sleep wakes as a chip reset, which would restart the board mid-run and cut
//! the harness's serial stream, so it is covered by construction and host tests
//! (`kernel::power`, `soc_esp32::sleep`) only.
//!
//! # Why this needs the chip
//!
//! On a host there is no CPU clock to gate and no RTC to keep running while it
//! is gated, so the one property that matters — that the tick freezes across a
//! real sleep and is then reconciled from the RTC — is not falsifiable off
//! silicon. The reconciliation *arithmetic* is host-tested in
//! `kernel::power`; this is the check that the arithmetic is fed real numbers.
//!
//! # Safety of the test itself
//!
//! The sleep is short (20 ms) and always timer-armed, so a bad config fails as
//! a timeout — `light_sleep` returns an error and the test panics loudly —
//! rather than a permanent hang. The one failure this cannot convert to a
//! timeout is a sleep that pauses the CPU but whose timer never fires: the CPU
//! is halted, so no code runs to time out. That is the residual hardware risk
//! the SoC layer's `MIN_SLP_VAL` and timer arming guard against; see
//! `soc_esp32::sleep`.

use hal::tick::TickSource;

use crate::arch::Tick;

use super::Check;

/// A short light sleep must wake, and the tick must catch up without ever going
/// backwards.
///
/// The requested 20 ms is measured against the RTC, which keeps running while
/// the CPU is paused, then reconciled into the tick. The window is wide because
/// the RTC reference is an uncalibrated RC oscillator (5–10% off) and the tick
/// is whole-millisecond: a 20 ms sleep may reconcile to anywhere from ~15 to
/// ~30 ticks. What is *not* slack is the direction — the tick must not regress
/// — and that some real time must have passed, which is what proves the CPU
/// actually paused rather than the sleep being silently skipped.
#[cfg(target_os = "none")]
pub(crate) fn light_sleep_wakes_and_the_tick_catches_up() -> Check {
    const SLEEP_MS: u32 = 20;

    let before = Tick::now();
    let measured_ms = crate::power::light_sleep(SLEEP_MS);
    let after = Tick::now();

    // (a) Execution resumed: reaching this line at all proves the wake fired
    // and state was retained. A sleep that never woke would have hung here, not
    // returned.

    // (b) The tick did not go backwards — the whole reason reconciliation only
    // ever adds.
    if after < before {
        return Err("tick went backwards across light sleep");
    }

    let advanced = after - before;

    // Some real time must have elapsed. If the sleep were skipped entirely the
    // tick would have moved only by the handful of microseconds the call itself
    // takes — far less than a single 1 ms tick.
    if advanced == 0 {
        return Err("tick did not advance; the sleep was skipped, not entered");
    }

    // ...and it must land near the requested duration, not wildly past it (a
    // reconciliation that double-counted, or a runaway comparator).
    if !(10..=40).contains(&advanced) {
        return Err("tick advance is nowhere near the 20 ms slept");
    }

    // The reported measurement and the reconciled tick must agree: the tick was
    // advanced *from* the measurement, so a large disagreement means the two
    // paths disagree about how long the sleep was.
    if measured_ms.abs_diff(advanced) > 10 {
        return Err("measured sleep and reconciled tick disagree");
    }

    Ok(())
}

/// Host stand-in. The self-test module is target-only, so this is never
/// compiled into a real run; it documents that the test has no host form and
/// keeps the file honest about the `#[cfg]` split the other peripherals use.
#[cfg(not(target_os = "none"))]
pub(crate) fn light_sleep_wakes_and_the_tick_catches_up() -> Check {
    Err("light sleep is target-only; there is no CPU clock to gate on a host")
}
