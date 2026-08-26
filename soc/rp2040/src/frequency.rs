// SPDX-License-Identifier: Apache-2.0

//! Exclusive FC0 measurement of clk_sys against the board's 12 MHz crystal.
//!
//! Pico SDK 2.1.1 hardware_clocks/clocks.c supplies the sequence; generated
//! clocks.h supplies offsets/status bits. Interval 10 is about 1 ms with a
//! documented 2 kHz uncertainty, not a calibration of the crystal itself.
//! Spinlock 27 is reserved here (28 XIP, 29 entropy, 30 DMA, 31 device claims).
//! Acquisition never waits, including from a nested interrupt on the same core.

#[cfg(any(target_arch = "arm", test))]
mod counter {
    pub const REF_CTRL: u32 = 0x30;
    pub const REF_DIV: u32 = 0x34;
    pub const REF_SELECTED: u32 = 0x38;
    pub const REF_KHZ: u32 = 0x80;
    pub const MIN_KHZ: u32 = 0x84;
    pub const MAX_KHZ: u32 = 0x88;
    pub const DELAY: u32 = 0x8c;
    pub const INTERVAL: u32 = 0x90;
    pub const SOURCE: u32 = 0x94;
    pub const STATUS: u32 = 0x98;
    pub const RESULT: u32 = 0x9c;
    pub const DONE: u32 = 1 << 4;
    pub const BUSY: u32 = (1 << 8) | (1 << 12);
    pub const FAILED: u32 = (1 << 16) | (1 << 20) | (1 << 24) | (1 << 28);
    pub const TIMEOUT_US: u32 = 10_000;
    // Independent escape if the timer/reference stops. No polling loop relies
    // solely on the clock it is checking. This is a work bound, not wall time.
    pub const MAX_POLLS: u32 = 100_000;

    pub trait Counter {
        fn claim(&mut self) -> bool;
        fn release(&mut self);
        fn read(&mut self, offset: u32) -> u32;
        fn write(&mut self, offset: u32, value: u32);
        fn now_us(&mut self) -> u32;
    }

    fn known_reference(io: &mut impl Counter) -> bool {
        io.read(REF_CTRL) & 3 == 2 && io.read(REF_SELECTED) == 4 && io.read(REF_DIV) == 0x100
    }

    pub fn result_hz(raw: u32) -> Option<u32> {
        if raw & !0x3fff_ffff != 0 {
            return None;
        }
        let hz = u32::try_from(u64::from(raw) * 1_000 / 32).ok()?;
        // Current supported operating range, not an overclocking interface.
        (1_000_000..=133_000_000).contains(&hz).then_some(hz)
    }

    fn poll(io: &mut impl Counter, started: u32, finished: bool) -> Option<u32> {
        for _ in 0..MAX_POLLS {
            if io.now_us().wrapping_sub(started) >= TIMEOUT_US {
                return None;
            }
            let status = io.read(STATUS);
            if status & BUSY == 0 && (!finished || status & DONE != 0) {
                return Some(status);
            }
            core::hint::spin_loop();
        }
        None
    }

    pub fn measure(io: &mut impl Counter) -> Option<u32> {
        if !io.claim() {
            return None;
        }
        let answer = measure_owned(io);
        io.release();
        answer
    }

