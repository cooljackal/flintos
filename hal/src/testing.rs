// SPDX-License-Identifier: Apache-2.0

//! Test-only bus mocks shared by the logical device-driver unit tests.
//!
//! Behind the `test-support` feature so they never reach a real build; a driver
//! pulls them in with `api = { .., features = ["test-support"] }` under
//! `[dev-dependencies]`. Before this, each register-backed sensor driver kept
//! its own near-identical `impl Bus for FakeThing`, and they had already drifted
//! — most visibly on what a read of an absent register returns.
//!
//! [`RegBus`] is the generic register-file device; [`WriteLog`] is the write-only
//! device (a display). A driver whose fake is genuinely structured — answering
//! whole calibration blocks rather than a flat register map — is better off with
//! a bespoke mock and does not use these.

extern crate std;

use std::sync::Mutex;
use std::vec::Vec;

use crate::bus::{Bus, BusError, BusKind, BusResult, Op};

/// A register-backed I2C device: a mutable `(register, value)` file that answers
/// multi-byte reads from consecutive registers and records what was asked of it.
///
/// A read of a register the file does not hold returns
/// [`BusError::DeviceNotResponding`] — a real device NAKs an address it does not
/// implement, and returning zeros would let a driver read a plausible value out
/// of nothing. Register writes (`[reg, value]`) are applied to the file.
pub struct RegBus {
    regs: Mutex<Vec<(u8, u8)>>,
    reads: Mutex<Vec<u8>>,
    writes: Mutex<Vec<(u8, u8)>>,
    write_bytes: Mutex<Vec<u8>>,
}

impl RegBus {
    /// A device pre-loaded with `regs`.
    pub fn new(regs: &[(u8, u8)]) -> Self {
        Self {
            regs: Mutex::new(regs.to_vec()),
            reads: Mutex::new(Vec::new()),
            writes: Mutex::new(Vec::new()),
            write_bytes: Mutex::new(Vec::new()),
        }
    }

    /// The value at `reg`, if the file holds it.
    pub fn get(&self, reg: u8) -> Option<u8> {
        self.regs.lock().unwrap().iter().find(|(r, _)| *r == reg).map(|(_, v)| *v)
    }

    /// The starting register of every read, in order — "what was asked".
    pub fn reads(&self) -> Vec<u8> {
        self.reads.lock().unwrap().clone()
    }

    /// Every `[register, value]` write, in order.
    pub fn writes(&self) -> Vec<(u8, u8)> {
        self.writes.lock().unwrap().clone()
    }

    /// The raw bytes of every write op, concatenated in order.
    pub fn write_bytes(&self) -> Vec<u8> {
        self.write_bytes.lock().unwrap().clone()
    }

    fn set(&self, reg: u8, val: u8) {
        let mut regs = self.regs.lock().unwrap();
        match regs.iter_mut().find(|(r, _)| *r == reg) {
            Some(e) => e.1 = val,
            None => regs.push((reg, val)),
        }
    }
}

impl Bus for RegBus {
    fn transfer(&self, ops: &mut [Op]) -> BusResult<()> {
        for op in ops.iter_mut() {
            match (op.tx, op.rx.as_deref_mut()) {
                // Register read starting at `tx[0]`, one byte per `rx` slot.
                (Some(tx), Some(rx)) => {
                    let start = *tx.first().ok_or(BusError::InvalidConfig)?;
                    self.reads.lock().unwrap().push(start);
                    for (i, out) in rx.iter_mut().enumerate() {
                        *out = self.get(start + i as u8).ok_or(BusError::DeviceNotResponding)?;
                    }
                }
                // Register write: `[reg, value]` is applied and recorded; any
                // write is kept whole in `write_bytes`.
                (Some(tx), None) => {
                    self.write_bytes.lock().unwrap().extend_from_slice(tx);
                    if tx.len() == 2 {
                        self.writes.lock().unwrap().push((tx[0], tx[1]));
                        self.set(tx[0], tx[1]);
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn max_transfer(&self) -> usize {
        64
    }

    // Answers like an addressed I2C device: the bytes from the register named in
    // `tx[0]` on. A handle's SPI framing (an extra address slot in the reply) is
    // covered by the tests in `hal::bus`.
    fn kind(&self) -> BusKind {
        BusKind::I2c
    }
}

/// A write-only device (e.g. a display): records each transaction whole, so a
/// test can assert the exact byte sequence the driver framed.
#[derive(Default)]
pub struct WriteLog {
    transactions: Mutex<Vec<Vec<u8>>>,
}

impl WriteLog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every write transaction, in order.
    pub fn transactions(&self) -> Vec<Vec<u8>> {
        self.transactions.lock().unwrap().clone()
    }
}

impl Bus for WriteLog {
    fn transfer(&self, ops: &mut [Op]) -> BusResult<()> {
        for op in ops.iter_mut() {
            if let Some(tx) = op.tx {
                self.transactions.lock().unwrap().push(tx.to_vec());
            }
        }
        Ok(())
    }

    fn max_transfer(&self) -> usize {
        64
    }

    fn kind(&self) -> BusKind {
        BusKind::I2c
    }
}
