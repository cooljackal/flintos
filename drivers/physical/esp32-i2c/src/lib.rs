// SPDX-License-Identifier: Apache-2.0

#![no_std]

use hal::bus::{BusConfig, BusError, BusResult, PhysicalBus};
use hal::pinmux::{PinConfig, PinMux, Signal};
use soc_esp32::addr;
use soc_esp32::{dport, Esp32PinMux, APB_HZ};

/// ESP32 I2C master driver (polled mode).
///
/// Base: 0x3FF53000 (I2C0) or 0x3FF67000 (I2C1) -- see
/// `soc_esp32::addr`. A prior revision documented I2C1 as 0x3FF53020, a
/// +0x20 offset from I2C0, but the two controllers are entirely separate
/// register blocks.
pub struct Esp32I2c {
    base: u32,
}

// ── Register map ─────────────────────────────────────────────────────────────
//
// ESP32 TRM chapter 12 (I2C Controller), register summary; offsets confirmed
// against esp-idf `soc/i2c_reg.h`. The previous revision had every offset
// below SDA_HOLD shifted by one register slot (e.g. FIFO_CONF pointed at
// SLAVE_ADDR, FIFO_DATA pointed at INT_RAW) and COMD_BASE pointed at
// SDA_HOLD instead of COMD0 -- so every command written after the first
// landed on whatever register happened to sit at the wrong address.

const I2C_SCL_LOW: u32 = 0x00;
const I2C_CTR: u32 = 0x04;
#[allow(dead_code)] // Read-only transaction status; not currently polled directly (INT_RAW is used instead).
const I2C_SR: u32 = 0x08;
const I2C_TOUT: u32 = 0x0C;
#[allow(dead_code)]
const I2C_SLAVE_ADDR: u32 = 0x10;
#[allow(dead_code)]
const I2C_RXFIFO_ST: u32 = 0x14;
const I2C_FIFO_CONF: u32 = 0x18;
const I2C_FIFO_DATA: u32 = 0x1C;
const I2C_INT_RAW: u32 = 0x20;
const I2C_INT_CLR: u32 = 0x24;
#[allow(dead_code)]
const I2C_INT_ENA: u32 = 0x28;
#[allow(dead_code)]
const I2C_INT_STATUS: u32 = 0x2C;
const I2C_SDA_HOLD: u32 = 0x30;
#[allow(dead_code)]
const I2C_SDA_SAMPLE: u32 = 0x34;
const I2C_SCL_HIGH: u32 = 0x38;
/// First of 16 command registers (`I2C_COMD0_REG` .. `I2C_COMD15_REG`),
/// spanning 0x58..=0x94, 4 bytes apart. A prior revision used 0x30 (really
/// `I2C_SDA_HOLD_REG`) as the command-register base.
const I2C_COMD_BASE: u32 = 0x58;
/// Number of command register slots. Confirmed against esp-idf
/// `soc/i2c_reg.h` (`I2C_COMD0_REG` .. `I2C_COMD15_REG`).
const I2C_COMD_SLOTS: usize = 16;

// ── I2C_COMDn_REG command word ───────────────────────────────────────────────
//
// Bit layout confirmed against esp-idf `soc/i2c_struct.h`:
//   [7:0]   byte_num
//   [8]     ack_en (ack_check_en)
//   [9]     ack_exp
//   [10]    ack_value
//   [13:11] op_code: 0=RSTART 1=WRITE 2=READ 3=STOP 4=END
//   [31]    done (read-only)
//
// A prior revision encoded op_code as 1=RSTART 2=WRITE 3=READ 4=STOP 5=END
// -- every opcode was one higher than the hardware expects -- and separately
// OR'd the 7-bit slave address into the RSTART command word itself
// (`I2C_CMD_RSTART | (addr << 1)`), which stomps into the op_code/ack bits
// of that command instead of being a payload. RSTART takes no address
// operand; the address (with R/W bit) is a FIFO byte sent by a *following*
// WRITE command, exactly like any other data byte.

const I2C_CMD_OP_RSTART: u32 = 0 << 11;
const I2C_CMD_OP_WRITE: u32 = 1 << 11;
const I2C_CMD_OP_READ: u32 = 2 << 11;
const I2C_CMD_OP_STOP: u32 = 3 << 11;
#[allow(dead_code)] // Documented for completeness of the opcode map; this driver terminates transfers with STOP, not END.
const I2C_CMD_OP_END: u32 = 4 << 11;
const I2C_CMD_ACK_VALUE_NAK: u32 = 1 << 10;

