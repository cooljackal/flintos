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
//! Two modes.
//!
//! [`Rmt::transmit`] is one shot: write the entries, start, done. It is
//! bounded by the channel's 64-entry block — 63 usable, and a WS2812 bit is
//! one entry, so **two LEDs**. Simple, and enough for a board with a single
//! onboard LED.
//!
//! [`Rmt::start_stream`] is for anything longer. The channel wraps around its
//! block while an interrupt refills the half just played, so frame length is
//! bounded by nothing but the caller's buffer. A 5×5 panel is 600 entries and
//! needs this: `MEM_SIZE` maxed gives one channel all eight blocks, 512
//! entries, at the cost of every other channel, and 512 < 600.
//!
//! Streaming buys that at the price of a deadline. A refill has one half-block
//! — about 40 µs at WS2812 rates — before the transmitter reaches what it is
//! writing, and there is no underrun flag, so being late is silent and looks
//! like corrupt pixels. [`Refill`] holds the bookkeeping and is pure, so the
//! ping-pong can be tested on a host rather than by watching an LED.
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
//! - the channel FIFO register is at offset **0x0000**, not 0x800
//! - `IDLE_OUT_EN` and `IDLE_OUT_LV` live in **CONF1**, not CONF0
//! - `ETS_RMT_INTR_SOURCE` is **47**, not 46
//! - `RMT_SIG_OUT0_IDX` is 87
//!
//! # Two ways in, and only one of them works twice
//!
//! There are two paths to a channel's entries, and the first version of this
//! module took the wrong one. Writing `RMT_CHnDATA_REG` pushes through an APB
//! FIFO; setting `RMT_APB_FIFO_MASK` instead maps the 64-entry block into
//! memory at `RMT_BASE + 0x800` and you write it by index.
//!
//! The FIFO path transmits correctly *once*. Its write pointer is rewound by
//! `APB_MEM_RST`, which is a different bit from the `MEM_RD_RST` that rewinds
//! the read pointer, so a driver that resets only the read pointer replays its
//! first frame forever -- an LED that lights the right colour and then ignores
//! every later one. That is what happened, and the symptom is quiet enough to
//! be mistaken for the encoder.
//!
//! This uses the memory path, because esp-idf does: `rmt_ll_tx_reset_pointer`
//! touches `mem_rd_rst` alone, which is only sufficient when the writes went
//! to `RMTMEM` rather than through the FIFO. `mem_wr_rst`, despite the name,
//! rewinds the *receiver's* pointer and has nothing to do with transmitting.
//!
//! `RMTMEM = 0x3ff56800` comes from esp-idf's `esp32.peripherals.ld`, and the
//! rest from `rmt_reg.h` read literally rather than summarised.

#![no_std]

use core::sync::atomic::{AtomicBool, Ordering};

use hal::bus::BusError;
use hal::pinmux::{PinConfig, PinMux, Signal};
use soc_esp32::addr::RMT_BASE;
use soc_esp32::dport::{self, ClockBit};
use soc_esp32::{reg, Esp32PinMux};

/// APB clock feeding the RMT divider.
pub const APB_HZ: u32 = soc_esp32::APB_HZ;

/// The chip's RMT peripheral interrupt source, for `interrupt::connect`.
///
/// One source serves all eight channels (`ETS_RMT_INTR_SOURCE`); the handler
/// reads the per-channel status to tell them apart. Re-exported so a caller can
/// wire the channel to a CPU interrupt without naming the SoC's address map.
pub const IRQ_SOURCE: u8 = soc_esp32::addr::IRQ_RMT;

/// Number of channels this driver can claim.
const CHANNELS: usize = 8;

/// One claim flag per channel. [`Rmt::on_pin`] wins exactly one — the
/// `svd2rust` `Peripherals::take` pattern — which discharges the "own the
/// channel exclusively" invariant [`Rmt::new`] rests on. `core::sync::atomic`,
/// not `portable_atomic`: a physical driver may not name a crates.io crate (see
/// `tools/check-layers.sh`) and Xtensa has native atomics.
static CHANNEL_CLAIMED: [AtomicBool; CHANNELS] = [const { AtomicBool::new(false) }; CHANNELS];

