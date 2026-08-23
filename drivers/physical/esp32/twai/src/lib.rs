// SPDX-License-Identifier: Apache-2.0

//! TWAI (CAN 2.0): the ESP32's CAN controller, an SJA1000-style core.
//!
//! This driver covers the **self-test loopback** path: the controller
//! transmits a frame and receives its own copy, with no second node and no bus.
//!
//! # How the loopback works, without a wire or a transceiver
//!
//! Two mechanisms combine:
//!
//! - **Self-test mode** (`MODE.STM`) lets a transmission *complete* with no
//!   acknowledgement — normally another node must ACK or the frame is retried
//!   forever. A **self-reception request** (`CMD.SRR`) transmits and receives
//!   the same frame at once.
//! - The transmitted bits still have to reach the RX input. Routed through the
//!   GPIO matrix, the controller's TX **output** and RX **input** signals land
//!   on the *same pad*, so what it drives it reads back. Real analog path
//!   (pad → pad), no internal register loopback.
//!
//! So a single ESP32, one pin, tests the whole controller: bit timing, framing,
//! CRC, the TX and RX buffers.
//!
//! # Not covered
//!
//! A real bus, arbitration against another node, and acknowledgement by a
//! second controller. Those need a transceiver and a peer (issue #27). This is
//! the on-chip functional test, fixed at 125 kbit/s — the rate is immaterial
//! when a node talks only to itself.
//!
//! # Register facts
//!
//! `DR_REG_CAN_BASE` = `0x3FF6B000`, from esp-idf `soc/soc.h`. The register
//! layout is `soc/twai_struct.h` (each 8-bit SJA1000 register in the low byte
//! of a 32-bit word); the sequences are `hal/twai_ll.h`.
//!
//! | Register | Offset | Fields |
//! |---|---|---|
//! | `MODE` | `0x00` | `RM` 0, `LOM` 1, `STM` 2, `AFM` 3 |
//! | `CMD` | `0x04` | `TR` 0, `AT` 1, `RRB` 2, `CDO` 3, `SRR` 4 |
//! | `STATUS` | `0x08` | `RBS` 0, `DOS` 1, `TBS` 2, `TCS` 3, `RS` 4, `TS` 5, `ES` 6, `BS` 7 |
//! | `INT_ENA` | `0x10` | interrupt enables, all cleared |
//! | `BUS_TIMING_0` | `0x18` | `BRP` `[5:0]`, `SJW` `[7:6]` |
//! | `BUS_TIMING_1` | `0x1C` | `TSEG1` `[3:0]`, `TSEG2` `[6:4]`, `SAM` 7 |
//! | `DATA[0..13]` | `0x40..0x74` | TX/RX buffer; ACR/AMR in reset mode |
//! | `CLOCK_DIVIDER` | `0x7C` | `CD` `[2:0]`, `CO` 3, `CM` 7 (PeliCAN) |

#![no_std]

use hal::bus::BusResult;
use hal::pinmux::{PinConfig, PinMux, Signal};
use soc_esp32::{dport, poll, reg, Esp32PinMux};

const CAN_BASE: u32 = 0x3FF6_B000;

#[allow(clippy::identity_op)]
const MODE: u32 = CAN_BASE + 0x00;
const CMD: u32 = CAN_BASE + 0x04;
const STATUS: u32 = CAN_BASE + 0x08;
const INT_ENA: u32 = CAN_BASE + 0x10;
const BUS_TIMING_0: u32 = CAN_BASE + 0x18;
const BUS_TIMING_1: u32 = CAN_BASE + 0x1C;
const DATA0: u32 = CAN_BASE + 0x40;
const CLOCK_DIVIDER: u32 = CAN_BASE + 0x7C;

// MODE bits.
const MODE_RM: u32 = 1 << 0;
const MODE_LOM: u32 = 1 << 1;
const MODE_STM: u32 = 1 << 2;
const MODE_AFM: u32 = 1 << 3;

// CMD bits.
const CMD_RRB: u32 = 1 << 2;
const CMD_CDO: u32 = 1 << 3;
const CMD_SRR: u32 = 1 << 4;

// STATUS bits.
const STATUS_RBS: u32 = 1 << 0;

// CLOCK_DIVIDER: PeliCAN layout (CM, bit 7) with CLKOUT disabled (CO, bit 3).
const CDR_PELICAN_NO_CLKOUT: u32 = (1 << 7) | (1 << 3);

/// Bus timing for 125 kbit/s at the 80 MHz APB source. Encoded as the SJA1000
/// wants it: every field is its value minus one, `brp` additionally halved.
///
/// From `TWAI_TIMING_CONFIG_125KBITS`: brp 32, sjw 3, tseg1 15, tseg2 4. Bit
/// time = brp·(1+tseg1+tseg2)/80 MHz = 32·20/80 MHz = 8 µs.
const BTR0_125K: u32 = (32 / 2 - 1) | ((3 - 1) << 6); // brp=15, sjw=2 -> 0x8F
const BTR1_125K: u32 = (15 - 1) | ((4 - 1) << 4); // tseg1=14, tseg2=3 -> 0x3E

