// SPDX-License-Identifier: Apache-2.0

//! RP2040 SIO primitives and the ROM core-1 launch protocol.

#[cfg(target_arch = "arm")]
use core::ptr::{read_volatile, write_volatile};

#[cfg(any(target_arch = "arm", test))]
const CORE1_LAUNCH_COMMANDS: usize = 6;
#[cfg(target_arch = "arm")]
const MAX_PSM_POLLS: usize = 1_000_000;
#[cfg(target_arch = "arm")]
const MAX_FIFO_POLLS: usize = 1_000_000;
#[cfg(target_arch = "arm")]
const MAX_LAUNCH_EXCHANGES: usize = 64;

#[cfg(target_arch = "arm")]
const SIO_BASE: usize = super::SIO_BASE as usize;
#[cfg(target_arch = "arm")]
const SIO_CPUID: *const u32 = SIO_BASE as *const u32;
#[cfg(target_arch = "arm")]
const SIO_FIFO_ST: *mut u32 = (SIO_BASE + 0x50) as *mut u32;
#[cfg(target_arch = "arm")]
const SIO_FIFO_WR: *mut u32 = (SIO_BASE + 0x54) as *mut u32;
#[cfg(target_arch = "arm")]
const SIO_FIFO_RD: *const u32 = (SIO_BASE + 0x58) as *const u32;

#[cfg(target_arch = "arm")]
const PSM_BASE: usize = 0x4001_0000;
#[cfg(target_arch = "arm")]
const PSM_FRCE_OFF: *mut u32 = (PSM_BASE + 0x04) as *mut u32;
#[cfg(target_arch = "arm")]
const PSM_FRCE_OFF_SET: *mut u32 = (PSM_BASE + 0x2004) as *mut u32;
#[cfg(target_arch = "arm")]
const PSM_FRCE_OFF_CLR: *mut u32 = (PSM_BASE + 0x3004) as *mut u32;

#[cfg(target_arch = "arm")]
const FIFO_ST_VLD: u32 = 1 << 0;
#[cfg(target_arch = "arm")]
const FIFO_ST_RDY: u32 = 1 << 1;
#[cfg(target_arch = "arm")]
const FIFO_ST_WOF: u32 = 1 << 2;
#[cfg(target_arch = "arm")]
const FIFO_ST_ROE: u32 = 1 << 3;
#[cfg(target_arch = "arm")]
const PSM_PROC1: u32 = 1 << 16;

/// SIO FIFO interrupt on core 0.
pub const IRQ_SIO_PROC0: u8 = 15;
/// SIO FIFO interrupt on core 1.
pub const IRQ_SIO_PROC1: u8 = 16;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LaunchError {
    Core1DidNotReset,
    FifoWriteTimeout,
    FifoReadTimeout,
    ProtocolDidNotSynchronize,
}

/// The core currently executing this call.
#[cfg(target_arch = "arm")]
pub fn core_id() -> u8 {
    unsafe { (read_volatile(SIO_CPUID) & 1) as u8 }
}

/// Read one word if the other core has put data in the SIO FIFO.
#[cfg(target_arch = "arm")]
pub fn fifo_try_pop() -> Option<u32> {
    unsafe {
        if read_volatile(SIO_FIFO_ST) & FIFO_ST_VLD == 0 {
            None
        } else {
            Some(read_volatile(SIO_FIFO_RD))
        }
    }
}

/// Send one word to the other core without waiting for FIFO space.
#[cfg(target_arch = "arm")]
pub fn fifo_try_push(value: u32) -> Result<(), u32> {
    unsafe {
        if read_volatile(SIO_FIFO_ST) & FIFO_ST_RDY == 0 {
            return Err(value);
        }
        write_volatile(SIO_FIFO_WR, value);
        core::arch::asm!("sev", options(nomem, nostack, preserves_flags));
    }
    Ok(())
}

/// Clear sticky FIFO overflow and underflow indicators for the current core.
#[cfg(target_arch = "arm")]
pub fn fifo_clear_errors() {
    unsafe { write_volatile(SIO_FIFO_ST, FIFO_ST_WOF | FIFO_ST_ROE) }
}

#[cfg(target_arch = "arm")]
fn fifo_drain() {
    while fifo_try_pop().is_some() {}
    fifo_clear_errors();
}