// ── Address byte ─────────────────────────────────────────────────────────────
//
// The 7-bit slave address is shifted up one and the low bit carries direction.
// Named rather than written as `| 0` and `| 1`, which reads as a magic number
// in one direction and as a no-op in the other.

const I2C_RW_WRITE: u32 = 0;
const I2C_RW_READ: u32 = 1;

// ── I2C_CTR_REG bits ──────────────────────────────────────────────────────────
//
// Confirmed against esp-idf `soc/i2c_reg.h`. A prior revision wrote
// `1 << 1` for "MS_MODE", which is not the master-mode bit at all (and the
// plain `write_volatile` calls in `write`/`read` also clobbered it on every
// transaction instead of only setting TRANS_START).

const I2C_MS_MODE: u32 = 1 << 4;
const I2C_TRANS_START: u32 = 1 << 5;

// ── I2C_INT_RAW_REG / I2C_INT_CLR_REG bits ───────────────────────────────────
//
// Confirmed against esp-idf `soc/i2c_reg.h`. A prior revision polled/cleared
// bit 0, but `I2C_TRANS_COMPLETE_INT_RAW` is bit 7 -- bit 0 is unrelated, so
// `wait_done` could return immediately on a stale/unrelated bit or never
// return at all, depending on what bit 0 happens to reflect.

const I2C_TRANS_COMPLETE: u32 = 1 << 7;

// ── Pin routing ──────────────────────────────────────────────────────────────
//
// Unlike UART and HSPI/VSPI, the classic ESP32 I2C controllers have *no*
// IO_MUX-native SDA/SCL pins. Both signals are always carried through the GPIO
// matrix, which is why this driver rejected every configuration until the SoC
// layer existed to do that routing. `soc_esp32::Esp32PinMux` now owns it:
// the signal indices, the per-pad GPIO function number (which is not uniform),
// the open-drain pad driver bit, and leaving output-enable with the peripheral
// so the controller can release the line.

/// Route SDA and SCL for controller `instance` to the requested pads.
///
/// Open-drain with the internal pull-up, which is a convenience for a short
/// bus with one device and not a substitute for real external pull-ups.
fn route_pins(instance: u8, sda: u8, scl: u8) -> BusResult<()> {
    if sda == scl {
        // The hardware would accept this and the bus would never work: SDA and
        // SCL both driven from the same pad is a permanent stuck condition,
        // and it presents as "no device responds" rather than as a config
        // error.
        return Err(BusError::InvalidConfig);
    }
    let mux = Esp32PinMux::new();
    let sda_sig = Signal::I2cSda(instance);
    let scl_sig = Signal::I2cScl(instance);

    // Check both before routing either. Routing is not transactional, and a
    // bus with SDA connected and SCL dangling is harder to diagnose than one
    // that refused to come up.
    mux.can_route(sda_sig, sda)?;
    mux.can_route(scl_sig, scl)?;

    mux.route(sda_sig, sda, PinConfig::OPEN_DRAIN_PULLUP)?;
    mux.route(scl_sig, scl, PinConfig::OPEN_DRAIN_PULLUP)?;
    Ok(())
}

/// Bound on `wait_done` poll iterations before giving up. Generous relative
/// to a worst-case (100 kHz, 9 bits/byte with ACK) single-byte transaction,
/// which completes in well under a millisecond.
const I2C_TIMEOUT_SPINS: u32 = 1_000_000;

/// Maximum bytes per `write`/`read` call. The command register file has
/// `I2C_COMD_SLOTS` (16) slots and every transfer needs 1 (RSTART) + 1
/// (address byte, folded into the first WRITE) + n (data, one WRITE/READ
/// command per byte in this simple polled implementation) + 1 (STOP), so
/// the largest `n` that fits is `I2C_COMD_SLOTS - 3`.
const I2C_MAX_BYTES: usize = I2C_COMD_SLOTS - I2C_COMD_OVERHEAD;

/// Command slots a transfer spends on framing rather than data: one RSTART,
/// one WRITE carrying the address byte, one STOP.
const I2C_COMD_OVERHEAD: usize = 3;

// The budget, checked at compile time. Equality, not `<=`: I2C_MAX_BYTES is
// meant to be the largest payload that fits, so anything else means the two
// constants have drifted apart.
const _: () = assert!(I2C_MAX_BYTES + I2C_COMD_OVERHEAD == I2C_COMD_SLOTS);