/// Poll bound for a self-reception. A 125 kbit/s standard frame is well under a
/// millisecond; this absorbs interrupts and still fails a dead controller.
const RX_SPINS: u32 = 2_000_000;

/// A standard (11-bit identifier) CAN frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub id: u16,
    pub len: u8,
    pub data: [u8; 8],
}

/// Operating mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Requires a peer to acknowledge every frame.
    Normal,
    /// Transmits without an acknowledgement, and can receive its own frame.
    SelfTest,
    /// Receives only, never acknowledges or transmits.
    ListenOnly,
}

impl Mode {
    /// The `MODE` bits for this mode, always with the single-filter bit.
    const fn mode_bits(self) -> u32 {
        MODE_AFM
            | match self {
                Mode::Normal => 0,
                Mode::SelfTest => MODE_STM,
                Mode::ListenOnly => MODE_LOM,
            }
    }
}

/// Why a self-reception failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TwaiError {
    /// No frame arrived in the RX buffer within the poll bound.
    Timeout,
}

/// Encode a standard frame into the eleven TX-buffer bytes: frame info, the two
/// identifier bytes, then up to eight data bytes.
///
/// Frame info is DLC in `[3:0]`, RTR at 6, and the extended-frame flag at 7 — 0
/// here, this is standard-format only. The 11-bit ID is left-justified: its top
/// eight bits in byte 1, its low three in bits `[7:5]` of byte 2.
fn encode(frame: &Frame) -> ([u8; 11], usize) {
    let len = core::cmp::min(frame.len as usize, 8);
    let mut buf = [0u8; 11];
    buf[0] = len as u8; // FF=0, RTR=0, DLC=len
    buf[1] = (frame.id >> 3) as u8;
    buf[2] = ((frame.id << 5) & 0xE0) as u8;
    buf[3..3 + len].copy_from_slice(&frame.data[..len]);
    (buf, 3 + len)
}

/// Decode the eleven RX-buffer bytes back into a frame.
fn decode(buf: &[u8; 11]) -> Frame {
    let len = core::cmp::min((buf[0] & 0x0F) as usize, 8);
    let id = ((buf[1] as u16) << 3) | ((buf[2] as u16) >> 5);
    let mut data = [0u8; 8];
    data[..len].copy_from_slice(&buf[3..3 + len]);
    Frame { id, len: len as u8, data }
}

/// The TWAI controller.
pub struct Twai {
    _private: (),
}

impl Twai {
    /// Bring the controller up in `mode`, with TX on `tx_pin` and RX on
    /// `rx_pin`. For an on-chip loopback pass the same pin for both and
    /// [`Mode::SelfTest`].
    ///
    /// # Safety
    /// Takes exclusive ownership of the TWAI registers and the two pads.
    pub unsafe fn new(tx_pin: u8, rx_pin: u8, mode: Mode) -> BusResult<Self> {
        // Clock and de-assert the peripheral reset.
        dport::enable(dport::ClockBit::TWAI);

        // Route TX out and RX in through the matrix. On the same pin these
        // overlap deliberately: the pad is driven by TX and read by RX, which
        // is the loopback. TX first, RX second, so the pad ends input-enabled.
        let mux = Esp32PinMux::new();
        mux.can_route(Signal::TwaiTx, tx_pin)?;
        mux.can_route(Signal::TwaiRx, rx_pin)?;
        mux.route(Signal::TwaiTx, tx_pin, PinConfig::PUSH_PULL)?;
        mux.route(Signal::TwaiRx, rx_pin, PinConfig::PUSH_PULL)?;

        // Everything below is configured in reset mode; the core only accepts
        // it there.
        write8(MODE, MODE_RM);
        // PeliCAN register layout; without it the acceptance filter and buffers
        // are laid out the older BasicCAN way and none of the offsets match.
        write8(CLOCK_DIVIDER, CDR_PELICAN_NO_CLKOUT);
        write8(BUS_TIMING_0, BTR0_125K);
        write8(BUS_TIMING_1, BTR1_125K);
        // Accept every identifier: code 0, mask all-ones. ACR0..3 at 0x40, then
        // AMR0..3 at 0x50, one byte per word.
        for i in 0..4 {
            write8(DATA0 + i * 4, 0x00);
            write8(DATA0 + (4 + i) * 4, 0xFF);
        }
        // No interrupts — this driver polls.
        write8(INT_ENA, 0);
        // Mode bits (self-test / listen-only / single filter), still in reset.
        write8(MODE, MODE_RM | mode.mode_bits());

        // Leave reset: the controller is now on the bus (or its own pad).
        write8(MODE, mode.mode_bits());

        // Clear anything reset left behind.
        write8(CMD, CMD_RRB | CMD_CDO);

        Ok(Self { _private: () })
    }

