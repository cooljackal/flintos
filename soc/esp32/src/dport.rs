// SPDX-License-Identifier: Apache-2.0

//! DPORT: peripheral clock gating, reset, and safe access to the block.
//!
//! Most ESP32 peripherals come out of reset clock-gated *off* and held in
//! reset. Every register access to such a peripheral reads as zero and writes
//! nowhere, with no fault — so a driver that forgets this looks like a driver
//! with a wrong register map, and behaves identically to one.
//!
//! Bit positions confirmed against esp-idf `soc/dport_reg.h`.
//!
//! # Two hazards, and they are not the same one
//!
//! DPORT is shared by both cores, which makes it unsafe in two independent
//! ways. Fixing either alone leaves a real failure in place.
//!
//! **1. A silicon erratum on reads.** When one CPU reads a DPORT register
//! while the other accesses APB, the DPORT read can return the APB value.
//! Nothing faults; the caller simply gets the wrong number. Espressif's
//! workaround is not a lock — it is to read *any* APB register immediately
//! before the DPORT read, with the two loads adjacent and interrupts masked.
//! The APB pre-read synchronises the two CPUs' view of the bus.
//!
//! Verified against esp-idf `soc/esp32/dport_access.c` (v5.1) and
//! `esp_hw_support/port/esp32/dport_access.c` (v4.4), which agree
//! instruction for instruction. Note what the same header says about writes:
//!
//! > Write value to DPORT register (does not require protecting)
//!
//! So [`write`] is a plain store. Only reads need the dance.
//!
//! **2. An ordinary read-modify-write race.** Two cores each setting a
//! different bit in `PERIP_CLK_EN` can lose one of them — the second read
//! happens before the first write lands. The erratum workaround does nothing
//! about this, because each read is individually correct. [`modify`] and the
//! clock-gate helpers take a lock across the whole sequence.
//!
//! Before both cores ran, neither hazard was reachable and the second was
//! documented as a caller's problem. Both are live now.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::addr::{DPORT_BASE, LEDC_BASE, RMT_BASE, I2C0_BASE, I2C1_BASE, SPI2_BASE, SPI3_BASE, UART0_BASE, UART1_BASE, UART2_BASE};

/// `DPORT_PERIP_CLK_EN_REG`. Public so the on-target self-tests have a real
/// DPORT register to read — the erratum workaround can only be checked against
/// hardware, and a test that invents an address checks nothing.
pub const PERIP_CLK_EN: u32 = DPORT_BASE + 0xC0;
/// `DPORT_PERIP_RST_EN_REG`.
pub const PERIP_RST_EN: u32 = DPORT_BASE + 0xC4;

/// `DPORT_PERI_CLK_EN_REG`. The crypto blocks — AES, SHA, RSA — are *not*
/// gated by `PERIP_CLK_EN` like every other peripheral; they share this
/// separate register (and `PERI_RST_EN` below). Confirmed against esp-idf
/// `soc/esp32/include/soc/dport_reg.h` and `hal/esp32/clk_gate_ll.h`.
pub const PERI_CLK_EN: u32 = DPORT_BASE + 0x1C;
/// `DPORT_PERI_RST_EN_REG`.
pub const PERI_RST_EN: u32 = DPORT_BASE + 0x20;

/// The APB register read immediately before every DPORT read.
///
/// Any readable APB register would serve — the load is never used, and only
/// its effect on the bus matters. This is UART0's `DATE` register, which is
/// what esp-idf uses (as the bare literal `0x3ff40078`) and which is
/// read-only, always powered, and never gated.
// Used by the asm below on the chip, and by a test that pins the address.
// Neither exists in a plain host build, which is a configuration that only
// exists so the rest of this crate can be tested.
#[cfg_attr(not(target_arch = "xtensa"), allow(dead_code))]
const APB_PREREAD: u32 = UART0_BASE + 0x78;

