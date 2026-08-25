// SPDX-License-Identifier: Apache-2.0

//! Reset recovery used by the architecture-neutral panic path.
//!
//! A panic handler may need to arrange an unattended reboot without naming a
//! chip or its watchdog registers. The kernel selects the active SoC and calls
//! it through [`PanicRecovery`], just as it selects timers and low-power
//! support through their HAL traits. A SoC that cannot preserve a postmortem
//! snapshot implements the empty default and the panic path remains halted.

/// A delayed, snapshot-preserving reboot after a terminal panic.
pub trait PanicRecovery {
    /// Arm recovery to occur after `timeout_ms`, returning whether recovery is
    /// available. The delay leaves time for bounded console output first.
    ///
    /// The default reports unsupported and changes no hardware.
    ///
    /// # Safety
    /// An implementation may change reset routing and arm a hardware watchdog.
    unsafe fn arm_panic_recovery(_timeout_ms: u32) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::PanicRecovery;

    struct NoRecovery;
    impl PanicRecovery for NoRecovery {}

    #[test]
    fn unsupported_soc_leaves_the_panic_halted() {
        assert!(!unsafe { NoRecovery::arm_panic_recovery(1_000) });
    }
}