/// Entries in one channel's memory block.
///
/// Each entry encodes two pulses, so a block is 128 pulses — and a WS2812 bit
/// is one entry, giving 64 bits, or two LEDs and change.
pub const ENTRIES_PER_BLOCK: usize = 64;

/// Base of the channel entry RAM. `RMTMEM` is `DR_REG_RMT_BASE + 0x800`.
///
/// Not to be confused with `RMT_CH0DATA_REG` at offset 0x0000, which is the
/// APB FIFO window onto the same storage. See the module header for why this
/// driver uses one and not the other.
const MEM_BASE: u32 = RMT_BASE + 0x800;

/// One channel's entry block: 64 entries of 4 bytes.
const fn ch_mem(ch: u8) -> u32 {
    MEM_BASE + (ENTRIES_PER_BLOCK as u32 * 4) * ch as u32
}

/// `RMT_APB_CONF_REG`. Global, not per channel.
const APB_CONF: u32 = RMT_BASE + 0xF0;

/// "Set this bit to enable RMTMEM and disable apb fifo access." Resets to 0,
/// so the FIFO is the default and this has to be set explicitly.
const APB_FIFO_MASK: u32 = 1 << 0;

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
/// Rewinds the transmitter's read pointer. The one that matters here.
const CONF1_MEM_RD_RST: u32 = 1 << 3;
/// Rewinds the APB FIFO pointer.
///
/// Deliberately unused: this driver does not touch the FIFO. It is defined
/// anyway because the bug that made the first version replay one frame forever
/// was confusing it with `MEM_RD_RST`, and a named constant asserted against
/// the header is how that stays fixed. Deleting it would leave nothing to
/// distinguish it from.
#[allow(dead_code)]
const CONF1_APB_MEM_RST: u32 = 1 << 4;
/// 0 = the transmitter owns the block. Set would hand it to the receiver.
const CONF1_MEM_OWNER: u32 = 1 << 5;
const CONF1_REF_CNT_RST: u32 = 1 << 16;
/// Clock the divider from APB rather than REF_TICK. REF_TICK is 1 MHz, far too
/// coarse for a 350 ns pulse.
const CONF1_REF_ALWAYS_ON: u32 = 1 << 17;
const CONF1_IDLE_OUT_LV: u32 = 1 << 18;
const CONF1_IDLE_OUT_EN: u32 = 1 << 19;

/// `RMT_MEM_TX_WRAP_EN`, in `APB_CONF` alongside `APB_FIFO_MASK`. Without it
/// the channel stops at the end of the block instead of wrapping to the start,
/// which is the difference between a stream and a one-shot.
const APB_MEM_TX_WRAP_EN: u32 = 1 << 1;

/// `RMT_CHn_TX_LIM_REG`: how many entries the transmitter consumes before
/// raising `TX_THR_EVENT`. 9 bits, so a full block fits comfortably.
const fn ch_tx_lim(ch: u8) -> u32 {
    RMT_BASE + 0xD0 + 4 * ch as u32
}

const INT_RAW: u32 = RMT_BASE + 0xA0;
const INT_ENA: u32 = RMT_BASE + 0xA8;
const INT_CLR: u32 = RMT_BASE + 0xAC;

/// `RMT_CHn_TX_THR_EVENT_INT`: bits 24..31, one per channel.
const fn tx_thr_bit(ch: u8) -> u32 {
    1 << (24 + ch as u32)
}

/// `RMT_CHn_TX_END_INT`. The per-channel interrupts are grouped in threes --
/// TX_END, RX_END, ERR -- so channel n's TX_END is bit 3n, not bit n.
const fn tx_end_bit(ch: u8) -> u32 {
    1 << (3 * ch as u32)
}

