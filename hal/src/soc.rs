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

