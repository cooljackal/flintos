// SPDX-License-Identifier: Apache-2.0

//! RMT: the remote-control peripheral, used here as a precise pulse generator.
//!
//! Eight channels that emit a programmed sequence of high/low durations,
//! clocked off APB through a per-channel divider. It exists for infrared
//! remote protocols, and it is also the only sane way to drive an addressable
//! LED — WS2812 and friends need sub-microsecond pulse widths held to a few
//! hundred nanoseconds, which is not something a task competing with a 1 ms
//! tick can do by toggling a pin.
//!
//! # What this does and does not do
//!
//! One shot, one channel's worth of entries, polled to completion. No
//! interrupts, no DMA, no continuous streaming. That is enough to drive the
//! onboard LED on a board like the M5Stack Atom, which is what motivated it,
//! and it is bounded by the channel's 64-entry memory block — about two
//! WS2812 LEDs. Longer strings need refill-on-interrupt, which is a different
//! and much larger job (see the follow-up issue).
//!
//! Rejecting an over-long sequence is deliberate. Silently truncating it
//! would light some LEDs and leave others at whatever they held before, which
//! looks like a wiring fault rather than a programming error.
//!
//! # Register facts, all checked against esp-idf headers
//!
//! Every constant here was read out of `soc/rmt_reg.h`, `soc/soc.h`,
//! `soc/dport_reg.h` and `soc/gpio_sig_map.h` rather than recalled. Four of
//! them would have been wrong from memory, which is why:
//!
//! - the channel FIFO is at offset **0x0000**, not 0x800
//! - `IDLE_OUT_EN` and `IDLE_OUT_LV` live in **CONF1**, not CONF0
//! - `ETS_RMT_INTR_SOURCE` is **47**, not 46
//! - `RMT_SIG_OUT0_IDX` is 87

use crate::addr::RMT_BASE;

/// APB clock feeding the RMT divider.
pub const APB_HZ: u32 = crate::APB_HZ;

/// Entries in one channel's memory block.
///
/// Each entry encodes two pulses, so a block is 128 pulses — and a WS2812 bit
/// is one entry, giving 64 bits, or two LEDs and change.
pub const ENTRIES_PER_BLOCK: usize = 64;

/// Channel FIFO write register. `RMT_CH0DATA_REG` is `DR_REG_RMT_BASE + 0x0000`
/// and the channels are 4 bytes apart.
const fn ch_data(ch: u8) -> u32 {
    RMT_BASE + 4 * ch as u32
}

/// `RMT_CHnCONF0_REG`: divider, memory blocks, carrier.
const fn ch_conf0(ch: u8) -> u32 {
    RMT_BASE + 0x20 + 8 * ch as u32
}

/// `RMT_CHnCONF1_REG`: start, resets, clock source, idle level.
const fn ch_conf1(ch: u8) -> u32 {
    RMT_BASE + 0x24 + 8 * ch as u32
}

// CONF0 fields.
const CONF0_DIV_CNT_SHIFT: u32 = 0;
const CONF0_MEM_SIZE_SHIFT: u32 = 24;
const CONF0_CARRIER_EN: u32 = 1 << 28;

// CONF1 fields.
const CONF1_TX_START: u32 = 1 << 0;
const CONF1_MEM_RD_RST: u32 = 1 << 3;
const CONF1_REF_CNT_RST: u32 = 1 << 16;
/// Clock the divider from APB rather than REF_TICK. REF_TICK is 1 MHz, far too
/// coarse for a 350 ns pulse.
const CONF1_REF_ALWAYS_ON: u32 = 1 << 17;
const CONF1_IDLE_OUT_LV: u32 = 1 << 18;
const CONF1_IDLE_OUT_EN: u32 = 1 << 19;

/// One RMT entry: two consecutive pulses, each a level and a 15-bit duration
/// in divided clock ticks.
///
/// A duration of zero is the end marker, which is why [`Rmt::transmit`] appends
/// one rather than trusting the caller to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry(pub u32);