/// Entries in half a block. The transmitter plays one half while the other is
/// being refilled, which is what bounds the memory a stream needs to 64
/// entries however long the frame is.
pub const HALF_BLOCK: usize = ENTRIES_PER_BLOCK / 2;

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
        // high between frames would corrupt the next one. MEM_OWNER clear
        // leaves the entry block with the transmitter.
        let conf1 = CONF1_REF_ALWAYS_ON | CONF1_IDLE_OUT_EN;
        (ch_conf1(ch) as *mut u32)
            .write_volatile(conf1 & !(CONF1_IDLE_OUT_LV | CONF1_MEM_OWNER));

        // Map the entry blocks into memory instead of the APB FIFO. This
        // register is global, so it is set for every channel by whichever is
        // constructed first -- which is correct, since no channel in this
        // driver uses the FIFO, but it does mean a caller mixing this with
        // FIFO writes elsewhere would break them.
        let apb = APB_CONF as *mut u32;
        reg::set(apb, APB_FIFO_MASK);

        Some(Self { ch })
    }

    /// Claim channel `ch`, gate the RMT clock, route its output to `pin`, and
    /// configure it to transmit with `div` as the clock divider. The safe
    /// constructor.
    ///
    /// Wins the channel's claim flag (a second `on_pin` for the same channel
    /// returns [`BusError::Busy`]), gates the RMT peripheral clock — the block
    /// answers reads with garbage until it is clocked — routes
    /// `Signal::PulseOut(ch)` to the pad per `config`, and configures the channel
    /// from [`Rmt::new`]. That is what `Esp32I2c::open` does for its bus
    /// internally: the driver gates its own clock and owns its own pin, so the
    /// caller touches neither `dport` nor `PinMux`.
    ///
    /// The claim proves single ownership, so no `unsafe` at the call site;
    /// [`Rmt::new`] stays for callers doing their own clock/route bring-up (the
    /// kernel self-tests). The pad's *output enable* is left to the caller: on
    /// this chip the GPIO matrix carries it, but esp-idf also sets the GPIO
    /// direction, and whether that is load-bearing is the board's call, not this
    /// driver's.
    pub fn on_pin(ch: u8, div: u8, pin: u8, config: PinConfig) -> hal::Result<Self> {
        if ch as usize >= CHANNELS {
            return Err(BusError::InvalidConfig.into());
        }
        CHANNEL_CLAIMED[ch as usize]
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Acquire)
            .map_err(|_| BusError::Busy)?;

        // Clock the peripheral before touching any register.
        // SAFETY: the RMT is this program's to gate; `enable` is itself safe
        // against the other core and interrupts.
        unsafe { dport::enable(ClockBit::RMT) };

        let route = Esp32PinMux::new().route(Signal::PulseOut(ch), pin, config);
        // SAFETY: the claim above is exclusive, so this is the only live `Rmt`
        // on channel `ch`, and its output was just routed to the pad.
        let built = route.ok().and_then(|()| unsafe { Self::new(ch, div) });
        match built {
            Some(rmt) => Ok(rmt),
            None => {
                CHANNEL_CLAIMED[ch as usize].store(false, Ordering::Release);
                Err(BusError::InvalidConfig.into())
            }
        }
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
    /// Safe: writes only the registers and memory block of the channel this
    /// `Rmt` owns (the unsafe `new` established that ownership). The "wait for
    /// the previous frame" note is a timing contract, not a memory-safety one.
    pub fn transmit(&mut self, entries: &[Entry]) -> bool {
        // One slot reserved for the terminator.
        if entries.len() >= ENTRIES_PER_BLOCK {
            return false;
        }

        // SAFETY: every write below targets this channel's own memory block and
        // registers, which owning the `Rmt` entitles.
        unsafe {
            // Write the block by index. There is no pointer to advance and so
            // none to get out of step between frames, which is the whole reason
            // this driver does not use the FIFO.
            let mem = ch_mem(self.ch) as *mut u32;
            for (i, e) in entries.iter().enumerate() {
                mem.add(i).write_volatile(e.0);
            }
            mem.add(entries.len()).write_volatile(Entry::END.0);

            let c1 = ch_conf1(self.ch) as *mut u32;
            // Drop the one-shot bits before the read-modify-write, so a TX_START
            // left set by the previous frame cannot be re-asserted here.
            let base =
                c1.read_volatile() & !(CONF1_TX_START | CONF1_MEM_RD_RST | CONF1_REF_CNT_RST);

            // Rewind the read pointer and the divider phase, both pulsed rather
            // than left set -- held in reset, the channel emits nothing.
            // Resetting the divider is what makes the first pulse a full one; at
            // a 350 ns pulse and a +/-150 ns budget, a partial first tick is a
            // wrong bit.
            c1.write_volatile(base | CONF1_MEM_RD_RST | CONF1_REF_CNT_RST);
            c1.write_volatile(base);

            c1.write_volatile(base | CONF1_TX_START);
        }
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


/// One refill: copy `len` entries from `src` in the source into the block at
/// `dest`, and append a terminator if this is the last of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    /// Entry index within the channel's 64-entry block.
    pub dest: usize,
    /// Entry index within the caller's source sequence.
    pub src: usize,
    /// How many entries to copy. Zero is normal on the final chunk, when the
    /// source ended exactly on a half boundary and only the terminator is left.
    pub len: usize,
    /// Write [`Entry::END`] at `dest + len` after copying.
    pub terminator: bool,
}