/// Interrupt level masked around a DPORT read and around a locked sequence.
///
/// 5 matches esp-idf's default (`CONFIG_ESP32_DPORT_DIS_INTERRUPT_LVL`), and
/// is deliberately higher than this kernel's own critical section, which masks
/// level 1. The erratum needs the two loads adjacent: *any* interrupt taken
/// between them breaks the workaround, including one at a level our critical
/// section leaves open.
#[cfg(target_arch = "xtensa")]
const MASK_LEVEL: u32 = 5;

/// Clock-enable / reset bit for a peripheral, in `DPORT_PERIP_CLK_EN_REG` and
/// `DPORT_PERIP_RST_EN_REG`. The same bit position serves both registers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClockBit(u32);

impl ClockBit {
    pub const UART0: Self = Self(1 << 2);
    pub const UART1: Self = Self(1 << 5);
    pub const UART2: Self = Self(1 << 23);
    pub const SPI2: Self = Self(1 << 6);
    pub const SPI3: Self = Self(1 << 16);
    pub const I2C0: Self = Self(1 << 7);
    pub const I2C1: Self = Self(1 << 18);
    pub const RMT: Self = Self(1 << 9);
    /// `DPORT_LEDC_CLK_EN`.
    pub const LEDC: Self = Self(1 << 11);
    /// `DPORT_TWAI_CLK_EN` (a.k.a. `DPORT_CAN_CLK_EN`).
    pub const TWAI: Self = Self(1 << 19);
    /// `DPORT_I2S0_CLK_EN`.
    pub const I2S0: Self = Self(1 << 4);
    /// `DPORT_PCNT_CLK_EN`.
    pub const PCNT: Self = Self(1 << 10);
    /// `DPORT_PWM0_CLK_EN` — the first MCPWM unit.
    pub const PWM0: Self = Self(1 << 17);

    pub const fn mask(self) -> u32 {
        self.0
    }
}

/// The clock/reset bit for a peripheral base address.
pub fn clock_bit(base: u32) -> Option<ClockBit> {
    Some(match base {
        UART0_BASE => ClockBit::UART0,
        UART1_BASE => ClockBit::UART1,
        UART2_BASE => ClockBit::UART2,
        SPI2_BASE => ClockBit::SPI2,
        SPI3_BASE => ClockBit::SPI3,
        I2C0_BASE => ClockBit::I2C0,
        I2C1_BASE => ClockBit::I2C1,
        RMT_BASE => ClockBit::RMT,
        LEDC_BASE => ClockBit::LEDC,
        _ => return None,
    })
}

// ── Erratum-safe access ─────────────────────────────────────────────────────

/// Read a DPORT register, with the erratum workaround.
///
/// Use this for **every** DPORT read. A plain `read_volatile` on this block is
/// correct almost always, which is what makes the bug so unpleasant: it fails
/// only when the other core happens to touch APB in the same few cycles, so it
/// is rare, load-dependent, and reproduces on nobody's desk.
///
/// # Safety
/// `reg` must be a valid DPORT register address.
#[inline(always)]
pub unsafe fn read(reg: u32) -> u32 {
    #[cfg(target_arch = "xtensa")]
    {
        let value: u32;
        let saved_ps: u32;
        let _apb: u32;
        // One asm block, because the guarantee is that the two loads are
        // adjacent. Splitting this into Rust volatile reads keeps their order
        // but lets the compiler schedule anything it likes between them.
        //
        // The APB address arrives in a register rather than via `movi`: a
        // 32-bit `movi` expands to a literal-pool load, and this crate's code
        // can end up in sections where the pool placement is its own problem.
        core::arch::asm!(
            "rsil {saved}, {lvl}",
            "l32i {apb}, {apb_addr}, 0",
            "l32i {val}, {reg}, 0",
            "wsr.ps {saved}",
            "rsync",
            saved = out(reg) saved_ps,
            apb = out(reg) _apb,
            val = out(reg) value,
            reg = in(reg) reg,
            apb_addr = in(reg) APB_PREREAD,
            lvl = const MASK_LEVEL,
            options(nostack),
        );
        let _ = saved_ps;
        value
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        // Host builds compile this but never execute it — the tests exercise
        // the pure logic, not the register block.
        (reg as *const u32).read_volatile()
    }
}

