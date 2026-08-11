// SPDX-License-Identifier: Apache-2.0

//! Interrupting the other core.
//!
//! The ESP32 has four "from CPU" interrupt sources. Each is a single bit in a
//! DPORT register that any core may set, and it asserts a level-triggered
//! interrupt source in the crossbar. Which core it lands on is decided by
//! [`crate::intr_map`], not by which register was written: route the source in
//! one core's table and leave it parked in the other's, and the signal is
//! aimed.
//!
//! Level-triggered, so **the handler must clear the bit**. Returning without
//! calling [`clear`] re-enters it forever.
//!
//! # Register facts
//!
//! From `soc/dport_reg.h` and `soc/soc.h` at v4.4, read rather than recalled:
//!
//! ```text
//! DPORT_CPU_INTR_FROM_CPU_0_REG   DPORT + 0x0DC   bit 0    source 24
//! DPORT_CPU_INTR_FROM_CPU_1_REG   DPORT + 0x0E0   bit 0    source 25
//! DPORT_CPU_INTR_FROM_CPU_2_REG   DPORT + 0x0E4   bit 0    source 26
//! DPORT_CPU_INTR_FROM_CPU_3_REG   DPORT + 0x0E8   bit 0    source 27
//! ```
//!
//! esp-idf's comments assign 0 and 1 to FreeRTOS's own cross-core yield and 2
//! and 3 to `IPC_ISR`. FlintOS uses 2, which is the one esp-idf uses for the
//! same job — parking the other core for a flash operation.

use crate::addr::DPORT_BASE;

/// Which of the four signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// esp-idf's FreeRTOS yield channel. Not used here; named so that a future
    /// user picks a free one deliberately rather than by counting.
    FromCpu0,
    FromCpu1,
    /// What `esp32-flash` parks the other core with. esp-idf's `IPC_ISR` pair.
    FromCpu2,
    FromCpu3,
}

impl Signal {
    /// The DPORT register holding this signal's bit.
    pub const fn reg(self) -> u32 {
        DPORT_BASE
            + match self {
                Signal::FromCpu0 => 0x0DC,
                Signal::FromCpu1 => 0x0E0,
                Signal::FromCpu2 => 0x0E4,
                Signal::FromCpu3 => 0x0E8,
            }
    }

    /// The crossbar source number to route.
    pub const fn source(self) -> u8 {
        match self {
            Signal::FromCpu0 => 24,
            Signal::FromCpu1 => 25,
            Signal::FromCpu2 => 26,
            Signal::FromCpu3 => 27,
        }
    }
}

/// Bit 0 of every one of the four registers.
const BIT: u32 = 1 << 0;

/// Raise `signal`, interrupting whichever core has its source routed.
///
/// Written straight through rather than via `dport::write`: the caller is
/// about to disable its own cache, and everything from here to the release has
/// to be reachable without one. A plain store to a DPORT register needs none
/// of the erratum handling a read-modify-write does.
///
/// # Safety
/// Writes a DPORT register. Raising a signal nothing has routed sets a bit
/// that stays set.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.crosscore")]
pub unsafe fn raise(signal: Signal) {
    unsafe { (signal.reg() as *mut u32).write_volatile(BIT) };
}

/// Clear `signal`. **A handler must do this**, or it re-enters forever.
///
/// # Safety
/// Writes a DPORT register.
#[inline(never)]
#[cfg_attr(target_os = "none", link_section = ".iram1.crosscore")]
pub unsafe fn clear(signal: Signal) {
    unsafe { (signal.reg() as *mut u32).write_volatile(0) };
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registers_are_where_dport_reg_h_puts_them() {
        // All four quoted from the header. One would prove the base; four
        // prove the stride, which is what indexing by signal assumes.
        assert_eq!(Signal::FromCpu0.reg(), 0x3FF0_00DC);
        assert_eq!(Signal::FromCpu1.reg(), 0x3FF0_00E0);
        assert_eq!(Signal::FromCpu2.reg(), 0x3FF0_00E4);
        assert_eq!(Signal::FromCpu3.reg(), 0x3FF0_00E8);
    }

    #[test]
    fn the_sources_are_the_ones_soc_h_names() {
        assert_eq!(Signal::FromCpu0.source(), 24);
        assert_eq!(Signal::FromCpu1.source(), 25);
        assert_eq!(Signal::FromCpu2.source(), 26);
        assert_eq!(Signal::FromCpu3.source(), 27);
    }

    #[test]
    fn every_signal_has_its_own_register_and_source() {
        // A copy-paste that gave two signals the same register would make one
        // of them silently interrupt the wrong handler.
        let all = [
            Signal::FromCpu0,
            Signal::FromCpu1,
            Signal::FromCpu2,
            Signal::FromCpu3,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a.reg(), b.reg());
                assert_ne!(a.source(), b.source());
            }
        }
    }

    #[test]
    fn the_source_is_routable_to_a_level_one_input() {
        // A from-CPU source that the crossbar will not serve is a signal that
        // can never be delivered.
        assert!(Signal::FromCpu2.source() < crate::intr_map::SOURCE_COUNT);
    }
}