/// Bookkeeping for a frame longer than the block it is played from.
///
/// Deliberately pure: no registers, no channel, nothing target-specific. The
/// half-block ping-pong is the part with the off-by-ones in it, and keeping it
/// separate is what lets it be tested on a host instead of by watching an LED.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Refill {
    total: usize,
    written: usize,
    next_half: usize,
    terminated: bool,
}

impl Refill {
    /// Prepare to send `total` entries.
    pub const fn new(total: usize) -> Self {
        Self {
            total,
            written: 0,
            next_half: 0,
            terminated: false,
        }
    }

    /// The next half-block to fill, or `None` once the terminator is placed.
    ///
    /// Called twice before starting -- filling both halves -- and once per
    /// threshold event after that.
    pub fn next_chunk(&mut self) -> Option<Chunk> {
        if self.terminated {
            return None;
        }
        let remaining = self.total - self.written;
        let len = if remaining < HALF_BLOCK {
            remaining
        } else {
            HALF_BLOCK
        };
        // A terminator needs a slot, so it only goes in a half that the source
        // did not fill. When the source ends exactly on the boundary this
        // yields one more chunk with len 0, which is the whole reason `len` is
        // allowed to be zero.
        let terminator = len < HALF_BLOCK;
        let chunk = Chunk {
            dest: self.next_half * HALF_BLOCK,
            src: self.written,
            len,
            terminator,
        };
        self.written += len;
        self.next_half ^= 1;
        self.terminated = terminator;
        Some(chunk)
    }

    /// Whether the terminator has been placed and nothing more needs writing.
    pub const fn finished(&self) -> bool {
        self.terminated
    }

    /// Entries handed over so far. For diagnostics; a stalled stream shows up
    /// as this not advancing.
    pub const fn written(&self) -> usize {
        self.written
    }
}

impl Rmt {
    /// Begin a transmission of any length, refilled half a block at a time.
    ///
    /// Fills both halves, arms the threshold interrupt and starts the channel.
    /// The caller must keep `entries` alive and unchanged until the stream
    /// finishes, and must call [`Rmt::service`] on every `TX_THR_EVENT`.
    ///
    /// # The deadline this creates
    ///
    /// A refill has until the transmitter finishes the other half. At WS2812
    /// rates a half block is 32 bits, about **40 µs**. Miss it and the channel
    /// plays the stale half as data — on an LED string, a burst of wrong
    /// colours partway along. There is no hardware underrun flag to notice it
    /// with, so a late refill is silent.
    ///
    /// Safe: writes this channel's registers and memory block, plus the global
    /// `APB_CONF` bit RMT shares (as `new` already does). Owning the `Rmt` is
    /// the proof; the "keep `entries` alive until it finishes" note is a timing
    /// contract, not a memory-safety one.
    pub fn start_stream(&mut self, entries: &[Entry]) -> Refill {
        let mut refill = Refill::new(entries.len());
        // SAFETY: every access below is to this channel's own registers and
        // block, or the global APB_CONF bit RMT owns.
        unsafe {
            // Wrap at the end of the block rather than stopping. Global, like
            // FIFO_MASK; harmless for one-shot users because a terminator stops
            // them before the end of the block is ever reached.
            let apb = APB_CONF as *mut u32;
            reg::set(apb, APB_MEM_TX_WRAP_EN);

            // Fire the threshold once per half consumed.
            (ch_tx_lim(self.ch) as *mut u32).write_volatile(HALF_BLOCK as u32);

            // Prime both halves before starting: the first threshold does not
            // arrive until half the block is already gone.
            let mem = ch_mem(self.ch) as *mut u32;
            for _ in 0..2 {
                match refill.next_chunk() {
                    Some(c) => self.write_chunk(mem, entries, c),
                    None => break,
                }
            }

            let c1 = ch_conf1(self.ch) as *mut u32;
            let base =
                c1.read_volatile() & !(CONF1_TX_START | CONF1_MEM_RD_RST | CONF1_REF_CNT_RST);
            c1.write_volatile(base | CONF1_MEM_RD_RST | CONF1_REF_CNT_RST);
            c1.write_volatile(base);

            // Clear a stale threshold from the previous frame before enabling,
            // or the first interrupt arrives immediately and refills a half the
            // transmitter has not reached.
            (INT_CLR as *mut u32).write_volatile(tx_thr_bit(self.ch) | tx_end_bit(self.ch));
            let ena = INT_ENA as *mut u32;
            reg::set(ena, tx_thr_bit(self.ch));

            c1.write_volatile(base | CONF1_TX_START);
        }
        refill
    }