    fn measure_owned(io: &mut impl Counter) -> Option<u32> {
        if !known_reference(io) {
            return None;
        }
        let started = io.now_us();
        // Do not overwrite a running/waiting measurement inherited from other
        // firmware. SDK notes the counter can run even with SOURCE=NULL.
        poll(io, started, false)?;
        io.write(REF_KHZ, crate::XOSC_HZ / 1_000);
        io.write(MIN_KHZ, 0);
        io.write(MAX_KHZ, 0x01ff_ffff);
        io.write(DELAY, 1);
        io.write(INTERVAL, 10);
        io.write(SOURCE, 9); // clk_sys, including its divider, not raw PLL_SYS.
        let answer = poll(io, started, true).and_then(|status| {
            if status & FAILED != 0 || !known_reference(io) {
                return None;
            }
            result_hz(io.read(RESULT))
        });
        // A NULL write can itself run the counter briefly. The next call waits
        // for idle; it must never mistake this operation's DONE for a new one.
        io.write(SOURCE, 0);
        answer
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        extern crate std;
        use std::vec::Vec;

        struct Fake {
            locked: bool,
            reference: [u32; 3],
            reference_changes: bool,
            initial_status: u32,
            final_status: u32,
            raw: u32,
            time: u32,
            step: u32,
            started: bool,
            reads: u32,
            result_reads: u32,
            released: u32,
            writes: Vec<(u32, u32)>,
        }
        impl Default for Fake {
            fn default() -> Self {
                Self {
                    locked: false,
                    reference: [2, 0x100, 4],
                    reference_changes: false,
                    initial_status: DONE,
                    final_status: DONE | 1,
                    raw: 125_000 << 5,
                    time: 0,
                    step: 1,
                    started: false,
                    reads: 0,
                    result_reads: 0,
                    released: 0,
                    writes: Vec::new(),
                }
            }
        }
        impl Counter for Fake {
            fn claim(&mut self) -> bool {
                if self.locked {
                    return false;
                }
                self.locked = true;
                true
            }
            fn release(&mut self) {
                self.locked = false;
                self.released += 1;
            }
            fn now_us(&mut self) -> u32 {
                self.time = self.time.wrapping_add(self.step);
                self.time
            }
            fn read(&mut self, offset: u32) -> u32 {
                self.reads += 1;
                match offset {
                    REF_CTRL => {
                        if self.started && self.reference_changes {
                            0
                        } else {
                            self.reference[0]
                        }
                    }
                    REF_DIV => self.reference[1],
                    REF_SELECTED => self.reference[2],
                    STATUS => {
                        if self.started {
                            self.final_status
                        } else {
                            self.initial_status
                        }
                    }
                    RESULT => {
                        self.result_reads += 1;
                        self.raw
                    }
                    _ => panic!("unexpected read"),
                }
            }
            fn write(&mut self, offset: u32, value: u32) {
                self.writes.push((offset, value));
                if offset == SOURCE {
                    self.started = value != 0;
                }
            }
        }

        #[test]
        fn converts_fixed_point_khz_without_overflow_or_reserved_bits() {
            assert_eq!(result_hz(12_000 << 5), Some(12_000_000));
            assert_eq!(result_hz((125_000 << 5) | 16), Some(125_000_500));
            assert_eq!(result_hz(1_000 << 5), Some(1_000_000));
            assert_eq!(result_hz(133_000 << 5), Some(133_000_000));
            for raw in [0, 999 << 5, 133_001 << 5, 0x3fff_ffff, u32::MAX] {
                assert_eq!(result_hz(raw), None);
            }
        }
        #[test]
        fn programs_vendor_sequence_and_releases_after_each_measurement() {
            let mut io = Fake::default();
            assert_eq!(measure(&mut io), Some(125_000_000));
            assert_eq!(
                io.writes,
                [
                    (REF_KHZ, 12_000),
                    (MIN_KHZ, 0),
                    (MAX_KHZ, 0x01ff_ffff),
                    (DELAY, 1),
                    (INTERVAL, 10),
                    (SOURCE, 9),
                    (SOURCE, 0)
                ]
            );
            assert!(!io.locked);
            assert_eq!(io.released, 1);
            io.raw = 12_000 << 5;
            assert_eq!(measure(&mut io), Some(12_000_000));
            assert_eq!(io.released, 2);
        }
        #[test]
        fn competing_owner_is_not_touched_or_released() {
            let mut io = Fake {
                locked: true,
                ..Fake::default()
            };
            assert_eq!(measure(&mut io), None);
            assert_eq!((io.reads, io.released), (0, 0));
            assert!(io.writes.is_empty());
            assert!(io.locked);
        }
        #[test]
        fn unknown_or_divided_reference_is_not_reported_as_measured() {
            for reference in [[0, 0x100, 1], [2, 0x200, 4], [2, 0x100, 0]] {
                let mut io = Fake {
                    reference,
                    ..Fake::default()
                };
                assert_eq!(measure(&mut io), None);
                assert!(io.writes.is_empty());
                assert_eq!(io.released, 1);
            }
        }
        #[test]
        fn inherited_busy_or_waiting_counter_has_a_deadline_and_is_not_overwritten() {
            for status in [1 << 8, 1 << 12] {
                let mut io = Fake {
                    initial_status: status,
                    step: 1_000,
                    ..Fake::default()
                };
                assert_eq!(measure(&mut io), None);
                assert!(io.writes.is_empty());
                assert_eq!(io.released, 1);
            }
        }
        #[test]
        fn stopped_timer_cannot_make_either_poll_unbounded() {
            for initial in [true, false] {
                let mut io = Fake {
                    initial_status: if initial { BUSY } else { DONE },
                    final_status: BUSY,
                    step: 0,
                    ..Fake::default()
                };
                assert_eq!(measure(&mut io), None);
                assert!(io.reads <= MAX_POLLS + 8);
                assert_eq!(io.result_reads, 0);
                assert_eq!(io.released, 1);
                if !initial {
                    assert_eq!(io.writes.last(), Some(&(SOURCE, 0)));
                }
            }
        }
        #[test]
        fn stale_done_is_not_used_after_start_and_timeout_releases_the_counter() {
            let mut io = Fake {
                final_status: BUSY,
                step: 1_000,
                ..Fake::default()
            };
            assert_eq!(measure(&mut io), None);
            assert_eq!(io.result_reads, 0);
            assert_eq!(io.writes.last(), Some(&(SOURCE, 0)));
            io.final_status = DONE | 1;
            assert_eq!(measure(&mut io), Some(125_000_000));
        }
        #[test]
        fn failed_or_stopped_clock_never_exposes_a_stale_result() {
            for flag in [1 << 16, 1 << 20, 1 << 24, 1 << 28] {
                let mut io = Fake {
                    final_status: DONE | flag,
                    ..Fake::default()
                };
                assert_eq!(measure(&mut io), None);
                assert_eq!(io.result_reads, 0);
                assert_eq!(io.writes.last(), Some(&(SOURCE, 0)));
                assert!(!io.locked);
            }
        }
        #[test]
        fn wrapping_timer_is_supported() {
            let mut io = Fake {
                time: u32::MAX - 1,
                ..Fake::default()
            };
            assert_eq!(measure(&mut io), Some(125_000_000));
        }

        #[test]
        fn changed_reference_discards_an_otherwise_valid_count() {
            let mut io = Fake {
                reference_changes: true,
                ..Fake::default()
            };
            assert_eq!(measure(&mut io), None);
            assert_eq!(io.result_reads, 0);
            assert_eq!(io.writes.last(), Some(&(SOURCE, 0)));
            assert_eq!(io.released, 1);
        }

        #[test]
        fn zero_or_implausible_result_releases_ownership_and_allows_retry() {
            let mut io = Fake {
                raw: 0,
                ..Fake::default()
            };
            assert_eq!(measure(&mut io), None);
            assert_eq!(io.writes.last(), Some(&(SOURCE, 0)));
            io.raw = 134_000 << 5;
            assert_eq!(measure(&mut io), None);
            io.raw = 125_000 << 5;
            assert_eq!(measure(&mut io), Some(125_000_000));
            assert_eq!(io.released, 3);
        }
    }
}