impl Entry {
    /// Build an entry from two (level, duration) pulses.
    ///
    /// Durations are clamped to the 15-bit field rather than truncated: a
    /// wrapped duration produces a pulse of almost no length, which on a WS2812
    /// is a different bit value, so the LED lights the wrong colour and nothing
    /// reports an error.
    pub const fn new(lv0: bool, d0: u16, lv1: bool, d1: u16) -> Self {
        const MAX: u16 = 0x7FFF;
        let d0 = if d0 > MAX { MAX } else { d0 };
        let d1 = if d1 > MAX { MAX } else { d1 };
        Self(
            (d0 as u32)
                | ((lv0 as u32) << 15)
                | ((d1 as u32) << 16)
                | ((lv1 as u32) << 31),
        )
    }

    /// The terminating entry. A zero duration stops the channel.
    pub const END: Self = Self(0);
}

/// Divider giving approximately `ns` per tick, clamped to the 8-bit field.
///
/// Returns the divider and the resulting nanoseconds-per-tick, because the
/// caller needs the latter to convert its own pulse widths and recomputing it
/// from the divider is how the two drift apart.
pub const fn divider_for_ns(ns: u32) -> (u8, u32) {
    // APB is 80 MHz, so one undivided tick is 12.5 ns. Work in picoseconds to
    // keep the integer arithmetic honest at that scale.
    const PS_PER_APB_TICK: u32 = 12_500;
    let want_ps = ns * 1000;
    let mut div = want_ps / PS_PER_APB_TICK;
    if div == 0 {
        div = 1;
    }
    if div > 255 {
        div = 255;
    }
    (div as u8, (div * PS_PER_APB_TICK) / 1000)
}

/// One RMT channel, configured for transmit.
pub struct Rmt {
    ch: u8,
}

impl Rmt {
    /// Claim channel `ch` and configure it to transmit with `div` as the clock
    /// divider.
    ///
    /// # Safety
    /// Takes exclusive ownership of the channel's registers and its memory
    /// block. Two instances on one channel corrupt each other's transmissions.
    /// The caller must also have routed the channel's output signal to a pad.
    pub unsafe fn new(ch: u8, div: u8) -> Option<Self> {
        if ch >= 8 {
            return None;
        }
        // One memory block, no carrier modulation (that is for IR, not LEDs).
        let conf0 = ((div as u32) << CONF0_DIV_CNT_SHIFT) | (1u32 << CONF0_MEM_SIZE_SHIFT);
        (ch_conf0(ch) as *mut u32).write_volatile(conf0 & !CONF0_CARRIER_EN);

        // Idle low: a WS2812 reads a long high as data, and leaving the line
        // high between frames would corrupt the next one.
        let conf1 = CONF1_REF_ALWAYS_ON | CONF1_IDLE_OUT_EN;
        (ch_conf1(ch) as *mut u32).write_volatile(conf1 & !CONF1_IDLE_OUT_LV);

        Some(Self { ch })
    }

    /// Write `entries` and start transmitting. Returns immediately.
    ///
    /// Returns `false` if the sequence does not fit the channel's memory block.
    ///
    /// **Non-blocking on purpose.** Waiting would mean reading `CCOUNT`, and
    /// this crate has to build on stable Rust so its lookup tables can be
    /// host-tested — Xtensa inline assembly would take that away for the sake
    /// of a busy-wait. The caller knows how long to wait and has better ways to
    /// spend the time; [`frame_ns`] computes it.
    ///
    /// Do not call again until the previous frame has finished. A WS2812 reads
    /// a restart mid-frame as data and latches whatever it has.
    ///
    /// # Safety
    /// Writes the channel's registers.
    pub unsafe fn transmit(&mut self, entries: &[Entry]) -> bool {
        // One slot reserved for the terminator.
        if entries.len() >= ENTRIES_PER_BLOCK {
            return false;
        }

        // Rewind the read pointer, or the channel resumes from wherever the
        // previous transmission left it.
        let c1 = ch_conf1(self.ch) as *mut u32;
        let base = c1.read_volatile();
        c1.write_volatile(base | CONF1_MEM_RD_RST | CONF1_REF_CNT_RST);
        c1.write_volatile(base);

        let fifo = ch_data(self.ch) as *mut u32;
        for e in entries {
            fifo.write_volatile(e.0);
        }
        fifo.write_volatile(Entry::END.0);

        c1.write_volatile(base | CONF1_TX_START);
        true
    }
}