    /// Refill the half the transmitter has just finished. Returns `true` when
    /// there is nothing left to feed.
    ///
    /// **That is not the same as the transmission being over**, and treating
    /// it as such is a mistake this driver made first time out. `true` is
    /// returned on the threshold *after* the terminator was written -- and
    /// once the channel reaches a terminator it stops, so it may never consume
    /// another half block and that threshold may never arrive. Whether it does
    /// depends on how long the tail is, which makes it a race that mostly
    /// works.
    ///
    /// For "has the frame finished", ask the hardware: [`Rmt::stream_done`]
    /// reads `TX_END`, which the channel sets when it actually stops.
    ///
    /// Call from the channel's interrupt handler. Clearing the threshold flag
    /// happens here, so the handler does not have to know which bit that is.
    ///
    /// Safe: writes this channel's block and interrupt registers, which owning
    /// the `Rmt` entitles. `entries` being the same sequence passed to
    /// [`Rmt::start_stream`] is a correctness contract (a mismatched slice only
    /// plays wrong data, bounds-checked), not a memory-safety one.
    pub fn service(&mut self, entries: &[Entry], refill: &mut Refill) -> bool {
        // SAFETY: writes this channel's own interrupt and block registers.
        unsafe {
            // Clear before refilling. The other order loses an event that
            // arrives while the copy is in progress, and a lost threshold
            // stalls the stream permanently.
            (INT_CLR as *mut u32).write_volatile(tx_thr_bit(self.ch));

            match refill.next_chunk() {
                Some(c) => {
                    self.write_chunk(ch_mem(self.ch) as *mut u32, entries, c);
                    false
                }
                None => {
                    // Nothing left to feed. Stop the interrupt, or it repeats
                    // for as long as the tail plays out.
                    let ena = INT_ENA as *mut u32;
                    reg::clear(ena, tx_thr_bit(self.ch));
                    true
                }
            }
        }
    }

    /// Whether the channel has reached the terminator and stopped.
    ///
    /// This is the completion signal. `TX_END` is set by the hardware when the
    /// channel stops, and [`Rmt::start_stream`] clears it, so a set bit always
    /// refers to the frame in progress.
    ///
    /// Safe: a side-effect-free read of the interrupt status register for the
    /// channel this `Rmt` owns.
    pub fn stream_done(&self) -> bool {
        // SAFETY: a volatile read of a status register.
        unsafe { (INT_RAW as *const u32).read_volatile() & tx_end_bit(self.ch) != 0 }
    }