/// Write a DPORT register.
///
/// Plain store: esp-idf's `DPORT_REG_WRITE` is `_DPORT_REG_WRITE`, documented
/// as not requiring protection. This exists so that call sites read
/// symmetrically with [`read`], and so the one place that could change is one
/// place.
///
/// # Safety
/// `reg` must be a valid DPORT register address, and `value` meaningful for it.
#[inline(always)]
pub unsafe fn write(reg: u32, value: u32) {
    (reg as *mut u32).write_volatile(value);
}

// ── The cross-core lock ─────────────────────────────────────────────────────

/// Held across a read-modify-write sequence so the two cores cannot lose each
/// other's bits.
static LOCK: AtomicBool = AtomicBool::new(false);

/// Mask interrupts on this core, returning the previous `PS`.
#[inline(always)]
unsafe fn mask_interrupts() -> u32 {
    #[cfg(target_arch = "xtensa")]
    {
        let saved: u32;
        core::arch::asm!("rsil {0}, {1}", out(reg) saved, const MASK_LEVEL);
        saved
    }
    #[cfg(not(target_arch = "xtensa"))]
    {
        0
    }
}

/// Restore `PS` saved by [`mask_interrupts`].
#[inline(always)]
unsafe fn restore_interrupts(saved: u32) {
    #[cfg(target_arch = "xtensa")]
    core::arch::asm!("wsr.ps {0}", "rsync", in(reg) saved);
    #[cfg(not(target_arch = "xtensa"))]
    let _ = saved;
}

/// Run `f` with interrupts masked and the DPORT lock held.
///
/// Interrupts first, then the lock — the ordering rule this kernel uses
/// everywhere. The other way round deadlocks: hold the lock, take an interrupt
/// on the same core, have the handler want the lock.
///
/// Not reentrant, and does not detect reentry. Nothing inside `f` may call
/// [`modify`], [`enable`] or [`disable`]; use [`read`] and [`write`] directly.
/// The critical sections here are a handful of instructions, so the plain
/// test-and-set cannot spin long: the only possible contender is the other
/// core, which has its own interrupts masked and so must finish.
///
/// # Safety
/// `f` must not panic — the lock would stay held. In a `panic = "abort"`
/// kernel that is academic, but the constraint is real.
#[inline(always)]
unsafe fn locked<R>(f: impl FnOnce() -> R) -> R {
    let saved = mask_interrupts();
    while LOCK
        .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        core::hint::spin_loop();
    }
    let result = f();
    LOCK.store(false, Ordering::Release);
    restore_interrupts(saved);
    result
}

/// The value a masked read-modify-write should write.
///
/// Split out so the arithmetic is testable without a register block: on a host
/// there is nothing at `0x3FF000C0` to read.
#[inline(always)]
const fn apply(old: u32, clear: u32, set: u32) -> u32 {
    (old & !clear) | set
}

/// Read-modify-write one DPORT register, atomically against the other core.
///
/// # Safety
/// `reg` must be a valid DPORT register address. Must not be called from
/// inside another `modify`/[`enable`]/[`disable`].
#[inline(always)]
pub unsafe fn modify(reg: u32, clear: u32, set: u32) {
    locked(|| {
        let old = read(reg);
        write(reg, apply(old, clear, set));
    })
}

// ── Clock gating ────────────────────────────────────────────────────────────

/// Enable a peripheral's clock and release it from reset.
///
/// Call this before touching any of the peripheral's registers.
///
/// Both registers are updated under one lock acquisition. Taking the lock
/// twice would let the other core see a peripheral clocked but still held in
/// reset, which is the one intermediate state a caller must never observe.
///
/// # Safety
/// Touches shared DPORT state. Safe against the other core and against
/// interrupts; still requires that the peripheral is yours to gate.
pub unsafe fn enable(bit: ClockBit) {
    locked(|| {
        let clk = read(PERIP_CLK_EN);
        write(PERIP_CLK_EN, apply(clk, 0, bit.mask()));
        let rst = read(PERIP_RST_EN);
        write(PERIP_RST_EN, apply(rst, bit.mask(), 0));
    })
}

