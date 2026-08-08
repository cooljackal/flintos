// SPDX-License-Identifier: Apache-2.0

#![no_std]

use hal::bus::{BusConfig, BusError, BusResult, PhysicalBus, SpiMode};
use hal::pinmux::{PinConfig, PinMux, Signal};
use soc_esp32::addr;
use soc_esp32::{dport, Esp32PinMux, APB_HZ};

/// ESP32 SPI2 (HSPI) / SPI3 (VSPI) physical driver (polled mode).
///
/// Bases: SPI2/HSPI 0x3FF64000, SPI3/VSPI 0x3FF65000.
// DMA transfers. Own file: the FIFO path above is complete in itself and the
// engine is a separate contract with separate registers, so interleaving them
// would make both harder to check against the header.
#[path = "dma.rs"]
mod dma_impl;

pub use dma_impl::{SPI_IN_SUC_EOF, SPI_OUT_EOF};

pub struct Esp32Spi {
    base: u32,
}

// ── Register map ─────────────────────────────────────────────────────────────
//
// ESP32 TRM chapter 7 (SPI Controller), register summary; offsets confirmed
// against esp-idf `soc/spi_reg.h`. A prior revision had CLOCK/USER/USER1/
// PIN/SLAVE shifted down by one register slot (CLOCK=0x0C, USER=0x10,
// USER1=0x14, PIN=0x18, SLAVE=0x1C), which is really CTRL1/RD_STATUS/CTRL2/
// CLOCK/USER -- every one of those writes landed on the wrong register.

pub(crate) const SPI_CMD: u32 = 0x00;
#[allow(dead_code)] // Not needed for the byte-oriented polled transfer this driver implements.
const SPI_ADDR: u32 = 0x04;
#[allow(dead_code)]
const SPI_CTRL: u32 = 0x08;
#[allow(dead_code)] // Not needed for the byte-oriented polled transfer this driver implements.
const SPI_CTRL1: u32 = 0x0C;
#[allow(dead_code)]
const SPI_RD_STATUS: u32 = 0x10;
#[allow(dead_code)]
const SPI_CTRL2: u32 = 0x14;
const SPI_CLOCK: u32 = 0x18;
pub(crate) const SPI_USER: u32 = 0x1C;
#[allow(dead_code)] // Superseded by MOSI_DLEN/MISO_DLEN for byte-length transfers.
const SPI_USER1: u32 = 0x20;
#[allow(dead_code)]
const SPI_USER2: u32 = 0x24;
pub(crate) const SPI_MOSI_DLEN: u32 = 0x28;
pub(crate) const SPI_MISO_DLEN: u32 = 0x2C;
const SPI_PIN: u32 = 0x34;
pub(crate) const SPI_SLAVE: u32 = 0x38;
const SPI_W0: u32 = 0x80; // Data buffer: 16 words (W0..W15), 64 bytes.

/// SPI_CMD_REG: start a user-defined transaction. bitpos [18], confirmed
/// against esp-idf `soc/spi_reg.h` (`SPI_USR`). A prior revision wrote/polled
/// bit 0, which is `SPI_DOUTDIN` (a mode bit, not the start-transaction
/// strobe) -- the poll loop could spin forever since nothing ever clears it.
pub(crate) const SPI_CMD_USR: u32 = 1 << 18;

/// SPI_USER_REG bits (bitpos confirmed against esp-idf `soc/spi_reg.h`).
pub(crate) const SPI_USR_MISO: u32 = 1 << 28;
pub(crate) const SPI_USR_MOSI: u32 = 1 << 27;

/// `SPI_DOUTDIN`, bitpos [0]: "Set the bit to enable full duplex
/// communication." Without it the MOSI and MISO phases run one after the
/// other, so a full-duplex `transfer` sends all its bytes and *then* clocks in
/// the reply -- reading a line nothing is driving.
///
/// The signature of `transfer(tx, rx)` promises simultaneous exchange, which
/// is what every SPI device expects. This bit is what makes that true.
pub(crate) const SPI_DOUTDIN: u32 = 1 << 0;

/// Data buffer capacity: 16 32-bit words.
const SPI_DATA_BUF_WORDS: usize = 16;
const SPI_MAX_BYTES: usize = SPI_DATA_BUF_WORDS * 4;

