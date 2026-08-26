// SPDX-License-Identifier: Apache-2.0
//! Programmed digital I/O, not an addressed bus. Backends lower this small,
//! portable instruction set; applications never provide native opcodes.
//! This initial contract has one board-routed input and one output, explicit
//! 32-bit FIFOs, and no interrupt, DMA, side-set or arbitrary instruction access.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Busy,
    Invalid,
    NoSpace,
    NotConfigured,
    WouldBlock,
    Timeout,
    Hardware,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    /// Wait for a word, then load the output shift register (LSB first).
    Pull,
    /// Wait for FIFO space, then publish and clear the input shift register.
    Push,
    /// Shift out one bit onto the configured output.
    OutputBit,
    /// Sample one input bit into the MSB; shift existing bits right.
    InputBit,
    SetOutput(bool),
    WaitInput(bool),
    SetCounter(u8),
    Jump(u8),
    /// Jump if the counter was nonzero, then decrement it.
    JumpDecrement(u8),
    Nop,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Instruction {
    pub operation: Operation,
    /// Extra engine cycles after the instruction completes; at most 31.
    pub delay: u8,
}
impl Instruction {
    pub const fn new(operation: Operation, delay: u8) -> Self {
        Self { operation, delay }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Config {
    /// Maximum requested instruction rate. A backend may quantize downward;
    /// requests outside its divider range are rejected.
    pub frequency_hz: u32,
    pub input: bool,
    pub output: bool,
}

/// Exclusive owner of an engine and its board-routed pins. Machine indices
/// are local to this owner, not globally transferable handles. Every method
/// takes exclusive access; external synchronization belongs to the caller.
/// Programs wrap from their last instruction to their first. Configure reserves
/// program memory and a machine but leaves it stopped. Each routed pin can be
/// used by only one configured machine, even for input.
pub trait ProgrammableIo: Send {
    fn configure(
        &mut self,
        machine: u8,
        program: &[Instruction],
        config: Config,
    ) -> Result<(), Error>;
    fn start(&mut self, machine: u8) -> Result<(), Error>;
    fn try_write(&mut self, machine: u8, word: u32) -> Result<(), Error>;
    fn try_read(&mut self, machine: u8) -> Result<u32, Error>;
    /// Bounded send of one word, then receive one word in FIFO order. This is
    /// not a transaction tag: do not mix it with outstanding try_write traffic
    /// when a one-to-one response is required. On timeout cancel this machine, flush
    /// both FIFOs, release its allocation and tri-state its output. The caller
    /// must configure it again. timeout_us must be in 1..=1_000_000.
    fn exchange(&mut self, machine: u8, word: u32, timeout_us: u32) -> Result<u32, Error>;
    /// Stop and release one machine/program; other machines are undisturbed.
    fn cancel(&mut self, machine: u8) -> Result<(), Error>;
    /// Cancel every machine; keep the exclusive engine/pin lease for reuse.
    fn reset(&mut self);
}