/// How long a frame of `entries` takes, in nanoseconds, at `ns_per_tick`.
///
/// Each entry holds two pulses; `total_ticks` is their summed durations. The
/// caller has that number already from building the entries, and recomputing it
/// from the packed words here would only be a second chance to get it wrong.
pub const fn frame_ns(total_ticks: u32, ns_per_tick: u32) -> u32 {
    total_ticks.saturating_mul(ns_per_tick)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_channel_fifo_is_at_offset_zero() {
        // Would have been 0x800 from memory. The CONF registers start at 0x20,
        // which is exactly where eight 4-byte FIFO registers end -- the layout
        // only makes sense this way.
        assert_eq!(ch_data(0), RMT_BASE);
        assert_eq!(ch_data(7), RMT_BASE + 0x1C);
        assert_eq!(ch_conf0(0), RMT_BASE + 0x20, "CONF0 follows the eight FIFOs");
    }

    #[test]
    fn conf_registers_are_interleaved_per_channel() {
        assert_eq!(ch_conf1(0) - ch_conf0(0), 4);
        assert_eq!(ch_conf0(1) - ch_conf0(0), 8);
        assert_eq!(ch_conf0(7), RMT_BASE + 0x20 + 56);
    }

    #[test]
    fn conf1_fields_do_not_collide() {
        // The check that would have caught IDLE_OUT_* being placed in CONF0.
        let all = [
            CONF1_TX_START,
            CONF1_MEM_RD_RST,
            CONF1_REF_CNT_RST,
            CONF1_REF_ALWAYS_ON,
            CONF1_IDLE_OUT_LV,
            CONF1_IDLE_OUT_EN,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_eq!(a & b, 0, "two CONF1 fields share a bit");
            }
        }
    }

    #[test]
    fn an_entry_packs_two_pulses_into_the_documented_layout() {
        // Level in the top bit of each half, duration in the low 15.
        let e = Entry::new(true, 0x1234, false, 0x0567);
        assert_eq!(e.0 & 0x7FFF, 0x1234);
        assert_eq!(e.0 & (1 << 15), 1 << 15, "first level");
        assert_eq!((e.0 >> 16) & 0x7FFF, 0x0567);
        assert_eq!(e.0 & (1 << 31), 0, "second level");
    }

    #[test]
    fn an_over_long_duration_clamps_rather_than_wrapping() {
        // A wrapped duration is a very short pulse, which on a WS2812 is a
        // different bit -- the LED lights the wrong colour and nothing errors.
        let e = Entry::new(true, 0xFFFF, true, 0xFFFF);
        assert_eq!(e.0 & 0x7FFF, 0x7FFF);
        assert_eq!((e.0 >> 16) & 0x7FFF, 0x7FFF);
    }

    #[test]
    fn the_end_marker_is_a_zero_duration() {
        assert_eq!(Entry::END.0, 0);
    }

    #[test]
    fn the_divider_never_returns_zero_and_clamps_to_the_field() {
        // Divider 0 would mean "no division" on some fields and a divide-by-
        // zero in the caller's own timing maths.
        let (d, _) = divider_for_ns(1);
        assert!(d >= 1);
        let (d, _) = divider_for_ns(1_000_000);
        assert_eq!(d, 255, "clamped to the 8-bit field");
    }

    #[test]
    fn a_divider_of_ten_gives_125ns_ticks() {
        // 80 MHz / 10 = 8 MHz, one tick every 125 ns. The value a WS2812
        // driver wants, since its pulse widths are multiples of ~125 ns.
        let (div, ns) = divider_for_ns(125);
        assert_eq!(div, 10);
        assert_eq!(ns, 125);
    }
}
