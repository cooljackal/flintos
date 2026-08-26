// SPDX-License-Identifier: Apache-2.0
#![no_std]
//! Exclusive block ownership, private instruction lowering and bounded FIFOs.
//! SDK 2.1.1 pio.c/pio.h define initialization, relocation and FIFO clearing.
//! No native instruction escape, DMA or interrupt enables are exposed.

use hal::pio::{Config, Error, Instruction, Operation, ProgrammableIo};
use hal::soc::SystemOnChip;
use soc_rp2040::ctrl::{try_claim_pio, PioLease};

const CTRL: u32 = 0;
const FSTAT: u32 = 4;
const FDEBUG: u32 = 8;
const IMEM: u32 = 0x48;
const SM: u32 = 0xc8;
const STRIDE: u32 = 0x18;
const SHIFT: u32 = (1 << 18) | (1 << 19); // LSB-first, explicit PUSH/PULL.
const POLL_LIMIT: u32 = 100_000;

/// Board-owned routing: at most one input and one output per engine.
#[derive(Clone, Copy, Debug)]
pub struct Port {
    pub block: u8,
    pub input: Option<u8>,
    pub output: Option<u8>,
}
impl Port {
    fn mask(self) -> Result<u32, Error> {
        if self.block >= 2
            || self.input.is_some_and(|p| p >= 30)
            || self.output.is_some_and(|p| p >= 30)
            || (self.input.is_some() && self.input == self.output)
        {
            return Err(Error::Invalid);
        }
        Ok(self.input.map_or(0, |p| 1 << p) | self.output.map_or(0, |p| 1 << p))
    }
    fn base(self) -> u32 {
        0x5020_0000 + u32::from(self.block) * 0x0010_0000
    }
}

