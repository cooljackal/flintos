// SPDX-License-Identifier: Apache-2.0

#![no_std]

use flint_hal::bus::{BusConfig, BusError, BusResult, PhysicalBus};

/// ESP32 I2C master driver (polled mode).
/// Base: 0x3FF53000 (I2C0) or 0x3FF53020 (I2C1).
pub struct Esp32I2c {
    base: u32,
}

const I2C_SCL_LOW: u32 = 0x00;
const I2C_CTR: u32 = 0x04;
const I2C_TOUT: u32 = 0x08;
const I2C_SDA_HOLD: u32 = 0x0C;
const I2C_FIFO_CONF: u32 = 0x10;
const I2C_FIFO_DATA: u32 = 0x20;
const I2C_INT_RAW: u32 = 0x24;
const I2C_INT_CLR: u32 = 0x2C;
const I2C_COMD_BASE: u32 = 0x30; // 16 command registers, 4 bytes each

const I2C_CMD_RSTART: u32 = 1 << 11;
const I2C_CMD_WRITE: u32 = 2 << 11;
const I2C_CMD_READ: u32 = 3 << 11;
const I2C_CMD_STOP: u32 = 4 << 11;
const I2C_CMD_END: u32 = 5 << 11;

const I2C_TRANS_START: u32 = 1 << 5;
const I2C_TRANS_DONE: u32 = 1 << 0;

impl Esp32I2c {
    pub fn new(base_addr: u32) -> Self {
        Self { base: base_addr }
    }

    fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    fn wait_done(&self) {
        loop {
            let raw = unsafe { self.reg(I2C_INT_RAW).read_volatile() };
            if raw & I2C_TRANS_DONE != 0 {
                unsafe { self.reg(I2C_INT_CLR).write_volatile(I2C_TRANS_DONE); }
                break;
            }
        }
    }

    /// Write bytes to a slave device.
    pub fn write(&self, addr: u8, data: &[u8]) -> BusResult<()> {
        let len = data.len().min(16);
        unsafe {
            // Command 0: RSTART + slave addr (write) + ACK
            let cmd0 = I2C_CMD_RSTART | ((addr as u32) << 1) | 0;
            // We write the addr byte to the FIFO.
            self.reg(I2C_FIFO_DATA).write_volatile((addr as u32) << 1 | 0);
            self.reg(I2C_COMD_BASE).write_volatile(cmd0 | 1); // 1 byte

            // Commands 1..N: write data bytes
            for i in 0..len {
                self.reg(I2C_FIFO_DATA).write_volatile(data[i] as u32);
                let cmd = I2C_CMD_WRITE | 1; // 1 byte
                self.reg(I2C_COMD_BASE + (i as u32 + 1) * 4).write_volatile(cmd);
            }

            // Last command: STOP
            self.reg(I2C_COMD_BASE + (len as u32 + 1) * 4).write_volatile(I2C_CMD_STOP | I2C_CMD_END | 1);

            // Start transfer.
            self.reg(I2C_CTR).write_volatile(I2C_TRANS_START);
        }
        self.wait_done();
        Ok(())
    }

    /// Read bytes from a slave device.
    pub fn read(&self, addr: u8, len: usize) -> BusResult<()> {
        let len = len.min(16);
        unsafe {
            // Command 0: RSTART + slave addr (read) + ACK
            let cmd0 = I2C_CMD_RSTART | ((addr as u32) << 1) | 1;
            self.reg(I2C_FIFO_DATA).write_volatile((addr as u32) << 1 | 1);
            self.reg(I2C_COMD_BASE).write_volatile(cmd0 | 1);

            // Commands 1..N: read with ACK, last with NAK
            for i in 0..len {
                let ack = if i < len - 1 { 0 } else { 1 };
                let cmd = I2C_CMD_READ | 1 | (ack << 8);
                self.reg(I2C_COMD_BASE + (i as u32 + 1) * 4).write_volatile(cmd);
            }

            // Last command: STOP
            self.reg(I2C_COMD_BASE + (len as u32 + 1) * 4).write_volatile(I2C_CMD_STOP | I2C_CMD_END | 1);

            // Start transfer.
            self.reg(I2C_CTR).write_volatile(I2C_TRANS_START);
        }
        self.wait_done();
        Ok(())
    }
}

impl PhysicalBus for Esp32I2c {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        match config {
            BusConfig::I2c { speed, .. } => {
                let apb_hz: u32 = 80_000_000;
                let scl_hz = speed.hz();
                let half_period = (apb_hz / scl_hz / 2).max(10);

                unsafe {
                    self.reg(I2C_SCL_LOW).write_volatile(half_period);
                    self.reg(I2C_SCL_LOW + 0x04).write_volatile(half_period); // SCL_HIGH
                    self.reg(I2C_SDA_HOLD).write_volatile(half_period / 2);   // SDA hold
                    self.reg(I2C_TOUT).write_volatile(0); // disable timeout

                    // Enable I2C master.
                    self.reg(I2C_CTR).write_volatile(1 << 1); // MS_MODE
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
        // I2C raw_transfer does a write followed by repeated-start read.
        if tx.len() > 0 && rx.len() > 0 {
            self.write(tx[0], &tx[1..])?;
            self.read(tx[0], rx.len())?;
        }
        Ok(())
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