/// Bound on `SPI_CMD_USR` poll iterations before giving up. A polled byte
/// transfer at the slowest supported clock completes in well under a
/// millisecond; this bound is generous enough to absorb scheduling jitter
/// while still failing a genuinely wedged peripheral instead of hanging
/// forever.
pub(crate) const SPI_TIMEOUT_SPINS: u32 = 1_000_000;

// ── Pin routing ──────────────────────────────────────────────────────────────
//
// Bases, DPORT clock bits, native pads and the IO_MUX offset table all live in
// `soc-esp32`. This driver used to carry its own copy of that table with
// a comment saying to keep it in sync with the one in `esp32-uart` by hand --
// which is exactly the arrangement the SoC layer exists to end.

/// Route MOSI, MISO and SCK for controller `instance`.
///
/// Any pads will do; `PinMux` takes the IO_MUX direct path when the requested
/// pad is native to the signal and the GPIO matrix otherwise. Before the SoC
/// layer existed this driver accepted only the native triple.
///
/// Off-native routing costs a couple of cycles of latency, which matters at
/// this bus's top speeds -- so it is reported, not silently accepted, once
/// there is somewhere to report it to.
fn route_pins(instance: u8, mosi: u8, miso: u8, sck: u8) -> BusResult<()> {
    if mosi == miso || mosi == sck || miso == sck {
        return Err(BusError::InvalidConfig);
    }
    let mux = Esp32PinMux::new();
    let sigs = [
        (Signal::SpiMosi(instance), mosi),
        (Signal::SpiMiso(instance), miso),
        (Signal::SpiSck(instance), sck),
    ];
    for (sig, pin) in sigs {
        mux.can_route(sig, pin)?;
    }
    for (sig, pin) in sigs {
        mux.route(sig, pin, PinConfig::PUSH_PULL)?;
    }
    Ok(())
}

/// Pack up to 4 bytes (little-endian: `bytes[0]` is the first byte
/// transmitted) into one `SPI_Wn` word. The ESP32 SPI data buffer packs 4
/// bytes per 32-bit word; a prior revision wrote/read one byte per word
/// (`word_addr` advancing 4 bytes per *byte* index), which both wasted 3 of
/// every 4 buffer words and misaligned every byte after the first.
fn pack_word(bytes: &[u8]) -> u32 {
    let mut word = 0u32;
    for (i, &b) in bytes.iter().take(4).enumerate() {
        word |= (b as u32) << (i * 8);
    }
    word
}

/// Inverse of `pack_word`: unpack up to 4 bytes from a `SPI_Wn` word into
/// `out`, honouring the default little-endian `SPI_WR_BYTE_ORDER`/
/// `SPI_RD_BYTE_ORDER` = 0 reset state (first byte transferred = LSB).
fn unpack_word(word: u32, out: &mut [u8]) {
    for (i, b) in out.iter_mut().take(4).enumerate() {
        *b = ((word >> (i * 8)) & 0xFF) as u8;
    }
}

impl Esp32Spi {
    /// Bind a driver instance to the SPI register block at `base_addr`.
    ///
    /// # Safety
    /// `base_addr` must be the base address of a real ESP32 SPI2 or SPI3
    /// register block (0x3FF64000 / 0x3FF65000) and must not be concurrently
    /// owned by another driver instance -- this type performs unchecked
    /// `read_volatile`/`write_volatile` at `base_addr + offset` with no
    /// further validation of the address itself.
    pub unsafe fn new(base_addr: u32) -> Self {
        Self { base: base_addr }
    }

    pub(crate) fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// Perform a polled SPI transfer (up to 64 bytes).
    pub fn transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let len = tx.len().min(rx.len()).min(SPI_MAX_BYTES);
        if len == 0 {
            return Ok(());
        }
        let nwords = len.div_ceil(4);

