// SPDX-License-Identifier: Apache-2.0

//! System-on-chip services used by portable kernel code.

use crate::dma::DmaReach;

/// Static hardware facts that change kernel strategy, not board pin wiring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SocCapabilities {
    pub cores: u8,
    pub interrupt_matrix: bool,
    pub cache_off_execution: bool,
    pub hardware_rng: bool,
}

/// The chip underneath a kernel build.
pub trait SystemOnChip {
    type Dma: DmaReach;

    const DMA: Self::Dma;
    const DEFAULT_CPU_HZ: u32;
    const APB_HZ: u32;
    const CAPABILITIES: SocCapabilities;

    /// The chip's memory-mapped peripheral register window, as
    /// `(low, high)` inclusive bounds.
    ///
    /// A chip fact, not a board one: every peripheral base address a board
    /// manifest names must land inside it. The board crate's manifest
    /// invariant tests check that here instead of hard-coding the window per
    /// board — the ESP32 family maps its DPORT peripherals into
    /// `0x3FF4_0000..=0x3FF7_FFFF` (widened slightly at both ends), the RP2040
    /// its APB peripherals from `0x4000_0000`.
    const PERIPHERAL_WINDOW: (u32, u32);

    /// The highest GPIO number the chip exposes.
    ///
    /// The ESP32 bonds out GPIO0..=39 (34-39 input-only); the RP2040 GP0..=29.
    /// A pin number in a board manifest above this is a copy-paste error, and
    /// the manifest invariant tests reject it against this bound.
    const MAX_GPIO: u8;

    /// Put the CPU clock into the operating state expected by the kernel.
    ///
    /// # Safety
    /// Called once during single-core boot, before clock consumers start.
    unsafe fn configure_cpu_clock();

    /// Read the reset cause retained by the chip.
    ///
    /// # Safety
    /// May read memory-mapped reset-state registers.
    unsafe fn reset_cause() -> u32;

    fn reset_cause_name(cause: u32) -> &'static str;

    /// Measure the CPU clock against a chip-owned reference clock.
    fn measure_cpu_hz(cycle_count: fn() -> Option<u32>) -> Option<u32>;
}