/// Gate a peripheral's clock off and hold it in reset.
///
/// # Safety
/// Same as [`enable`].
pub unsafe fn disable(bit: ClockBit) {
    locked(|| {
        let rst = read(PERIP_RST_EN);
        write(PERIP_RST_EN, apply(rst, 0, bit.mask()));
        let clk = read(PERIP_CLK_EN);
        write(PERIP_CLK_EN, apply(clk, bit.mask(), 0));
    })
}

// ── The crypto blocks' clock gate ───────────────────────────────────────────
//
// AES, SHA and RSA sit on `PERI_CLK_EN`/`PERI_RST_EN`, not the `PERIP_*` pair
// the rest of the chip uses. Their bit assignment is its own, so this is a
// separate `CryptoClockBit` rather than more `ClockBit` variants — mixing the
// two would let a caller write an AES bit into the wrong register and gate the
// wrong peripheral. Bits from esp-idf `soc/esp32/include/soc/dport_reg.h`
// (`DPORT_PERI_EN_AES` = bit 0, `DPORT_PERI_EN_SHA` = bit 1).

/// A clock-enable / reset bit in `DPORT_PERI_CLK_EN_REG` / `DPORT_PERI_RST_EN_REG`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CryptoClockBit(u32);

impl CryptoClockBit {
    /// `DPORT_PERI_EN_AES`.
    pub const AES: Self = Self(1 << 0);
    /// `DPORT_PERI_EN_SHA`.
    pub const SHA: Self = Self(1 << 1);

    pub const fn mask(self) -> u32 {
        self.0
    }

    /// Reset bits to release when enabling this block. The crypto units share
    /// reset lines, so releasing a block's own bit is not enough: SHA is held
    /// in reset unless `SECUREBOOT` (bit 3) is also released, and AES needs
    /// `SECUREBOOT` and `DIGITAL_SIGNATURE` (bit 4) too. From esp-idf
    /// `clk_gate_ll.h` `periph_ll_get_rst_en_mask(enable=true)`. Only the
    /// block's own bit is *re-asserted* on disable, so a shared unit is never
    /// pulled back into reset under a peer that is still running.
    const fn reset_release_mask(self) -> u32 {
        const SECUREBOOT: u32 = 1 << 3;
        const DIGITAL_SIGNATURE: u32 = 1 << 4;
        if self.0 == 1 << 0 {
            (1 << 0) | SECUREBOOT | DIGITAL_SIGNATURE // AES
        } else if self.0 == 1 << 1 {
            (1 << 1) | SECUREBOOT // SHA
        } else {
            self.0
        }
    }
}

/// Enable a crypto block's clock and release it from reset.
///
/// The reset polarity here is the reverse of [`enable`]: a set bit in
/// `PERI_RST_EN` holds the block in reset, so releasing it *clears* the bit.
/// Same one-lock invariant as [`enable`] — clock and reset move together so no
/// core sees the block clocked but still reset.
///
/// # Safety
/// Touches shared DPORT state. Safe against the other core and interrupts;
/// still requires that the block is yours to gate.
pub unsafe fn enable_crypto(bit: CryptoClockBit) {
    locked(|| {
        let clk = read(PERI_CLK_EN);
        write(PERI_CLK_EN, apply(clk, 0, bit.mask()));
        let rst = read(PERI_RST_EN);
        write(PERI_RST_EN, apply(rst, bit.reset_release_mask(), 0));
    })
}