        unsafe {
            // Write TX data into the data buffer, 4 bytes per word.
            for w in 0..nwords {
                let start = w * 4;
                let end = (start + 4).min(len);
                let word = pack_word(&tx[start..end]);
                self.reg(SPI_W0 + (w as u32 * 4)).write_volatile(word);
            }

            // Bit-length fields, not byte count minus one: SPI_MOSI_DLEN /
            // SPI_MISO_DLEN hold (bits - 1). A prior revision wrote the bit
            // count into USER1, which holds an address-phase bit-length
            // field, not the data-phase length.
            let bits = (len as u32) * 8 - 1;
            self.reg(SPI_MOSI_DLEN).write_volatile(bits);
            self.reg(SPI_MISO_DLEN).write_volatile(bits);

            // Configure the transfer: full duplex, MOSI + MISO phases, byte
            // order left at its little-endian reset default (bits 10/11 unset)
            // to match `pack_word`/`unpack_word`.
            self.reg(SPI_USER)
                .write_volatile(SPI_DOUTDIN | SPI_USR_MOSI | SPI_USR_MISO);

            // Start the transfer (SPI_USR, bit 18 -- not bit 0).
            self.reg(SPI_CMD).write_volatile(SPI_CMD_USR);

            // Wait for completion, bounded: SPI_USR self-clears when the
            // hardware finishes the transaction.
            let mut spins: u32 = 0;
            while self.reg(SPI_CMD).read_volatile() & SPI_CMD_USR != 0 {
                spins += 1;
                if spins > SPI_TIMEOUT_SPINS {
                    return Err(BusError::Timeout);
                }
                core::hint::spin_loop();
            }

            // Read RX data back out, 4 bytes per word.
            for w in 0..nwords {
                let start = w * 4;
                let end = (start + 4).min(len);
                let word = self.reg(SPI_W0 + (w as u32 * 4)).read_volatile();
                unpack_word(word, &mut rx[start..end]);
            }
        }

        Ok(())
    }
}

