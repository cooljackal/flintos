// SPDX-License-Identifier: Apache-2.0

//! Board manifest integration.
//!
//! Re-exports the board manifest and provides runtime initialisation
//! of board-level resources (buses, devices, peripherals).
//!
//! The concrete board is selected by a Cargo feature on `board`
//! (forwarded through this crate's own `board-*` features — see
//! `kernel/Cargo.toml`), not named here. This module just forwards
//! whichever manifest `board` resolved to, so switching boards never
//! touches kernel source:
//!
//! ```text
//! cargo build -p kernel --no-default-features --features board-m5-atom
//! ```

#[cfg(feature = "soc-esp32")]
pub use board::active;

/// SoC selected by the active ESP32 board family.
#[cfg(feature = "soc-esp32")]
pub type SelectedSoc = soc_esp32::Esp32;

#[cfg(feature = "soc-rp2040")]
pub struct SelectedSoc;

#[cfg(feature = "soc-rp2040")]
pub struct Rp2040Dma;

#[cfg(feature = "soc-rp2040")]
impl hal::dma::DmaReach for Rp2040Dma {
    fn reachable(&self, addr: u32, len: u32) -> bool {
        if len == 0 {
            return true;
        }
        addr.checked_add(len - 1)
            .is_some_and(|last| addr >= 0x2000_0000 && last < 0x2004_2000)
    }
}

#[cfg(feature = "soc-rp2040")]
impl hal::soc::SystemOnChip for SelectedSoc {
    type Dma = Rp2040Dma;

    const DMA: Self::Dma = Rp2040Dma;
    const DEFAULT_CPU_HZ: u32 = 125_000_000;
    const APB_HZ: u32 = 125_000_000;
    const CAPABILITIES: hal::soc::SocCapabilities = hal::soc::SocCapabilities {
        cores: 2,
        interrupt_matrix: false,
        cache_off_execution: false,
        hardware_rng: false,
    };

    unsafe fn configure_cpu_clock() {}

    unsafe fn reset_cause() -> u32 {
        0
    }

    fn reset_cause_name(_cause: u32) -> &'static str {
        "unknown"
    }

    fn measure_cpu_hz(_cycle_count: fn() -> Option<u32>) -> Option<u32> {
        None
    }
}