/// Gate a crypto block's clock off and hold it in reset.
///
/// # Safety
/// Same as [`enable_crypto`].
pub unsafe fn disable_crypto(bit: CryptoClockBit) {
    locked(|| {
        let rst = read(PERI_RST_EN);
        write(PERI_RST_EN, apply(rst, 0, bit.mask()));
        let clk = read(PERI_CLK_EN);
        write(PERI_CLK_EN, apply(clk, bit.mask(), 0));
    })
}

// ── Tests ───────────────────────────────────────────────────────────────────

// ── The radio's clock gate ──────────────────────────────────────────────────
//
// The Wi-Fi and Bluetooth blocks are not gated by `DPORT_PERIP_CLK_EN` like
// every other peripheral. They have their own register, their own bit
// assignment, and a *shared* group of bits that both radios need -- which is
// why this is a separate pair of functions rather than more `ClockBit`
// variants. A refcount over one bitmask would get the sharing wrong.
//
// Values from esp-idf `components/soc/esp32/include/soc/dport_reg.h` at tag
// v4.4, read rather than recalled:
//
//     DPORT_WIFI_CLK_EN_REG            DPORT + 0x0CC
//     DPORT_WIFI_CLK_WIFI_BT_COMMON_M  0x000003c9   bits 0, 3, 6, 7, 8, 9
//     DPORT_WIFI_CLK_WIFI_EN           0x00000406   bits 1, 2, 10
//     DPORT_WIFI_CLK_BT_EN             0x61 << 11   bits 11, 16, 17

/// `DPORT_WIFI_CLK_EN_REG`.
const WIFI_CLK_EN: u32 = DPORT_BASE + 0x0CC;

/// Clocks both radios need. Enabling either radio needs these on, and only
/// the last one out may turn them off -- which is the caller's business,
/// since this crate has no idea who else is using the radio.
pub const RADIO_CLK_COMMON: u32 = 0x0000_03C9;

/// Clocks only Wi-Fi needs. Bits 1, 2 and 10.
pub const RADIO_CLK_WIFI: u32 = 0x0000_0406;

/// Clocks only Bluetooth needs. Bits 11, 16 and 17.
pub const RADIO_CLK_BT: u32 = 0x61 << 11;

/// Turn on the radio clocks in `mask`.
///
/// Read-modify-write through [`read`]/[`write`], so it inherits the DPORT
/// erratum workaround and the lock: the PHY is brought up from a task while
/// the other core may be doing anything at all, and a lost bit here is a
/// radio block that never gets a clock.
///
/// # Safety
/// Writes DPORT. `mask` must be a combination of the `RADIO_CLK_*` constants.
pub unsafe fn radio_clock_enable(mask: u32) {
    unsafe { modify(WIFI_CLK_EN, 0, mask) }
}

/// Turn off the radio clocks in `mask`.
///
/// **Not the inverse of [`radio_clock_enable`] in practice.** The common bits
/// are shared, so clearing them while the other radio is up stops it dead.
/// Nothing here tracks that; the caller owns the decision, and today the only
/// caller refcounts it.
///
/// # Safety
/// Writes DPORT. `mask` must be a combination of the `RADIO_CLK_*` constants.
pub unsafe fn radio_clock_disable(mask: u32) {
    unsafe { modify(WIFI_CLK_EN, mask, 0) }
}

/// `DPORT_CORE_RST_EN_REG`. The radio blocks' resets, separate from
/// [`PERIP_RST_EN`] — the Wi-Fi and BT MACs are not on the peripheral bus.
const CORE_RST_EN: u32 = DPORT_BASE + 0x0D0;

/// `DPORT_MAC_RST`, bit 2 of [`CORE_RST_EN`]. Spelled `DPORT_WIFIMAC_RST` in
/// the same header, at the same bit — two names for one thing, and esp-idf
/// uses the first here.
const WIFI_MAC_RST: u32 = 1 << 2;

