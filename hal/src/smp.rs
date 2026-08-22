// SPDX-License-Identifier: Apache-2.0

//! Core identity, for kernels that run on more than one.
//!
//! **Symmetric cores only.** Every core FlintOS runs on is assumed to be a peer:
//! same instruction set, same speed, same view of memory. That covers the
//! ESP32's two LX6s and an RP2040's two M0s.
//!
//! Asymmetric parts — an M7 beside an M0 — are deliberately out of scope. In
//! practice those run two different images rather than one kernel across both,
//! and guessing at which of the many possible splits to support would be
//! designing against a hypothetical.

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

    /// How many cores this part has running FlintOS. Not how many exist.
    fn cores() -> u8;

    /// Notify `core` that kernel scheduling work is pending there.
    ///
    /// Returns `true` when a hardware notification was sent. Architectures
    /// without an inter-core interrupt may keep the default; the pending work
    /// remains recorded and will be observed by that core's next tick.
    fn request_reschedule(_core: CoreId) -> bool {
        false
    }

    /// Identifies the calling *execution context* for lock ownership.
    ///
    /// On hardware this is the core id, and the default reflects that: one
    /// core is one context. It is separate from [`MultiCore::current_core`]
    /// because the two answer different questions — a core id indexes per-core
    /// arrays and must be below [`MAX_CORES`], while a context id only has to
    /// be *unique among things that can hold a lock at once*.
    ///
    /// They diverge on a host, where threads stand in for cores: there can be
    /// more threads than cores, and two of them sharing an id would look to a
    /// spinlock like one core locking twice.
    fn context_id() -> u8 {
        Self::current_core().0
    }
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
