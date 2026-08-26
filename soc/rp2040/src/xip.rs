// SPDX-License-Identifier: Apache-2.0

//! Two-core exclusion for operations which temporarily remove flash/XIP.
//!
//! The existing SIO IRQ must call `service_request` after draining its FIFO.
//! Spinlock 28 serializes writers; 29 is entropy, 30 DMA, 31 device ownership,
//! and 14 the scheduler critical section. Never hold a peer's lock while parking.
//! Only the acknowledged victim runs without XIP, entirely from copied SRAM.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidContext,
    Busy,
    PeerTimeout,
    DmaActive,
}

#[cfg(any(target_arch = "arm", test))]
fn next_generation(previous: u32) -> u32 {
    previous.wrapping_add(2).max(2)
}

#[cfg(target_arch = "arm")]
mod hardware {
    use super::Error;
    use core::sync::atomic::{AtomicU32, Ordering};

    static REQUEST: AtomicU32 = AtomicU32::new(0);
    static ACK: AtomicU32 = AtomicU32::new(0);
    // Accessed only while spinlock 28 is owned.
    static mut GENERATION: u32 = 0;
    const LOCK: *mut u32 = 0xd000_0170 as *mut u32;

    /// A task-local guard; never transfer it to the other core.
    pub struct Guard(core::marker::PhantomData<*mut ()>);

    impl Guard {
        /// Park the other core, disable local IRQs and refuse active DMA.
        ///
        /// # Safety
        /// Both cores must use the cooperating SIO IRQ handler. No NMI,
        /// debugger, or other bus master may fetch XIP while the guard lives.
        /// Call only from a task, outside all critical sections. Restore XIP
        /// before dropping the guard; a hung ROM call must reset, not unpark.
        pub unsafe fn acquire() -> Result<Self, Error> {
            let mask: u32;
            let exception: u32;
            core::arch::asm!("mrs {}, PRIMASK", out(reg) mask, options(nomem, nostack));
            core::arch::asm!("mrs {}, IPSR", out(reg) exception, options(nomem, nostack));
            if mask != 0 || exception != 0 {
                return Err(Error::InvalidContext);
            }
            core::arch::asm!("cpsid i", options(nostack));
            if LOCK.read_volatile() == 0 {
                core::arch::asm!("cpsie i", options(nostack));
                return Err(Error::Busy);
            }
            core::arch::asm!("dmb", options(nostack));
            let guard = Self(core::marker::PhantomData);
            // A fresh generation prevents a late ACK from a timed-out request
            // from authorizing a later writer. Bit zero identifies the victim.
            let generation = core::ptr::addr_of_mut!(GENERATION);
            let next = super::next_generation(generation.read());
            generation.write(next);
            let request = next | u32::from(crate::multicore::core_id() ^ 1);
            REQUEST.store(request, Ordering::Release);
            let _ = crate::multicore::fifo_try_push(0x4658_4950);
            let start = crate::timer_us();
            while ACK.load(Ordering::Acquire) != request {
                if crate::timer_us().wrapping_sub(start) >= 50_000 {
                    drop(guard);
                    return Err(Error::PeerTimeout);
                }
            }
            // Once both cores are excluded no normal driver can start DMA.
            // Conservatively reject all channels, even SRAM-only transfers.
            for channel in 0..12 {
                let ctrl = (crate::DMA_BASE + channel * 0x40 + 0x0c) as *const u32;
                if ctrl.read_volatile() & (1 << 24) != 0 {
                    drop(guard);
                    return Err(Error::DmaActive);
                }
            }
            Ok(guard)
        }
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            REQUEST.store(0, Ordering::Release);
            unsafe {
                core::arch::asm!("dmb", options(nostack));
                LOCK.write_volatile(1);
                core::arch::asm!("cpsie i", options(nostack));
            }
        }
    }

    /// Cooperating half of the lockout. Called by each core's SIO IRQ.
    ///
    /// No timeout here: returning while the writer still has XIP disabled is
    /// unsafe. The writer installs a hardware watchdog before removing XIP.
    #[inline(never)]
    #[link_section = ".ram_func.xip_park"]
    pub fn service_request() {
        let request = REQUEST.load(Ordering::Acquire);
        let core = unsafe { (0xd000_0000 as *const u32).read_volatile() };
        if request == 0 || request & 1 != core {
            return;
        }
        unsafe {
            let mask: u32;
            core::arch::asm!("mrs {}, PRIMASK", "cpsid i", out(reg) mask, options(nostack));
            if REQUEST.load(Ordering::Acquire) == request {
                ACK.store(request, Ordering::Release);
                while REQUEST.load(Ordering::Acquire) == request {
                    core::arch::asm!("nop", options(nomem, nostack));
                }
                ACK.store(0, Ordering::Release);
            }
            core::arch::asm!("msr PRIMASK, {}", in(reg) mask, options(nostack));
        }
    }
}

#[cfg(target_arch = "arm")]
pub use hardware::{service_request, Guard};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn generations_skip_idle_and_reserve_the_victim_bit() {
        for previous in [0, 2, 4, 0xffff_fffc, 0xffff_fffe] {
            let next = next_generation(previous);
            assert_ne!(next, 0);
            assert_eq!(next & 1, 0);
            assert_ne!(next, previous);
            assert_ne!(next | 1, previous | 1);
        }
    }
}