impl Esp32I2c {
    /// Bind a driver instance to the I2C register block at `base_addr`.
    ///
    /// # Safety
    /// `base_addr` must be the base address of a real ESP32 I2C0 or I2C1
    /// register block (0x3FF53000 / 0x3FF67000) and must not be concurrently
    /// owned by another driver instance -- this type performs unchecked
    /// `read_volatile`/`write_volatile` at `base_addr + offset` with no
    /// further validation of the address itself.
    pub unsafe fn new(base_addr: u32) -> Self {
        Self { base: base_addr }
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// Set `I2C_TRANS_START` without disturbing the rest of `I2C_CTR_REG`
    /// (in particular, `I2C_MS_MODE` set during `init`).
    unsafe fn start_transfer(&self) {
        let ctr = self.reg(I2C_CTR);
        ctr.write_volatile(ctr.read_volatile() | I2C_TRANS_START);
    }

    /// Wait for `I2C_TRANS_COMPLETE_INT_RAW`, bounded.
    fn wait_done(&self) -> BusResult<()> {
        let mut spins: u32 = 0;
        loop {
            let raw = unsafe { self.reg(I2C_INT_RAW).read_volatile() };
            if raw & I2C_TRANS_COMPLETE != 0 {
                unsafe { self.reg(I2C_INT_CLR).write_volatile(I2C_TRANS_COMPLETE) };
                return Ok(());
            }
            spins += 1;
            if spins > I2C_TIMEOUT_SPINS {
                return Err(BusError::Timeout);
            }
            core::hint::spin_loop();
        }
    }

    /// Write bytes to a slave device.
    pub fn write(&self, addr: u8, data: &[u8]) -> BusResult<()> {
        if data.len() > I2C_MAX_BYTES {
            return Err(BusError::InvalidConfig);
        }
        unsafe {
            let mut slot = 0u32;

            // Command 0: RSTART. No address payload -- the address goes in
            // the FIFO, sent by the WRITE command that follows.
            self.reg(I2C_COMD_BASE + slot * 4).write_volatile(I2C_CMD_OP_RSTART);
            slot += 1;

            // Command 1: WRITE 1 byte (the slave address + R/W=0), from the
            // FIFO.
            self.reg(I2C_FIFO_DATA)
                .write_volatile(((addr as u32) << 1) | I2C_RW_WRITE);
            self.reg(I2C_COMD_BASE + slot * 4).write_volatile(I2C_CMD_OP_WRITE | 1);
            slot += 1;

            // Commands 2..N+1: write data bytes.
            for &byte in data {
                self.reg(I2C_FIFO_DATA).write_volatile(byte as u32);
                self.reg(I2C_COMD_BASE + slot * 4).write_volatile(I2C_CMD_OP_WRITE | 1);
                slot += 1;
            }

            // Last command: STOP.
            self.reg(I2C_COMD_BASE + slot * 4).write_volatile(I2C_CMD_OP_STOP);

            self.start_transfer();
        }
        self.wait_done()
    }

    /// Read bytes from a slave device into `buf`.
    ///
    /// Takes the buffer rather than a length because the previous signature
    /// took a length, clocked the bytes in, and left them in the RX FIFO --
    /// the caller got `Ok(())` and no data. Every read this driver has ever
    /// done returned nothing, which is consistent with I2C never having been
    /// confirmed against a real device.
    pub fn read(&self, addr: u8, buf: &mut [u8]) -> BusResult<()> {
        let len = buf.len();
        if len > I2C_MAX_BYTES {
            return Err(BusError::InvalidConfig);
        }
        if len == 0 {
            return Ok(());
        }
        unsafe {
            let mut slot = 0u32;

            // Command 0: RSTART.
            self.reg(I2C_COMD_BASE + slot * 4).write_volatile(I2C_CMD_OP_RSTART);
            slot += 1;

            // Command 1: WRITE 1 byte (the slave address + R/W=1), from the
            // FIFO.
            self.reg(I2C_FIFO_DATA)
                .write_volatile(((addr as u32) << 1) | I2C_RW_READ);
            self.reg(I2C_COMD_BASE + slot * 4).write_volatile(I2C_CMD_OP_WRITE | 1);
            slot += 1;

            // Commands 2..N+1: READ with ACK, except the last byte which
            // gets NAK (ack_value=1) to signal the slave to stop sending.
            for i in 0..len {
                let mut cmd = I2C_CMD_OP_READ | 1;
                if i == len - 1 {
                    cmd |= I2C_CMD_ACK_VALUE_NAK;
                }
                self.reg(I2C_COMD_BASE + slot * 4).write_volatile(cmd);
                slot += 1;
            }

            // Last command: STOP.
            self.reg(I2C_COMD_BASE + slot * 4).write_volatile(I2C_CMD_OP_STOP);

            self.start_transfer();
        }
        self.wait_done()?;

        // Drain the RX FIFO. The bytes are sitting in it; without this they
        // are simply dropped.
        unsafe {
            for byte in buf.iter_mut() {
                *byte = (self.reg(I2C_FIFO_DATA).read_volatile() & 0xFF) as u8;
            }
        }
        Ok(())
    }
}

impl PhysicalBus for Esp32I2c {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        match config {
            BusConfig::I2c { sda, scl, speed } => {
                let instance = addr::i2c_instance(self.base).ok_or(BusError::InvalidConfig)?;

                // Clock and un-reset the peripheral before touching any of
                // its registers -- I2C0/I2C1 are gated off and held in reset
                // at boot, so every access below would otherwise be a no-op.
                let clk_bit = dport::clock_bit(self.base).ok_or(BusError::InvalidConfig)?;
                unsafe { dport::enable(clk_bit) };

                // Route SDA/SCL through the GPIO matrix. Do this before
                // programming the timing registers: the pads come up as
                // push-pull inputs, and connecting a configured, running
                // controller to them is a window in which it can drive a bus
                // another device is holding low.
                route_pins(instance, *sda, *scl)?;

                let scl_hz = speed.hz();
                let half_period = (APB_HZ / scl_hz / 2).max(10);

                unsafe {
                    self.reg(I2C_SCL_LOW).write_volatile(half_period);
                    self.reg(I2C_SCL_HIGH).write_volatile(half_period);
                    self.reg(I2C_SDA_HOLD).write_volatile(half_period / 2);
                    self.reg(I2C_TOUT).write_volatile(0); // disable timeout

                    // Enable I2C master mode without disturbing other CTR
                    // bits (there are none set yet, but RMW keeps this
                    // robust if that changes).
                    let ctr = self.reg(I2C_CTR);
                    ctr.write_volatile(ctr.read_volatile() | I2C_MS_MODE);

                    self.reg(I2C_FIFO_CONF).write_volatile(
                        (1 << 13) | // TX_EMPTY_INT_ENA
                        (1 << 14)   // RX_FULL_INT_ENA
                    );
                }
                Ok(())
            }
            _ => Err(BusError::InvalidConfig),
        }
    }