#[cfg(target_arch = "arm")]
fn fifo_push_bounded(value: u32) -> Result<(), LaunchError> {
    for _ in 0..MAX_FIFO_POLLS {
        if fifo_try_push(value).is_ok() {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(LaunchError::FifoWriteTimeout)
}

#[cfg(target_arch = "arm")]
fn fifo_pop_bounded() -> Result<u32, LaunchError> {
    for _ in 0..MAX_FIFO_POLLS {
        if let Some(value) = fifo_try_pop() {
            return Ok(value);
        }
        core::hint::spin_loop();
    }
    Err(LaunchError::FifoReadTimeout)
}

/// Reset core 1, then start it through the boot ROM's exact FIFO handshake.
///
/// # Safety
///
/// This must run on core 0 while no other code uses the SIO FIFO. `stack_top`
/// must be 8-byte aligned and point past writable core-1 stack storage.
/// `entry` must be a Thumb function address; bit zero is supplied here. The
/// caller must keep the vector table and stack alive for core 1.
#[cfg(target_arch = "arm")]
pub unsafe fn launch_core1(
    vector_table: u32,
    stack_top: u32,
    entry: u32,
) -> Result<(), LaunchError> {
    unsafe {
        write_volatile(PSM_FRCE_OFF_SET, PSM_PROC1);
        let mut reset_seen = false;
        for _ in 0..MAX_PSM_POLLS {
            if read_volatile(PSM_FRCE_OFF) & PSM_PROC1 != 0 {
                reset_seen = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !reset_seen {
            return Err(LaunchError::Core1DidNotReset);
        }
        // A core reset does not empty words that core 1 already queued for
        // core 0. Drain them while core 1 cannot produce more, so the next
        // word is the boot ROM's reset acknowledgment.
        fifo_drain();
        write_volatile(PSM_FRCE_OFF_CLR, PSM_PROC1);
    }

    if fifo_pop_bounded()? != 0 {
        return Err(LaunchError::ProtocolDidNotSynchronize);
    }

    let commands = [0, 0, 1, vector_table, stack_top, entry | 1];
    let mut protocol = LaunchProtocol::new(commands);
    for _ in 0..MAX_LAUNCH_EXCHANGES {
        let command = protocol.command();
        if command == 0 {
            fifo_drain();
            unsafe { core::arch::asm!("sev", options(nomem, nostack, preserves_flags)) };
        }
        fifo_push_bounded(command)?;
        if protocol.accept_echo(fifo_pop_bounded()?) {
            return Ok(());
        }
    }
    Err(LaunchError::ProtocolDidNotSynchronize)
}

/// Hold core 1 in reset before a chip-wide ROM transition.
///
/// # Safety
/// Call only from core 0 when core 1 is no longer expected to release locks
/// or publish results. The stopped core cannot run cleanup.
#[cfg(target_arch = "arm")]
pub unsafe fn stop_core1() -> Result<(), LaunchError> {
    unsafe { write_volatile(PSM_FRCE_OFF_SET, PSM_PROC1) };
    for _ in 0..MAX_PSM_POLLS {
        if unsafe { read_volatile(PSM_FRCE_OFF) } & PSM_PROC1 != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(LaunchError::Core1DidNotReset)
}

#[cfg(any(target_arch = "arm", test))]
struct LaunchProtocol {
    commands: [u32; CORE1_LAUNCH_COMMANDS],
    next: usize,
}

#[cfg(any(target_arch = "arm", test))]
impl LaunchProtocol {
    const fn new(commands: [u32; CORE1_LAUNCH_COMMANDS]) -> Self {
        Self { commands, next: 0 }
    }

    fn command(&self) -> u32 {
        self.commands[self.next]
    }

    fn accept_echo(&mut self, echo: u32) -> bool {
        if echo == self.command() {
            self.next += 1;
        } else {
            self.next = 0;
        }
        self.next == self.commands.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_protocol_requires_every_exact_echo() {
        let mut protocol = LaunchProtocol::new([0, 0, 1, 2, 3, 5]);
        for echo in [0, 0, 1, 2, 3] {
            assert!(!protocol.accept_echo(echo));
        }
        assert!(protocol.accept_echo(5));
    }

    #[test]
    fn wrong_echo_restarts_the_sequence() {
        let mut protocol = LaunchProtocol::new([0, 0, 1, 2, 3, 5]);
        assert!(!protocol.accept_echo(0));
        assert!(!protocol.accept_echo(7));
        assert_eq!(protocol.command(), 0);
    }

    #[test]
    fn launch_sequence_matches_the_rom_contract() {
        let vector_table = 0x1000_0100;
        let stack_top = 0x2004_0000;
        let entry = 0x1000_1000;
        let protocol = LaunchProtocol::new([0, 0, 1, vector_table, stack_top, entry | 1]);
        assert_eq!(
            protocol.commands,
            [0, 0, 1, vector_table, stack_top, 0x1000_1001]
        );
    }
}