#[cfg(target_arch = "arm")]
mod hardware {
    use super::counter::Counter;
    const LOCK: *mut u32 = (crate::SIO_BASE + 0x100 + 27 * 4) as *mut u32;
    pub struct Hardware;
    impl Counter for Hardware {
        fn claim(&mut self) -> bool {
            unsafe {
                if LOCK.read_volatile() == 0 {
                    return false;
                }
                core::arch::asm!("dmb", options(nostack));
            }
            true
        }
        fn release(&mut self) {
            unsafe {
                core::arch::asm!("dmb", options(nostack));
                LOCK.write_volatile(1);
            }
        }
        fn read(&mut self, offset: u32) -> u32 {
            unsafe { ((crate::CLOCKS_BASE + offset) as *const u32).read_volatile() }
        }
        fn write(&mut self, offset: u32, value: u32) {
            unsafe { ((crate::CLOCKS_BASE + offset) as *mut u32).write_volatile(value) };
        }
        fn now_us(&mut self) -> u32 {
            crate::timer_us()
        }
    }
    /// Clear a lock inherited across reset before any counter user can exist.
    pub unsafe fn init() {
        unsafe {
            LOCK.write_volatile(1);
            core::arch::asm!("dmb", options(nostack));
        }
    }
}

/// Called only by single-core SoC clock setup before measurement/consumers.
#[cfg(target_arch = "arm")]
pub(crate) unsafe fn init() {
    unsafe { hardware::init() };
}

pub(crate) fn measure_cpu_hz() -> Option<u32> {
    #[cfg(target_arch = "arm")]
    {
        counter::measure(&mut hardware::Hardware)
    }
    #[cfg(not(target_arch = "arm"))]
    {
        None
    }
}

#[cfg(test)]
mod host_test {
    #[test]
    fn host_does_not_invent_a_measurement() {
        use hal::soc::SystemOnChip;
        assert_eq!(
            crate::Rp2040::measure_cpu_hz(|| panic!("not an ARM cycle counter")),
            None
        );
    }
}