    /// Transmit `frame` as a self-reception and return the copy the controller
    /// receives. Requires [`Mode::SelfTest`].
    ///
    /// # Safety
    /// Drives the TWAI TX and RX buffers.
    pub unsafe fn self_reception(&self, frame: &Frame) -> Result<Frame, TwaiError> {
        let (buf, n) = encode(frame);
        for (i, b) in buf.iter().enumerate().take(n) {
            write8(DATA0 + (i as u32) * 4, *b as u32);
        }

        // Transmit and receive the same frame.
        write8(CMD, CMD_SRR);

        poll::until(|| unsafe { read8(STATUS) & STATUS_RBS != 0 }, RX_SPINS)
            .map_err(|_| TwaiError::Timeout)?;

        let mut rx = [0u8; 11];
        for (i, b) in rx.iter_mut().enumerate() {
            *b = read8(DATA0 + (i as u32) * 4) as u8;
        }
        // Release the RX buffer for the next frame.
        write8(CMD, CMD_RRB);

        Ok(decode(&rx))
    }
}

// Address-based adapters over `soc_esp32::reg`. The SJA1000 registers are 8-bit
// in the low byte of a 32-bit word, hence the mask.
unsafe fn write8(addr: u32, val: u32) {
    reg::write(addr as *mut u32, val & 0xFF);
}

unsafe fn read8(addr: u32) -> u32 {
    reg::read(addr as *mut u32) & 0xFF
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_addresses_match_the_struct() {
        assert_eq!(MODE, 0x3FF6_B000);
        assert_eq!(CMD, 0x3FF6_B004);
        assert_eq!(STATUS, 0x3FF6_B008);
        assert_eq!(BUS_TIMING_0, 0x3FF6_B018);
        assert_eq!(BUS_TIMING_1, 0x3FF6_B01C);
        assert_eq!(DATA0, 0x3FF6_B040);
        assert_eq!(CLOCK_DIVIDER, 0x3FF6_B07C);
    }

    #[test]
    fn the_125k_timing_encodes_value_minus_one() {
        // brp 32 -> 32/2-1 = 15; sjw 3 -> 2 at `[7:6]`; so 0x8F.
        assert_eq!(BTR0_125K, 0x8F);
        // tseg1 15 -> 14; tseg2 4 -> 3 at `[6:4]`; sam 0; so 0x3E.
        assert_eq!(BTR1_125K, 0x3E);
    }

    #[test]
    fn pelican_mode_is_bit_seven_and_clkout_is_off() {
        // Getting CM wrong leaves the core in BasicCAN, where the buffer and
        // filter live at different offsets and every frame is malformed.
        assert_eq!(CDR_PELICAN_NO_CLKOUT, 0x88);
    }

    #[test]
    fn self_test_mode_sets_stm_and_the_single_filter_bit() {
        assert_eq!(Mode::SelfTest.mode_bits(), MODE_STM | MODE_AFM);
        assert_eq!(Mode::Normal.mode_bits(), MODE_AFM);
        assert_eq!(Mode::ListenOnly.mode_bits(), MODE_LOM | MODE_AFM);
        assert_eq!(MODE_STM, 1 << 2);
    }

    #[test]
    fn a_standard_frame_round_trips_through_the_buffer_format() {
        // The identifier is left-justified: top eight bits in byte 1, low three
        // in bits `[7:5]` of byte 2. A right-justified encoding would move every
        // ID by three bits and still look like a valid frame.
        let f = Frame { id: 0x2AB, len: 5, data: [0x11, 0x22, 0x33, 0x44, 0x55, 0, 0, 0] };
        let (buf, n) = encode(&f);
        assert_eq!(n, 8);
        assert_eq!(buf[0] & 0x0F, 5); // DLC
        assert_eq!(buf[1], (0x2AB >> 3) as u8);
        assert_eq!(buf[2] & 0x1F, 0, "the low five bits of byte 2 are not the ID");
        assert_eq!(decode(&buf), f);
    }

    #[test]
    fn the_max_length_frame_does_not_run_off_the_buffer() {
        let f = Frame { id: 0x7FF, len: 8, data: [1, 2, 3, 4, 5, 6, 7, 8] };
        let (buf, n) = encode(&f);
        assert_eq!(n, 11);
        assert_eq!(decode(&buf), f);
        // A DLC claiming more than 8 must be clamped, not read past the buffer.
        let mut over = buf;
        over[0] = 0x0F; // DLC 15
        assert_eq!(decode(&over).len, 8);
    }

    #[test]
    fn the_command_bits_are_distinct() {
        assert_eq!(CMD_SRR, 1 << 4);
        assert_eq!(CMD_RRB, 1 << 2);
        assert_eq!(CMD_CDO, 1 << 3);
        assert_eq!(STATUS_RBS, 1 << 0);
    }
}