trait Registers: Send {
    fn read(&mut self, address: u32) -> u32;
    fn write(&mut self, address: u32, value: u32);
    fn now_us(&mut self) -> u32;
}
struct Hardware;
impl Registers for Hardware {
    fn read(&mut self, address: u32) -> u32 {
        unsafe { (address as *const u32).read_volatile() }
    }
    fn write(&mut self, address: u32, value: u32) {
        unsafe { (address as *mut u32).write_volatile(value) }
    }
    fn now_us(&mut self) -> u32 {
        #[cfg(target_arch = "arm")]
        {
            soc_rp2040::timer_us()
        }
        #[cfg(not(target_arch = "arm"))]
        {
            0
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Machine {
    offset: u8,
    length: u8,
    config: Config,
}
struct Engine<I: Registers> {
    io: I,
    port: Port,
    _lease: Option<PioLease>,
    memory: u32,
    machines: [Option<Machine>; 4],
    saved_pins: [(u32, u32); 2],
    initialized: bool,
}

fn allocation(used: u32, length: usize) -> Result<(u8, u32), Error> {
    if !(1..=32).contains(&length) {
        return Err(Error::Invalid);
    }
    let mask = if length == 32 {
        u32::MAX
    } else {
        (1 << length) - 1
    };
    for offset in (0..=32 - length).rev() {
        if used & (mask << offset) == 0 {
            return Ok((offset as u8, mask << offset));
        }
    }
    Err(Error::NoSpace)
}

fn divider(cpu_hz: u32, requested_hz: u32) -> Result<u32, Error> {
    if requested_hz == 0 || cpu_hz == 0 || requested_hz > cpu_hz {
        return Err(Error::Invalid);
    }
    let fixed = (u64::from(cpu_hz) * 256).div_ceil(u64::from(requested_hz));
    if !(256..=0x0100_0000).contains(&fixed) {
        return Err(Error::Invalid);
    }
    // Integer 0 encodes exactly 65536, only with fraction zero.
    Ok((fixed as u32 & 0x00ff_ffff) << 8)
}

fn encode(instruction: Instruction, offset: u8, length: usize, cfg: Config) -> Result<u16, Error> {
    if instruction.delay > 31 {
        return Err(Error::Invalid);
    }
    let jump = |target: u8| {
        if usize::from(target) >= length || u16::from(target) + u16::from(offset) >= 32 {
            Err(Error::Invalid)
        } else {
            Ok(u16::from(target) + u16::from(offset))
        }
    };
    let bits = match instruction.operation {
        Operation::Pull => 0x80a0,
        Operation::Push => 0x8020,
        Operation::OutputBit if cfg.output => 0x6001,
        Operation::InputBit if cfg.input => 0x4001,
        Operation::SetOutput(high) if cfg.output => 0xe000 | u16::from(high),
        Operation::WaitInput(high) if cfg.input => 0x2020 | (u16::from(high) << 7),
        Operation::SetCounter(value) if value <= 31 => 0xe020 | u16::from(value),
        Operation::Jump(target) => jump(target)?,
        Operation::JumpDecrement(target) => 0x0040 | jump(target)?,
        Operation::Nop => 0xa042, // MOV Y,Y.
        _ => return Err(Error::Invalid),
    };
    Ok(bits | (u16::from(instruction.delay) << 8))
}

impl<I: Registers> Engine<I> {
    fn new(io: I, port: Port, lease: Option<PioLease>) -> Self {
        Self {
            io,
            port,
            _lease: lease,
            memory: 0,
            machines: [None; 4],
            saved_pins: [(0, 0); 2],
            initialized: false,
        }
    }
    fn read(&mut self, offset: u32) -> u32 {
        self.io.read(self.port.base() + offset)
    }
    fn write(&mut self, offset: u32, value: u32) {
        self.io.write(self.port.base() + offset, value);
    }
    fn sm(machine: u8, offset: u32) -> u32 {
        SM + u32::from(machine) * STRIDE + offset
    }
    fn get(&self, machine: u8) -> Result<Machine, Error> {
        self.machines
            .get(usize::from(machine))
            .ok_or(Error::Invalid)?
            .ok_or(Error::NotConfigured)
    }
    fn initialize(&mut self) -> Result<(), Error> {
        let block_mask = 1 << (10 + self.port.block);
        let banks = soc_rp2040::RESET_IO_BANK0 | soc_rp2040::RESET_PADS_BANK0;
        self.io.write(soc_rp2040::RESETS_BASE + 0x2000, block_mask);
        self.io
            .write(soc_rp2040::RESETS_BASE + 0x3000, block_mask | banks);
        let mut ready = false;
        for _ in 0..POLL_LIMIT {
            if self.io.read(soc_rp2040::RESETS_BASE + 8) & (block_mask | banks)
                == block_mask | banks
            {
                ready = true;
                break;
            }
        }
        if !ready {
            return Err(Error::Hardware);
        }
        self.write(CTRL, 0);
        self.write(0x12c, 0); // IRQ0_INTE
        self.write(0x138, 0); // IRQ1_INTE
        self.write(0x130, 0); // IRQ0_INTF
        self.write(0x13c, 0); // IRQ1_INTF
        self.write(0x30, 0xff); // clear all internal IRQ flags
        for (i, pin) in [self.port.input, self.port.output].into_iter().enumerate() {
            if let Some(pin) = pin {
                let pad = soc_rp2040::PADS_BANK0_BASE + 4 + u32::from(pin) * 4;
                let mux = soc_rp2040::IO_BANK0_BASE + 4 + u32::from(pin) * 8;
                let old_pad = self.io.read(pad);
                self.saved_pins[i] = (old_pad, self.io.read(mux));
                self.io.write(
                    pad,
                    (old_pad & !((1 << 7) | (1 << 3) | (1 << 2))) | (1 << 6),
                );
                // Reset left all PIO output enables low; no pin is driven yet.
                self.io.write(mux, 6 + u32::from(self.port.block));
            }
        }
        self.initialized = true;
        Ok(())
    }
    fn set_direction(&mut self, machine: u8, pin: u8, output: bool) {
        let offset = Self::sm(machine, 0x14);
        let saved = self.read(offset);
        self.write(offset, (1 << 26) | (u32::from(pin) << 5));
        self.write(Self::sm(machine, 0x10), 0xe080 | u32::from(output)); // SET PINDIRS
        self.write(offset, saved);
    }
    fn clear_machine(&mut self, machine: u8) {
        self.write(0x3000 + CTRL, 1 << machine); // atomic disable, preserve peers
        self.write(Self::sm(machine, 8), SHIFT | (1 << 31));
        self.write(Self::sm(machine, 8), SHIFT); // FIFO join toggle flushes both
        self.write(FDEBUG, 0x0101_0101 << machine);
        self.write(0x2000 + CTRL, (1 << (4 + machine)) | (1 << (8 + machine)));
        self.write(Self::sm(machine, 0x10), 0xa0c3); // MOV ISR,NULL
        self.write(Self::sm(machine, 0x10), 0xa0e3); // MOV OSR,NULL
        self.write(Self::sm(machine, 0x10), 0xe020); // SET X,0
    }
}

impl<I: Registers> ProgrammableIo for Engine<I> {
    fn configure(
        &mut self,
        machine: u8,
        program: &[Instruction],
        config: Config,
    ) -> Result<(), Error> {
        if machine >= 4 {
            return Err(Error::Invalid);
        }
        if self.machines[usize::from(machine)].is_some() {
            return Err(Error::Busy);
        }
        if (config.input && self.port.input.is_none())
            || (config.output && self.port.output.is_none())
        {
            return Err(Error::Invalid);
        }
        if self
            .machines
            .iter()
            .flatten()
            .any(|m| (config.input && m.config.input) || (config.output && m.config.output))
        {
            return Err(Error::Busy);
        }
        let div = divider(soc_rp2040::Rp2040::DEFAULT_CPU_HZ, config.frequency_hz)?;
        let (offset, mask) = allocation(self.memory, program.len())?;
        let mut lowered = [0u16; 32];
        for (i, instruction) in program.iter().enumerate() {
            lowered[i] = encode(*instruction, offset, program.len(), config)?;
        }
        self.clear_machine(machine);
        for (i, value) in lowered[..program.len()].iter().enumerate() {
            self.write(IMEM + (u32::from(offset) + i as u32) * 4, u32::from(*value));
        }
        self.write(Self::sm(machine, 0), div);
        self.write(
            Self::sm(machine, 4),
            ((u32::from(offset) + program.len() as u32 - 1) << 12) | (u32::from(offset) << 7),
        );
        let pinctrl = if config.input {
            u32::from(self.port.input.unwrap()) << 15
        } else {
            0
        } | if config.output {
            let pin = u32::from(self.port.output.unwrap());
            (1 << 20) | (1 << 26) | (pin << 5) | pin
        } else {
            0
        };
        self.write(Self::sm(machine, 0x14), pinctrl);
        if config.input {
            self.set_direction(machine, self.port.input.unwrap(), false);
        }
        if config.output {
            self.write(Self::sm(machine, 0x10), 0xe000); // initial low, before OE
            self.set_direction(machine, self.port.output.unwrap(), true);
        }
        self.write(Self::sm(machine, 0x10), u32::from(offset));
        self.memory |= mask;
        self.machines[usize::from(machine)] = Some(Machine {
            offset,
            length: program.len() as u8,
            config,
        });
        Ok(())
    }
    fn start(&mut self, machine: u8) -> Result<(), Error> {
        self.get(machine)?;
        self.write(0x2000 + CTRL, 1 << machine);
        Ok(())
    }
    fn try_write(&mut self, machine: u8, word: u32) -> Result<(), Error> {
        self.get(machine)?;
        if self.read(FSTAT) & (1 << (16 + machine)) != 0 {
            return Err(Error::WouldBlock);
        }
        self.write(0x10 + 4 * u32::from(machine), word);
        Ok(())
    }
    fn try_read(&mut self, machine: u8) -> Result<u32, Error> {
        self.get(machine)?;
        if self.read(FSTAT) & (1 << (8 + machine)) != 0 {
            return Err(Error::WouldBlock);
        }
        Ok(self.read(0x20 + 4 * u32::from(machine)))
    }
    fn exchange(&mut self, machine: u8, word: u32, timeout_us: u32) -> Result<u32, Error> {
        self.get(machine)?;
        if !(1..=1_000_000).contains(&timeout_us) {
            return Err(Error::Invalid);
        }
        let started = self.io.now_us();
        let mut sent = false;
        for _ in 0..POLL_LIMIT {
            if self.io.now_us().wrapping_sub(started) >= timeout_us {
                break;
            }
            if !sent {
                match self.try_write(machine, word) {
                    Ok(()) => sent = true,
                    Err(Error::WouldBlock) => {}
                    Err(e) => return Err(e),
                }
            }
            if sent {
                match self.try_read(machine) {
                    Ok(word) => return Ok(word),
                    Err(Error::WouldBlock) => {}
                    Err(e) => return Err(e),
                }
            }
        }
        self.cancel(machine)?;
        Err(Error::Timeout)
    }
    fn cancel(&mut self, machine: u8) -> Result<(), Error> {
        let m = self.get(machine)?;
        self.clear_machine(machine);
        if m.config.output {
            self.set_direction(machine, self.port.output.unwrap(), false);
        }
        for offset in m.offset..m.offset + m.length {
            self.write(IMEM + u32::from(offset) * 4, u32::from(offset));
        }
        let mask = if m.length == 32 {
            u32::MAX
        } else {
            ((1u32 << m.length) - 1) << m.offset
        };
        self.memory &= !mask;
        self.machines[usize::from(machine)] = None;
        Ok(())
    }
    fn reset(&mut self) {
        for machine in 0..4 {
            if self.machines[machine].is_some() {
                let _ = self.cancel(machine as u8);
            }
        }
    }
}

impl<I: Registers> Drop for Engine<I> {
    fn drop(&mut self) {
        if !self.initialized {
            return;
        }
        self.reset();
        for (i, pin) in [self.port.input, self.port.output].into_iter().enumerate() {
            if let Some(pin) = pin {
                self.io.write(
                    soc_rp2040::IO_BANK0_BASE + 4 + u32::from(pin) * 8,
                    self.saved_pins[i].1,
                );
                self.io.write(
                    soc_rp2040::PADS_BANK0_BASE + 4 + u32::from(pin) * 4,
                    self.saved_pins[i].0,
                );
            }
        }
        // Lease drop follows quiescence and pin restoration, without waiting.
    }
}

pub struct Rp2040Pio(Engine<Hardware>);
impl Rp2040Pio {
    pub fn open(port: Port) -> Result<Self, Error> {
        let pins = port.mask()?;
        if !cfg!(target_arch = "arm") {
            return Err(Error::Unsupported);
        }
        let lease = try_claim_pio(port.block, pins).ok_or(Error::Busy)?;
        let mut engine = Engine::new(Hardware, port, Some(lease));
        engine.initialize()?;
        Ok(Self(engine))
    }
}
impl ProgrammableIo for Rp2040Pio {
    fn configure(&mut self, m: u8, p: &[Instruction], c: Config) -> Result<(), Error> {
        self.0.configure(m, p, c)
    }
    fn start(&mut self, m: u8) -> Result<(), Error> {
        self.0.start(m)
    }
    fn try_write(&mut self, m: u8, w: u32) -> Result<(), Error> {
        self.0.try_write(m, w)
    }
    fn try_read(&mut self, m: u8) -> Result<u32, Error> {
        self.0.try_read(m)
    }
    fn exchange(&mut self, m: u8, w: u32, t: u32) -> Result<u32, Error> {
        self.0.exchange(m, w, t)
    }
    fn cancel(&mut self, m: u8) -> Result<(), Error> {
        self.0.cancel(m)
    }
    fn reset(&mut self) {
        self.0.reset();
    }
}

#[cfg(test)]
mod tests;