    fn raw_transfer(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        // `tx[0]` is the device's 7-bit address, UNSHIFTED -- this driver adds
        // the R/W bit itself (see `write`/`read`). A caller that pre-shifts
        // double-shifts, and 0x76 reaches the wire as 0xD8 with nothing to
        // ACK it. See `hal::PhysicalBus::raw_transfer`.
        let Some((&addr, data)) = tx.split_first() else {
            return Err(BusError::InvalidConfig);
        };

        // All three shapes, rather than silently succeeding on two of them.
        // The old version did nothing at all unless both tx and rx were
        // non-empty, so every write-only call returned Ok having sent nothing.
        match (data.is_empty(), rx.is_empty()) {
            (_, true) => self.write(addr, data),
            (true, false) => self.read(addr, rx),
            (false, false) => {
                self.write(addr, data)?;
                self.read(addr, rx)
            }
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        let ctr = self.reg(I2C_CTR);
        unsafe {
            let mut val = ctr.read_volatile();
            if enabled { val |= 1; } else { val &= !1; }
            ctr.write_volatile(val);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soc_esp32::addr::{I2C0_BASE, I2C1_BASE};

    #[test]
    fn register_offsets_match_trm_i2c_summary() {
        // Regression guard: the previous revision had FIFO_CONF=0x10 (really
        // SLAVE_ADDR), FIFO_DATA=0x20 (really INT_RAW), INT_RAW=0x24 (really
        // INT_CLR), INT_CLR=0x2C (really INT_STATUS), SDA_HOLD=0x0C (really
        // TOUT), and COMD_BASE=0x30 (really SDA_HOLD).
        assert_eq!(I2C_SCL_LOW, 0x00);
        assert_eq!(I2C_CTR, 0x04);
        assert_eq!(I2C_SR, 0x08);
        assert_eq!(I2C_TOUT, 0x0C);
        assert_eq!(I2C_SLAVE_ADDR, 0x10);
        assert_eq!(I2C_RXFIFO_ST, 0x14);
        assert_eq!(I2C_FIFO_CONF, 0x18);
        assert_eq!(I2C_FIFO_DATA, 0x1C);
        assert_eq!(I2C_INT_RAW, 0x20);
        assert_eq!(I2C_INT_CLR, 0x24);
        assert_eq!(I2C_INT_ENA, 0x28);
        assert_eq!(I2C_INT_STATUS, 0x2C);
        assert_eq!(I2C_SDA_HOLD, 0x30);
        assert_eq!(I2C_SDA_SAMPLE, 0x34);
        assert_eq!(I2C_SCL_HIGH, 0x38);
        assert_eq!(I2C_COMD_BASE, 0x58);
    }

    #[test]
    fn command_slots_cover_the_documented_range() {
        // COMD0..COMD15 at 4-byte strides must land exactly on 0x94, the
        // real I2C_COMD15_REG offset -- if this drifts, I2C_MAX_BYTES is
        // wrong too.
        let last = I2C_COMD_BASE + (I2C_COMD_SLOTS as u32 - 1) * 4;
        assert_eq!(last, 0x94);
    }

    #[test]
    fn opcode_values_are_zero_based() {
        // The core of the reported bug: op_code 0=RSTART..4=END, not
        // 1=RSTART..5=END.
        assert_eq!(I2C_CMD_OP_RSTART, 0 << 11);
        assert_eq!(I2C_CMD_OP_WRITE, 1 << 11);
        assert_eq!(I2C_CMD_OP_READ, 2 << 11);
        assert_eq!(I2C_CMD_OP_STOP, 3 << 11);
        assert_eq!(I2C_CMD_OP_END, 4 << 11);
    }

    #[test]
    fn rstart_carries_no_address_payload() {
        // RSTART must be a bare opcode -- OR'ing the address into it (the
        // original bug) corrupts the op_code/ack bits of that command.
        let addr: u32 = 0x50;
        let corrupted_pattern = I2C_CMD_OP_RSTART | (addr << 1);
        assert_ne!(I2C_CMD_OP_RSTART, corrupted_pattern);
        // The fixed encoding: RSTART is used verbatim.
        assert_eq!(I2C_CMD_OP_RSTART, 0);
    }

    #[test]
    fn trans_complete_bit_is_7_not_0() {
        assert_eq!(I2C_TRANS_COMPLETE, 1 << 7);
        assert_ne!(I2C_TRANS_COMPLETE, 1);
    }

    #[test]
    fn ms_mode_bit_is_4_not_1() {
        assert_eq!(I2C_MS_MODE, 1 << 4);
        assert_ne!(I2C_MS_MODE, 1 << 1);
    }

    #[test]
    fn byte_count_is_bounded_to_fit_the_command_register_file() {
        // The relationship is asserted at compile time; this pins the number
        // itself, so a change to I2C_COMD_SLOTS is a visible decision rather
        // than a silent capacity change.
        assert_eq!(I2C_MAX_BYTES, 13);
    }

    #[test]
    fn i2c_base_addresses_are_distinct_register_blocks() {
        // I2C1 is not I2C0 + 0x20; it's an entirely separate block.
        assert_eq!(I2C0_BASE, 0x3FF5_3000);
        assert_eq!(I2C1_BASE, 0x3FF6_7000);
        assert_ne!(I2C1_BASE - I2C0_BASE, 0x20);
    }

    #[test]
    fn dport_clk_bits_are_distinct_and_match_known_bases() {
        assert_eq!(dport::clock_bit(I2C0_BASE), Some(dport::ClockBit::I2C0));
        assert_eq!(dport::clock_bit(I2C1_BASE), Some(dport::ClockBit::I2C1));
        assert_ne!(dport::clock_bit(I2C0_BASE), dport::clock_bit(I2C1_BASE));
        assert_eq!(dport::clock_bit(0xDEAD_BEEF), None);
    }

    #[test]
    fn sda_and_scl_may_not_share_a_pad() {
        // The hardware accepts it and the bus then never works, presenting as
        // "no device responds" rather than as a configuration error.
        assert!(route_pins(0, 21, 21).is_err());
    }

    #[test]
    fn unroutable_pads_are_rejected() {
        // GPIO28-31 have no IO_MUX register at all.
        assert!(route_pins(0, 28, 22).is_err());
        // GPIO34-39 are input-only, and an I2C line has to be able to drive
        // low. Both pins are validated before either is routed, so this
        // rejects without having connected SDA first.
        assert!(route_pins(0, 21, 34).is_err());
        // A controller instance this chip does not have.
        assert!(route_pins(2, 21, 22).is_err());
    }
}