    /// Copy one chunk into the block.
    ///
    /// # Safety
    /// `mem` must be the channel's block and `chunk` must fit it.
    unsafe fn write_chunk(&self, mem: *mut u32, entries: &[Entry], chunk: Chunk) {
        for i in 0..chunk.len {
            mem.add(chunk.dest + i).write_volatile(entries[chunk.src + i].0);
        }
        if chunk.terminator {
            mem.add(chunk.dest + chunk.len).write_volatile(Entry::END.0);
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;


    /// Drive a `Refill` to completion, returning every chunk it asked for.
    fn drain(total: usize) -> ([Chunk; 64], usize) {
        let mut out = [Chunk { dest: 0, src: 0, len: 0, terminator: false }; 64];
        let mut r = Refill::new(total);
        let mut n = 0;
        while let Some(c) = r.next_chunk() {
            out[n] = c;
            n += 1;
            assert!(n < 64, "refill did not terminate for total={total}");
        }
        assert!(r.finished());
        (out, n)
    }

    #[test]
    fn a_stream_covers_its_source_exactly_once_and_in_order() {
        // The property that matters: every entry sent, none twice, none
        // skipped. A duplicated chunk shows on an LED string as a repeated
        // colour and a truncated tail, which is easy to misread as a bad wire.
        for total in [0, 1, 31, 32, 33, 63, 64, 65, 600] {
            let (chunks, n) = drain(total);
            let mut next_src = 0;
            for c in &chunks[..n] {
                assert_eq!(c.src, next_src, "total={total}: gap or overlap");
                next_src += c.len;
            }
            assert_eq!(next_src, total, "total={total}: source not fully sent");
        }
    }

    #[test]
    fn halves_alternate_so_a_refill_never_lands_on_the_playing_half() {
        // Writing the half currently being transmitted corrupts the frame in
        // flight. Alternating is the entire safety argument for the scheme.
        let (chunks, n) = drain(600);
        for (i, c) in chunks[..n].iter().enumerate() {
            let want = if i % 2 == 0 { 0 } else { HALF_BLOCK };
            assert_eq!(c.dest, want, "chunk {i} landed in the wrong half");
        }
    }

    #[test]
    fn every_chunk_fits_inside_the_block() {
        // Including the terminator, which is written at dest+len and is the
        // one most likely to run off the end.
        for total in [0, 1, 31, 32, 33, 64, 65, 600] {
            let (chunks, n) = drain(total);
            for c in &chunks[..n] {
                let end = c.dest + c.len + usize::from(c.terminator);
                assert!(end <= ENTRIES_PER_BLOCK, "total={total}: chunk overruns the block");
            }
        }
    }

    #[test]
    fn exactly_one_terminator_is_written_and_it_is_last() {
        for total in [0, 1, 32, 64, 600] {
            let (chunks, n) = drain(total);
            let count = chunks[..n].iter().filter(|c| c.terminator).count();
            assert_eq!(count, 1, "total={total}: needs exactly one terminator");
            assert!(chunks[n - 1].terminator, "total={total}: terminator must be last");
        }
    }

    #[test]
    fn a_source_ending_on_the_boundary_gets_an_empty_final_chunk() {
        // The case that motivates allowing len == 0. With 64 entries both
        // halves are full, so there is nowhere to put the terminator until a
        // third chunk comes back empty. Dropping that chunk would leave the
        // channel wrapping forever and the LED string held at its last colour.
        let (chunks, n) = drain(64);
        assert_eq!(n, 3);
        assert_eq!(chunks[0].len, HALF_BLOCK);
        assert_eq!(chunks[1].len, HALF_BLOCK);
        assert_eq!(chunks[2].len, 0);
        assert!(chunks[2].terminator);
        assert_eq!(chunks[2].dest, 0, "wraps back to the first half");
    }

    #[test]
    fn a_frame_that_fits_one_half_needs_no_second_chunk() {
        let (chunks, n) = drain(10);
        assert_eq!(n, 1);
        assert_eq!(chunks[0], Chunk { dest: 0, src: 0, len: 10, terminator: true });
    }

    #[test]
    fn a_five_by_five_panel_is_the_case_that_needed_streaming() {
        // 25 LEDs x 24 bits = 600 entries against a 512-entry ceiling even with
        // every block given to one channel. This is the number that made #51
        // exist, so it is pinned here rather than left in a commit message.
        const PANEL: usize = 25 * 24;
        // Compile-time, because both sides are constants and the point is the
        // relationship, not a runtime check.
        const _: () = assert!(PANEL == 600);
        const _: () = assert!(
            PANEL > ENTRIES_PER_BLOCK * 8,
            "a 5x5 panel would have fit without streaming"
        );
        // 600 = 18 full halves plus 24, so the 19th chunk is short and carries
        // the terminator itself. No extra chunk -- that only happens when the
        // source ends exactly on a boundary, which the 64-entry case covers.
        let (chunks, n) = drain(PANEL);
        assert_eq!(n, PANEL.div_ceil(HALF_BLOCK));
        assert_eq!(n, 19);
        assert_eq!(chunks[18].len, 24);
        assert!(chunks[18].terminator);
    }

    #[test]
    fn the_streaming_registers_are_where_the_header_says() {
        assert_eq!(ch_tx_lim(0), RMT_BASE + 0xD0, "RMT_CH0_TX_LIM_REG");
        assert_eq!(ch_tx_lim(1), RMT_BASE + 0xD4);
        assert_eq!(INT_RAW, RMT_BASE + 0xA0);
        assert_eq!(INT_ENA, RMT_BASE + 0xA8);
        assert_eq!(INT_CLR, RMT_BASE + 0xAC);
        assert_eq!(APB_MEM_TX_WRAP_EN, 1 << 1);
    }

    #[test]
    fn the_per_channel_interrupt_bits_do_not_alias() {
        // TX_END is bit 3n because the per-channel flags are grouped in threes
        // (TX_END, RX_END, ERR); TX_THR_EVENT is bit 24+n in the same word.
        // Using n for either would have channel 1 clearing channel 0's flag.
        assert_eq!(tx_end_bit(0), 1 << 0);
        assert_eq!(tx_end_bit(1), 1 << 3);
        assert_eq!(tx_thr_bit(0), 1 << 24);
        assert_eq!(tx_thr_bit(7), 1 << 31);
        for a in 0..8u8 {
            for b in 0..8u8 {
                if a != b {
                    assert_eq!(tx_end_bit(a) & tx_end_bit(b), 0);
                    assert_eq!(tx_thr_bit(a) & tx_thr_bit(b), 0);
                }
                assert_eq!(tx_end_bit(a) & tx_thr_bit(b), 0, "TX_END overlaps TX_THR");
            }
        }
    }

    #[test]
    fn the_entry_blocks_are_where_the_linker_script_puts_rmtmem() {
        // esp32.peripherals.ld: PROVIDE ( RMTMEM = 0x3ff56800 ). 0x800 was the
        // right number attached to the wrong thing in the first draft -- it is
        // the RAM, not the data register, and the data register is at 0x0000.
        assert_eq!(MEM_BASE, 0x3FF5_6800);
        assert_eq!(ch_mem(0), MEM_BASE);
        assert_eq!(ch_mem(1), MEM_BASE + 256, "64 entries of 4 bytes each");
        assert_eq!(ch_mem(7), MEM_BASE + 7 * 256);
    }

    #[test]
    fn the_blocks_tile_without_overlapping() {
        // A stride shorter than the block would put channel 1's entries inside
        // channel 0's, which shows up as one LED string corrupting another.
        for ch in 0..7u8 {
            let end = ch_mem(ch) + ENTRIES_PER_BLOCK as u32 * 4;
            assert_eq!(end, ch_mem(ch + 1), "channel {ch} runs into the next");
        }
    }

    #[test]
    fn the_memory_path_has_to_be_switched_on() {
        // RMT_APB_CONF_REG, quoted from rmt_reg.h: (DR_REG_RMT_BASE + 0x00f0).
        // FIFO_MASK "resets to 1'h0", so the FIFO is what you get by default
        // and writing RMTMEM without setting this goes nowhere.
        assert_eq!(APB_CONF, RMT_BASE + 0xF0);
        assert_eq!(APB_FIFO_MASK, 1);
    }

    #[test]
    fn the_two_pointer_resets_are_not_the_same_bit() {
        // The bug this driver shipped with. MEM_RD_RST rewinds the read
        // pointer; APB_MEM_RST rewinds the FIFO write pointer. Resetting only
        // the first while writing through the FIFO replays frame one forever,
        // which on an LED looks like a stuck colour rather than a fault.
        assert_ne!(CONF1_MEM_RD_RST, CONF1_APB_MEM_RST);
        assert_eq!(CONF1_MEM_RD_RST, 1 << 3);
        assert_eq!(CONF1_APB_MEM_RST, 1 << 4);
    }

    #[test]
    fn the_transmitter_keeps_the_entry_block() {
        // MEM_OWNER set hands the block to the receiver, which then overwrites
        // the entries with whatever it thinks it is sampling.
        assert_eq!(CONF1_MEM_OWNER, 1 << 5);
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
            CONF1_APB_MEM_RST,
            CONF1_MEM_OWNER,
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