/// Pulse the Wi-Fi MAC's reset.
///
/// esp-idf's `wifi_reset_mac_wrapper`, which is a set followed immediately by
/// a clear — the reset is edge-driven and leaving the bit asserted would hold
/// the MAC down rather than restart it.
///
/// Deliberately *not* `periph_module_reset(PERIPH_WIFI_MODULE)`, which is what
/// NuttX calls: on the ESP32 that lands in `periph_ll_get_rst_en_mask`'s
/// `default` arm and returns a mask of zero, so it sets and clears nothing.
/// The two references disagree, and this follows esp-idf, whose version is the
/// one the blob was built and tested against.
///
/// # Safety
/// Writes DPORT and resets a peripheral. The Wi-Fi MAC must not be mid-frame,
/// which is the caller's business — the blob calls this while its own driver
/// is stopped.
pub unsafe fn wifi_mac_reset() {
    unsafe {
        modify(CORE_RST_EN, 0, WIFI_MAC_RST);
        modify(CORE_RST_EN, WIFI_MAC_RST, 0);
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_radio_clock_masks_are_esp_idfs() {
        // From dport_reg.h at v4.4. A wrong bit here is a radio block with no
        // clock, which presents as the PHY failing to initialise rather than
        // as anything pointing at a clock gate.
        assert_eq!(WIFI_CLK_EN, 0x3FF0_00CC);
        assert_eq!(RADIO_CLK_COMMON, 0x0000_03C9);
        assert_eq!(RADIO_CLK_WIFI, 0x0000_0406);
        assert_eq!(RADIO_CLK_BT, 0x0003_0800);
    }

    #[test]
    fn wifi_and_bt_own_no_bits_in_common() {
        // The shared bits are `RADIO_CLK_COMMON` and nothing else. If the
        // per-radio masks overlapped, disabling one would silently take a
        // clock away from the other.
        assert_eq!(RADIO_CLK_WIFI & RADIO_CLK_BT, 0);
        assert_eq!(RADIO_CLK_WIFI & RADIO_CLK_COMMON, 0);
        assert_eq!(RADIO_CLK_BT & RADIO_CLK_COMMON, 0);
    }

    #[test]
    fn the_radio_clock_register_is_not_the_peripheral_one() {
        // Wi-Fi and BT are gated separately from every other peripheral.
        // Writing the radio masks into PERIP_CLK_EN would gate six unrelated
        // peripherals instead.
        assert_ne!(WIFI_CLK_EN, PERIP_CLK_EN);
    }

    #[test]
    fn the_mac_reset_is_its_own_register_and_bit() {
        // From dport_reg.h at v4.4:
        //
        //     #define DPORT_CORE_RST_EN_REG   (DR_REG_DPORT_BASE + 0x0D0)
        //     #define DPORT_MAC_RST           (BIT(2))
        //
        // Bit 2 of PERIP_RST_EN is UART0. Landing this pulse in the wrong
        // register would reset the console mid-log, which is a symptom that
        // points nowhere near the radio.
        assert_eq!(CORE_RST_EN, 0x3FF0_00D0);
        assert_eq!(WIFI_MAC_RST, 0x0000_0004);
        assert_ne!(CORE_RST_EN, PERIP_RST_EN);
        assert_ne!(CORE_RST_EN, WIFI_CLK_EN);
    }
    use super::*;

    #[test]
    fn register_addresses_match_the_idf_map() {
        assert_eq!(PERIP_CLK_EN, 0x3FF0_00C0);
        assert_eq!(PERIP_RST_EN, 0x3FF0_00C4);
    }

    #[test]
    fn the_apb_preread_is_the_address_idf_uses() {
        // esp-idf hardcodes this as the literal `0x3ff40078`. We derive it from
        // UART0's base so the relationship is visible, which is only an
        // improvement if the arithmetic actually lands on the same address.
        assert_eq!(APB_PREREAD, 0x3FF4_0078);
    }

    #[test]
    fn every_peripheral_has_a_distinct_bit() {
        let bits = [
            ClockBit::UART0,
            ClockBit::UART1,
            ClockBit::UART2,
            ClockBit::SPI2,
            ClockBit::SPI3,
            ClockBit::I2C0,
            ClockBit::I2C1,
            ClockBit::RMT,
            ClockBit::LEDC,
        ];
        for (i, a) in bits.iter().enumerate() {
            for b in &bits[i + 1..] {
                assert_ne!(a.mask(), b.mask(), "two peripherals share a clock bit");
            }
        }
    }

    #[test]
    fn base_addresses_map_to_the_right_bits() {
        assert_eq!(clock_bit(I2C0_BASE), Some(ClockBit::I2C0));
        assert_eq!(clock_bit(I2C1_BASE), Some(ClockBit::I2C1));
        assert_eq!(clock_bit(UART0_BASE), Some(ClockBit::UART0));
        assert_eq!(clock_bit(SPI3_BASE), Some(ClockBit::SPI3));
        assert_eq!(clock_bit(RMT_BASE), Some(ClockBit::RMT));
        assert_eq!(clock_bit(LEDC_BASE), Some(ClockBit::LEDC));
        // An address with no clock bit must map to None. This caught a real
        // bug: a base constant that is not imported becomes a *binding* in a
        // match arm rather than a comparison, so it matches everything and
        // every peripheral gets the last arm's clock bit.
        assert_eq!(clock_bit(0xDEAD_BEEF), None);
    }

    #[test]
    fn apply_sets_and_clears_without_touching_the_rest() {
        assert_eq!(apply(0b0000, 0, 0b0010), 0b0010);
        assert_eq!(apply(0b0110, 0b0010, 0), 0b0100);
        // Unrelated bits survive. This is the whole point of read-modify-write
        // over a blind store, and the reason the race matters.
        assert_eq!(apply(0xFFFF_0000, 0, 1), 0xFFFF_0001);
        assert_eq!(apply(0xFFFF_0001, 1, 0), 0xFFFF_0000);
    }

    #[test]
    fn clear_and_set_of_the_same_bit_ends_set() {
        // Order inside `apply` is clear-then-set. Pinning it down because
        // `enable` relies on it: a caller passing the same mask to both should
        // get a predictable answer rather than whichever the implementation
        // happened to do last.
        assert_eq!(apply(0b0000, 0b0001, 0b0001), 0b0001);
        assert_eq!(apply(0b0001, 0b0001, 0b0001), 0b0001);
    }

    /// The lock has to actually exclude. Two threads standing in for two
    /// cores, doing the read-modify-write the clock-gate helpers do, on a
    /// variable rather than a register.
    ///
    /// Without the lock this loses updates reliably — which is the bug this
    /// module now exists to prevent.
    #[test]
    fn the_lock_excludes_a_concurrent_read_modify_write() {
        use std::sync::atomic::{AtomicU32, Ordering as O};
        use std::sync::Arc;

        static SHARED: AtomicU32 = AtomicU32::new(0);
        SHARED.store(0, O::SeqCst);

        const PER_THREAD: u32 = 2000;
        let barrier = Arc::new(std::sync::Barrier::new(2));
        // Full path: this crate is `no_std`, so the prelude has no `Vec`.
        let mut handles = std::vec::Vec::new();

        for t in 0..2u32 {
            let barrier = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..PER_THREAD {
                    unsafe {
                        locked(|| {
                            // Deliberately non-atomic: load, pause, store.
                            // That is what a DPORT read-modify-write is, and
                            // the lock is the only thing making it safe.
                            let old = SHARED.load(O::Relaxed);
                            std::hint::spin_loop();
                            SHARED.store(old + 1, O::Relaxed);
                        })
                    }
                }
                let _ = t;
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            SHARED.load(O::SeqCst),
            2 * PER_THREAD,
            "the lock let an update be lost"
        );
    }

    #[test]
    fn the_lock_is_released_after_use() {
        // A lock that excludes but never releases also passes the test above,
        // right up until the second acquisition.
        unsafe {
            locked(|| {});
            locked(|| {});
            let v = locked(|| 42);
            assert_eq!(v, 42);
        }
        assert!(!LOCK.load(Ordering::SeqCst), "lock still held after use");
    }
}
