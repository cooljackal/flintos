// SPDX-License-Identifier: Apache-2.0

//! Read-modify-write on a memory-mapped register, spelled once.
//!
//! Sixteen sites across six files open-coded
//!
//! ```ignore
//! r.write_volatile(r.read_volatile() | bits);
//! r.write_volatile(r.read_volatile() & !bits);
//! ```
//!
//! Individually trivial. Together they are the pattern in this codebase most
//! likely to be mistyped, and the typo is nasty: `& bits` where `& !bits` was
//! meant clears every *other* bit in the register. On a config register that
//! presents as a peripheral that stopped working, several layers away from the
//! line that did it, and the compiler has no opinion.
//!
//! # Not for DPORT
//!
//! [`crate::dport`] has its own [`crate::dport::modify`], and these must not be
//! used in its place. A DPORT read needs the erratum workaround — an APB
//! pre-read with interrupts masked — and the cross-core lock. Plain volatile
//! access to that block is the bug #56 was about.
//!
//! # Not a `hal` trait
//!
//! A trait would drag `hal` into pointer arithmetic it has no reason to know
//! about, and every chip's registers are its own. Free functions in the SoC
//! crate keep this where the chip already is.

/// The address of `offset` within a peripheral's register block.
#[inline(always)]
pub const fn at(base: u32, offset: u32) -> *mut u32 {
    (base + offset) as *mut u32
}

/// Read a register.
///
/// # Safety
/// `r` must be a valid, aligned, memory-mapped register.
#[inline(always)]
pub unsafe fn read(r: *mut u32) -> u32 {
    r.read_volatile()
}

/// Write a register.
///
/// # Safety
/// As [`read`].
#[inline(always)]
pub unsafe fn write(r: *mut u32, val: u32) {
    r.write_volatile(val);
}

/// Set `bits`, leaving the rest alone.
///
/// # Safety
/// `r` must be a valid, aligned, memory-mapped register.
#[inline(always)]
pub unsafe fn set(r: *mut u32, bits: u32) {
    r.write_volatile(r.read_volatile() | bits);
}

/// Clear `bits`, leaving the rest alone.
///
/// # Safety
/// As [`set`].
#[inline(always)]
pub unsafe fn clear(r: *mut u32, bits: u32) {
    r.write_volatile(r.read_volatile() & !bits);
}

/// Clear `mask`, then set `value` — a field update in one read-modify-write.
///
/// `value` is used as given, so it must already be shifted into the field.
/// Clearing first is what makes this idempotent: `|` alone can only ever add
/// bits, so a field would accumulate every value it had ever held.
///
/// # Safety
/// As [`set`].
#[inline(always)]
pub unsafe fn modify(r: *mut u32, mask: u32, value: u32) {
    r.write_volatile((r.read_volatile() & !mask) | value);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in register. The helpers take a raw pointer, so a local is a
    /// perfectly good target on a host — which is the point of testing the
    /// arithmetic here rather than against silicon.
    fn cell(initial: u32) -> u32 {
        initial
    }

    #[test]
    fn set_adds_bits_without_disturbing_the_others() {
        let mut v = cell(0b1001);
        unsafe { set(&mut v as *mut u32, 0b0010) };
        assert_eq!(v, 0b1011);
    }

    #[test]
    fn clear_removes_bits_without_disturbing_the_others() {
        // The typo this module exists to prevent: `& bits` instead of
        // `& !bits` would leave 0b0001 here rather than 0b1010.
        let mut v = cell(0b1011);
        unsafe { clear(&mut v as *mut u32, 0b0001) };
        assert_eq!(v, 0b1010);
    }

    #[test]
    fn clearing_a_bit_that_is_not_set_changes_nothing() {
        let mut v = cell(0xF0F0_0F0F);
        unsafe { clear(&mut v as *mut u32, 0x0000_0000) };
        assert_eq!(v, 0xF0F0_0F0F);
    }

    #[test]
    fn modify_replaces_a_field_rather_than_accumulating_into_it() {
        // Three writes to the same field. With `|` alone the value would be
        // the union of all three, which is the bug `modify` exists to avoid.
        let mask = 0b1110;
        let mut v = cell(0b0001);
        unsafe { modify(&mut v as *mut u32, mask, 0b0010) };
        assert_eq!(v, 0b0011);
        unsafe { modify(&mut v as *mut u32, mask, 0b0100) };
        assert_eq!(v, 0b0101, "the previous field value survived");
        unsafe { modify(&mut v as *mut u32, mask, 0b0000) };
        assert_eq!(v, 0b0001, "the field could not be cleared");
    }

    #[test]
    fn modify_leaves_bits_outside_the_mask_alone() {
        let mut v = cell(0xDEAD_BEEF);
        unsafe { modify(&mut v as *mut u32, 0x0000_00FF, 0x0000_0042) };
        assert_eq!(v, 0xDEAD_BE42);
    }

    #[test]
    fn at_offsets_from_the_base() {
        assert_eq!(at(0x3FF4_4000, 0x24) as u32, 0x3FF4_4024);
        assert_eq!(at(0x3FF4_4000, 0) as u32, 0x3FF4_4000);
    }
}