impl PhysicalBus for Esp32Spi {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        match config {
            BusConfig::Spi { mosi, miso, sck, max_speed, mode } => {
                let instance = addr::spi_instance(self.base).ok_or(BusError::InvalidConfig)?;

                // Clock and un-reset the peripheral before touching any of
                // its registers -- SPI2/SPI3 are gated off and held in reset
                // at boot, so every access below would otherwise be a no-op.
                let clk_bit = dport::clock_bit(self.base).ok_or(BusError::InvalidConfig)?;
                unsafe { dport::enable(clk_bit) };

                route_pins(instance, *mosi, *miso, *sck)?;

                let speed_hz = max_speed.hz();
                let div = (APB_HZ / speed_hz).max(2);

                unsafe {
                    // Clock configuration.
                    //
                    // clkcnt_N and clkcnt_L must hold the same value: the
                    // resulting frequency is APB / ((pre + 1) * (N + 1)), and L
                    // is the low-phase count, not an independent divisor.
                    // clkcnt_H is the high-phase boundary, so N/2 gives a
                    // roughly even duty cycle. Matches esp-idf's
                    // `spi_ll_master_cal_clock`.
                    //
                    // A previous revision wrote N = div/2 and L = div-1. Those
                    // agree only at div == 2, which is what both board
                    // manifests happen to ask for (40 MHz off an 80 MHz APB) --
                    // so it was correct for every configuration in the tree and
                    // ran at roughly double the requested clock for any other.
                    let n = div - 1;
                    self.reg(SPI_CLOCK).write_volatile(
                        ((n & 0x3F) << 12) |        // clkcnt_N
                        (((n / 2) & 0x3F) << 6) |   // clkcnt_H
                        (n & 0x3F)                  // clkcnt_L
                    );

                    // SPI mode (CPOL, CPHA).
                    let (cpol, cpha) = match mode {
                        SpiMode::Mode0 => (0, 0),
                        SpiMode::Mode1 => (0, 1),
                        SpiMode::Mode2 => (1, 0),
                        SpiMode::Mode3 => (1, 1),
                    };

                    let mut pin = self.reg(SPI_PIN).read_volatile();
                    if cpol != 0 { pin |= 1 << 2; } else { pin &= !(1 << 2); }
                    if cpha != 0 { pin |= 1 << 1; } else { pin &= !(1 << 1); }
                    self.reg(SPI_PIN).write_volatile(pin);

                    // Enable master mode, disable slave.
                    let slave = self.reg(SPI_SLAVE);
                    slave.write_volatile(slave.read_volatile() & !1);
                }
                Ok(())
            }
            _ => Err(BusError::InvalidConfig),
        }
    }

    fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        self.transfer(tx, rx)
    }

    fn set_enabled(&mut self, _enabled: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_offsets_match_trm_spi_summary() {
        // Regression guard: the previous revision had CLOCK=0x0C, USER=0x10,
        // USER1=0x14, PIN=0x18, SLAVE=0x1C -- each one register slot early.
        assert_eq!(SPI_CMD, 0x00);
        assert_eq!(SPI_ADDR, 0x04);
        assert_eq!(SPI_CTRL, 0x08);
        assert_eq!(SPI_CTRL1, 0x0C);
        assert_eq!(SPI_RD_STATUS, 0x10);
        assert_eq!(SPI_CTRL2, 0x14);
        assert_eq!(SPI_CLOCK, 0x18);
        assert_eq!(SPI_USER, 0x1C);
        assert_eq!(SPI_USER1, 0x20);
        assert_eq!(SPI_USER2, 0x24);
        assert_eq!(SPI_MOSI_DLEN, 0x28);
        assert_eq!(SPI_MISO_DLEN, 0x2C);
        assert_eq!(SPI_PIN, 0x34);
        assert_eq!(SPI_SLAVE, 0x38);
        assert_eq!(SPI_W0, 0x80);
    }

    #[test]
    fn usr_start_bit_is_18_not_0() {
        // The core of the reported bug: writing/polling bit 0 (SPI_DOUTDIN)
        // instead of bit 18 (SPI_USR) never starts a real transaction and
        // can spin forever waiting for a bit nothing will clear.
        assert_eq!(SPI_CMD_USR, 1 << 18);
        assert_ne!(SPI_CMD_USR, 1);
    }

    #[test]
    fn pack_and_unpack_round_trip_four_bytes_per_word() {
        let bytes = [0xDE, 0xAD, 0xBE, 0xEF];
        let word = pack_word(&bytes);
        // Little-endian: first byte transmitted is the LSB.
        assert_eq!(word, 0xEFBE_ADDE);
        let mut out = [0u8; 4];
        unpack_word(word, &mut out);
        assert_eq!(out, bytes);
    }

    #[test]
    fn pack_handles_partial_final_word() {
        let bytes = [0x01, 0x02];
        let word = pack_word(&bytes);
        assert_eq!(word, 0x0000_0201);
    }

    #[test]
    fn dport_clk_bits_are_distinct_and_match_known_bases() {
        use soc_esp32::addr::{SPI2_BASE, SPI3_BASE};
        assert_eq!(dport::clock_bit(SPI2_BASE), Some(dport::ClockBit::SPI2));
        assert_eq!(dport::clock_bit(SPI3_BASE), Some(dport::ClockBit::SPI3));
        assert_ne!(dport::clock_bit(SPI2_BASE), dport::clock_bit(SPI3_BASE));
        assert_eq!(dport::clock_bit(0xDEAD_BEEF), None);
    }

    #[test]
    fn spi1_is_not_addressable_as_a_general_purpose_controller() {
        // SPI1 drives the boot flash; routing it anywhere bricks the running
        // image.
        use soc_esp32::addr::SPI1_BASE;
        assert_eq!(addr::spi_instance(SPI1_BASE), None);
    }

    #[test]
    fn a_bus_may_not_reuse_one_pad_for_two_signals() {
        assert!(route_pins(3, 23, 23, 18).is_err());
        assert!(route_pins(3, 23, 19, 19).is_err());
    }

    #[test]
    fn mosi_and_sck_cannot_land_on_input_only_pads() {
        // GPIO34-39 have no output driver.
        assert!(route_pins(3, 34, 19, 18).is_err());
        assert!(route_pins(3, 23, 19, 35).is_err());
        // MISO on one is fine -- it is an input.
        let mux = Esp32PinMux::new();
        assert!(mux.can_route(Signal::SpiMiso(3), 34).is_ok());
    }

    #[test]
    fn transfer_length_is_capped_at_the_data_buffer_size() {
        assert_eq!(SPI_MAX_BYTES, 64);
    }
}
