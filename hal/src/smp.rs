// SPDX-License-Identifier: Apache-2.0

//! Core identity, for kernels that run on more than one.
//!
//! Deliberately minimal and deliberately not ESP32-shaped. The interesting
//! multi-core parts today are as often asymmetric as symmetric — a Cortex-M7
//! beside an M0, an ESP32's PRO and APP cores, an RP2040's two identical M0s —
//! and the one thing all of them need from a kernel is *"which core am I?"*,
//! answered cheaply enough to sit at the top of a lock.

/// Which core is executing.
///
/// A small integer rather than an enum: cores are numbered on every part that
/// has more than one, and a kernel that wants to say "core 1" should not need
/// a variant added first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct CoreId(pub u8);

impl CoreId {
    /// The core that runs at reset and owns bring-up.
    ///
    /// Every part in this class has one: the ESP32 calls it PRO, an M7/M0 pair
    /// boots the M7, an RP2040 boots core 0. Naming it here means portable
    /// code can say "the boot core" without naming a chip.
    pub const BOOT: Self = Self(0);

    pub const fn index(self) -> usize {
        self.0 as usize
    }

    pub const fn is_boot(self) -> bool {
        self.0 == Self::BOOT.0
    }
}

/// The most cores any supported part has.
///
/// Sizes per-core arrays. Two today; raising it costs a little static memory
/// and nothing else.
pub const MAX_CORES: usize = 2;

/// Reading core identity.
///
/// One method, because everything else a kernel needs about a core — its
/// stack, its current task — is the kernel's own bookkeeping indexed by this.
pub trait MultiCore {
    /// Which core is calling.
    ///
    /// Must be cheap: this sits at the top of every lock acquisition. On
    /// Xtensa it is two instructions.
    fn current_core() -> CoreId;

    /// How many cores this part has running Flint. Not how many exist.
    fn cores() -> u8;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_boot_core_is_zero_everywhere() {
        // Portable code says "boot core" rather than "core 0"; if these ever
        // disagree, that code silently means something else.
        assert_eq!(CoreId::BOOT, CoreId(0));
        assert!(CoreId::BOOT.is_boot());
        assert!(!CoreId(1).is_boot());
    }

    #[test]
    fn a_core_id_indexes_a_per_core_array() {
        let mut per_core = [0u32; MAX_CORES];
        per_core[CoreId::BOOT.index()] = 7;
        per_core[CoreId(1).index()] = 9;
        assert_eq!(per_core, [7, 9]);
    }
}
