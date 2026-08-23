// SPDX-License-Identifier: Apache-2.0

//! I2C bus abstraction.
//!
//! Two types, because a controller and a device are not the same thing:
//!
//! - [`I2cController`] owns the physical driver `P` (by value) and speaks to
//!   the whole bus: it can [`scan`](I2cController::scan) the address space and
//!   hand out a [`device`](I2cController::device) for one slave.
//! - [`I2cDevice`] borrows the controller, bakes in one slave address, and is
//!   the [`Bus`] a logical driver talks through — every op it frames carries
//!   that address, unshifted, as `tx[0]`.
//!
//! Before this split a single `I2cBus` baked one address in, so a bus scan or
//! a second device had to bypass Layer 2 and call `exchange` with the
//! `tx[0]`-is-address convention by hand — exactly the raw-transfer reach-round
//! the layering exists to stop. `scan()` and `device(addr)` keep it on the
//! near side of the boundary.
//!
//! # Ownership
//!
//! The physical driver is held by value, not behind a `&'static dyn`, so one
//! `Once<I2cController<Esp32I2c>>` holds the whole stack. A caller with a
//! `&'static` driver can still pass it: `&T` is itself a [`PhysicalTransfer`]
//! (the blanket impl in `hal::bus`).
//!
//! # Locking (#116)
//!
//! Both the per-op transfer and each scan probe take an [`api::mutex`] lock
//! around the physical exchange, and [`I2cDevice`] holds it for the **whole**
//! op list so a compound transaction (write-then-read) is never split once a
//! board hands the same `&'static I2cController` to more than one task. That
//! lock is the kernel's priority-inheritance mutex, a syscall the kernel
//! **refuses from interrupt context** — so nothing here may be called from an
//! ISR. An interrupt that must touch the peripheral should own the physical
//! driver directly (`exchange` is `&self`) and coordinate with the task side
//! through a `CsCell`.

#![no_std]

use api::bus::{spin_rough_us, Bus, BusError, BusKind, BusResult, Op, PhysicalTransfer};
use api::mutex::{lock, Mutex};

/// Largest op payload, bounded by the controller's FIFO.
const MAX_PAYLOAD: usize = 64;

/// An I2C controller: owns the physical driver and addresses the whole bus.
pub struct I2cController<P: PhysicalTransfer> {
    phys: Mutex<P>,
}

impl<P: PhysicalTransfer> I2cController<P> {
    /// Create a controller, taking ownership of the physical driver.
    ///
    /// The driver is expected to be already `init`ed (pins routed, clock
    /// gated on); this wrapper only moves bytes.
    pub const fn new(phys: P) -> Self {
        Self { phys: Mutex::new(phys) }
    }

    /// A handle to one slave at `addr` (7-bit, unshifted). The returned
    /// [`I2cDevice`] is the [`Bus`] a logical driver talks through, and borrows
    /// the controller for its lifetime — so a second device, or a scan, is a
    /// second borrow rather than a second address baked into a fresh bus.
    pub fn device(&self, addr: u8) -> I2cDevice<'_, P> {
        I2cDevice { ctrl: self, addr }
    }

    /// Walk the 7-bit address space `0x08..=0x77` and call `on_found` for each
    /// address that acknowledges, returning how many did.
    ///
    /// A zero-length write is the conventional probe: it addresses the device
    /// and stops, so a present device ACKs and an absent one NAKs without any
    /// register being touched. This is the scan the `imu` app used to open-code
    /// against the raw physical driver.
    pub fn scan(&self, mut on_found: impl FnMut(u8)) -> usize {
        let mut found = 0;
        for addr in 0x08..=0x77u8 {
            // `tx[0]` is the 7-bit address, unshifted -- the physical driver
            // adds the R/W bit. See `hal::bus::PhysicalTransfer::exchange`.
            if lock(&self.phys).exchange(&[addr], &mut []).is_ok() {
                on_found(addr);
                found += 1;
            }
        }
        found
    }
}

/// One slave on an [`I2cController`], addressed by `addr`. This is the [`Bus`]
/// a logical driver is handed.
pub struct I2cDevice<'a, P: PhysicalTransfer> {
    ctrl: &'a I2cController<P>,
    addr: u8,
}

impl<P: PhysicalTransfer> Bus for I2cDevice<'_, P> {
    // Every op passes the address UNSHIFTED as `tx[0]`; the physical driver
    // adds the R/W bit. See `hal::bus::PhysicalTransfer::exchange`.
    fn transfer(&self, ops: &mut [Op]) -> BusResult<()> {
        // Hold the bus for the whole op list, so a write-then-read transaction
        // cannot be split by another task (#116).
        let phys = lock(&self.ctrl.phys);
        for op in ops.iter_mut() {
            if op.word_bits != 8 {
                return Err(BusError::InvalidConfig);
            }
            match (op.tx, op.rx.as_deref_mut()) {
                // Write (optionally with a repeated-start read): address, then
                // the payload; the caller's `rx` is what gets filled.
                (Some(tx), rx_opt) => {
                    // A frame is one addressed transaction and cannot be split
                    // without a fresh START, so a payload past the controller
                    // FIFO is refused rather than cut short (#98).
                    if tx.len() > MAX_PAYLOAD {
                        return Err(BusError::InvalidConfig);
                    }
                    let mut buf = [0u8; MAX_PAYLOAD + 1];
                    let len = tx.len();
                    buf[0] = self.addr;
                    buf[1..=len].copy_from_slice(&tx[..len]);
                    match rx_opt {
                        Some(rx) => phys.exchange(&buf[..=len], rx)?,
                        None => phys.exchange(&buf[..=len], &mut [])?,
                    }
                }
                // Plain read: address only, no data bytes — not a zero-length
                // write, which would address the I2C general-call address.
                (None, Some(rx)) => phys.exchange(&[self.addr], rx)?,
                (None, None) => {}
            }
            // I2C has no separate chip-select line; `op.cs` is not meaningful.
            if op.delay_us > 0 {
                spin_rough_us(op.delay_us);
            }
        }
        Ok(())
    }

    fn max_transfer(&self) -> usize {
        MAX_PAYLOAD
    }

    fn kind(&self) -> BusKind {
        BusKind::I2c
    }

    // I2C clock is fixed at init on this controller; `set_speed` keeps the
    // trait default (`InvalidConfig`).
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use api::bus::{BusConfig, PhysicalBus};
    use core::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;
    use std::vec::Vec;

    /// Records what reached the physical layer, and hands back canned bytes.
    ///
    /// The mock this replaced echoed `tx` into `rx`, and the tests only
    /// asserted `is_ok()`. That cannot tell a correct address from a doubled
    /// one, which is why the bus layer and the physical driver disagreed about
    /// shifting for as long as they did -- each was tested against a mock that
    /// shared its own author's assumption.
    struct Recorder {
        // StdMutex, not RefCell: `PhysicalTransfer` is `Sync`.
        seen: StdMutex<Vec<u8>>,
        canned: Vec<u8>,
    }

    impl PhysicalBus for Recorder {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> {
            Ok(())
        }
    }

    impl PhysicalTransfer for Recorder {
        fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
            *self.seen.lock().unwrap() = tx.to_vec();
            let n = rx.len().min(self.canned.len());
            rx[..n].copy_from_slice(&self.canned[..n]);
            Ok(())
        }
    }

    fn controller_with(canned: &[u8]) -> (I2cController<Recorder>, ()) {
        (
            I2cController::new(Recorder { seen: StdMutex::new(Vec::new()), canned: canned.to_vec() }),
            (),
        )
    }

    /// The device-at-0x76 the older single-address `I2cBus` tests targeted.
    fn bus_with(canned: &[u8]) -> (I2cController<Recorder>, u8) {
        (
            I2cController::new(Recorder { seen: StdMutex::new(Vec::new()), canned: canned.to_vec() }),
            0x76,
        )
    }

    #[test]
    fn the_address_reaches_the_physical_layer_unshifted() {
        // The physical driver adds the R/W bit. Pre-shifting here sends 0xEC,
        // which that driver shifts again to 0xD8 -- an address no device
        // answers to, and a fault that looks like bad wiring.
        let (ctrl, addr) = bus_with(&[]);
        ctrl.device(addr).transfer(&mut [Op::write(&[0xF4, 0x27])]).unwrap();
        // Reach through the lock to inspect the mock's record.
        assert_eq!(lock(&ctrl.phys).seen.lock().unwrap()[0], 0x76, "address must not be pre-shifted");
    }

    #[test]
    fn all_three_shapes_address_the_device_the_same_way() {
        // This crate used to pre-shift in `write` and not in `transfer`.
        for (name, run) in [("write", 0), ("read", 1), ("exchange", 2)] {
            let (ctrl, addr) = bus_with(&[0xAA; 4]);
            let dev = ctrl.device(addr);
            let mut rx = [0u8; 2];
            match run {
                0 => dev.transfer(&mut [Op::write(&[0x01])]).unwrap(),
                1 => dev.transfer(&mut [Op::read(&mut rx)]).unwrap(),
                _ => dev.transfer(&mut [Op::exchange(&[0x01], &mut rx)]).unwrap(),
            }
            assert_eq!(lock(&ctrl.phys).seen.lock().unwrap()[0], 0x76, "{name} addressed differently");
        }
    }

    #[test]
    fn a_read_returns_the_bytes_to_the_caller() {
        // A read used to go into a throwaway buffer and get dropped, returning
        // Ok -- so a sensor driver saw zeros and no error.
        let (ctrl, addr) = bus_with(&[0xDE, 0xAD, 0xBE]);
        let mut buf = [0u8; 3];
        ctrl.device(addr).transfer(&mut [Op::read(&mut buf)]).unwrap();
        assert_eq!(buf, [0xDE, 0xAD, 0xBE]);
    }

    #[test]
    fn an_exchange_returns_the_bytes_to_the_caller() {
        let (ctrl, addr) = bus_with(&[0x12, 0x34]);
        let mut buf = [0u8; 2];
        ctrl.device(addr).transfer(&mut [Op::exchange(&[0xF7], &mut buf)]).unwrap();
        assert_eq!(buf, [0x12, 0x34]);
    }

    #[test]
    fn a_plain_read_sends_the_address_and_nothing_else() {
        // A zeroed tx used to be sent instead, addressing the I2C general-call
        // address 0x00 rather than the device.
        let (ctrl, addr) = bus_with(&[0; 4]);
        let mut buf = [0u8; 4];
        ctrl.device(addr).transfer(&mut [Op::read(&mut buf)]).unwrap();
        assert_eq!(&lock(&ctrl.phys).seen.lock().unwrap()[..], &[0x76], "read sends only the address");
    }

    #[test]
    fn a_write_carries_its_payload_after_the_address() {
        let (ctrl, addr) = bus_with(&[]);
        ctrl.device(addr).transfer(&mut [Op::write(&[0xF4, 0x27])]).unwrap();
        assert_eq!(&lock(&ctrl.phys).seen.lock().unwrap()[..], &[0x76, 0xF4, 0x27]);
    }

    #[test]
    fn a_write_past_the_fifo_is_refused_not_cut_short() {
        // Companion to #98: the first 64 bytes used to go out and the rest was
        // dropped with an Ok.
        let (ctrl, addr) = bus_with(&[]);
        let dev = ctrl.device(addr);
        let tx = [0x55u8; MAX_PAYLOAD + 1];
        assert_eq!(dev.transfer(&mut [Op::write(&tx)]), Err(BusError::InvalidConfig));
        assert!(lock(&ctrl.phys).seen.lock().unwrap().is_empty(), "nothing may reach the wire");
        let tx = [0x55u8; MAX_PAYLOAD];
        dev.transfer(&mut [Op::write(&tx)]).unwrap();
        assert_eq!(lock(&ctrl.phys).seen.lock().unwrap().len(), MAX_PAYLOAD + 1);
    }

    #[test]
    fn a_non_byte_word_is_rejected() {
        let (ctrl, addr) = bus_with(&[]);
        assert_eq!(
            ctrl.device(addr).transfer(&mut [Op::write(&[0x01]).with_word_bits(7)]),
            Err(BusError::InvalidConfig)
        );
    }

    /// A physical mock that answers every address, so `scan` finds the whole
    /// range, and records which addresses were probed.
    struct ScanMock {
        probed: StdMutex<Vec<u8>>,
    }

    impl PhysicalBus for ScanMock {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> {
            Ok(())
        }
    }

    impl PhysicalTransfer for ScanMock {
        fn exchange(&self, tx: &[u8], _rx: &mut [u8]) -> BusResult<()> {
            self.probed.lock().unwrap().push(tx[0]);
            Ok(())
        }
    }

    #[test]
    fn scan_probes_the_whole_range_unshifted_without_leaving_layer_two() {
        // The imu app used to walk the address space itself, calling the raw
        // physical driver with the tx[0]-is-address convention. `scan` keeps
        // that on the Layer-2 side of the boundary.
        let ctrl = I2cController::new(ScanMock { probed: StdMutex::new(Vec::new()) });
        let mut found = Vec::new();
        let n = ctrl.scan(|a| found.push(a));
        assert_eq!(n, 0x77 - 0x08 + 1);
        assert_eq!(found.first().copied(), Some(0x08));
        assert_eq!(found.last().copied(), Some(0x77));
        // Probed unshifted: the physical layer adds the R/W bit.
        assert_eq!(lock(&ctrl.phys).probed.lock().unwrap()[0], 0x08);
    }

    /// A mock that records the order in which the whole transaction's exchanges
    /// arrive, so a split can be seen. On the host the `api::mutex` is a
    /// pass-through (like `kernel::arch::host`'s critical section), so this
    /// asserts the *structural* guarantee — the wrapper delivers a multi-op
    /// transaction as one uninterrupted run under a single lock scope — rather
    /// than exercising cross-task blocking, which only the kernel mutex on
    /// target provides.
    struct OrderMock {
        log: StdMutex<Vec<u8>>,
        calls: AtomicUsize,
    }

    impl PhysicalBus for OrderMock {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> {
            Ok(())
        }
    }

    impl PhysicalTransfer for OrderMock {
        fn exchange(&self, tx: &[u8], _rx: &mut [u8]) -> BusResult<()> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            // Record the first payload byte after the address of each op.
            self.log.lock().unwrap().push(*tx.get(1).unwrap_or(&0));
            Ok(())
        }
    }

    #[test]
    fn a_compound_transaction_serializes_as_one_locked_run() {
        let ctrl = I2cController::new(OrderMock { log: StdMutex::new(Vec::new()), calls: AtomicUsize::new(0) });
        let dev = ctrl.device(0x68);
        // Two ops in one transfer: a write of 0x11 then a write of 0x22. Each
        // reaches the physical layer as [addr, payload], so the recorded
        // payload byte is tx[1]. They must arrive in order, both under the one
        // lock the transfer takes.
        dev.transfer(&mut [Op::write(&[0x11]), Op::write(&[0x22])]).unwrap();
        let g = lock(&ctrl.phys);
        assert_eq!(g.calls.load(Ordering::Relaxed), 2, "both ops ran");
        assert_eq!(&g.log.lock().unwrap()[..], &[0x11, 0x22], "in order, uninterrupted");
    }

    /// A mock that records the deepest concurrency it ever sees inside
    /// `exchange`. If the bus lock is doing its job, two tasks hammering the
    /// same controller never overlap and the depth stays 1.
    struct ConcurrencyMock {
        depth: AtomicUsize,
        max_depth: AtomicUsize,
    }

    impl PhysicalBus for ConcurrencyMock {
        fn init(&mut self, _: &BusConfig) -> BusResult<()> {
            Ok(())
        }
    }

    impl PhysicalTransfer for ConcurrencyMock {
        fn exchange(&self, _tx: &[u8], _rx: &mut [u8]) -> BusResult<()> {
            let d = self.depth.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_depth.fetch_max(d, Ordering::SeqCst);
            // A window in which an unserialized second caller would overlap.
            for _ in 0..2000 {
                core::hint::spin_loop();
            }
            self.depth.fetch_sub(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[test]
    fn two_tasks_on_one_controller_serialize() {
        // #116: once a board hands the same `&'static I2cController` to more
        // than one task, their transfers must not interleave. The host
        // `api::mutex` shim (below) is a real per-address lock, so two OS
        // threads here stand in for two tasks and the mock proves they never
        // overlap inside the physical exchange.
        let ctrl =
            I2cController::new(ConcurrencyMock { depth: AtomicUsize::new(0), max_depth: AtomicUsize::new(0) });
        std::thread::scope(|s| {
            for _ in 0..2 {
                s.spawn(|| {
                    let dev = ctrl.device(0x68);
                    for _ in 0..200 {
                        dev.transfer(&mut [Op::write(&[0x00, 0x01])]).unwrap();
                    }
                });
            }
        });
        assert_eq!(
            lock(&ctrl.phys).max_depth.load(Ordering::SeqCst),
            1,
            "two tasks overlapped inside the bus -- the lock did not serialize them"
        );
    }

    #[test]
    fn a_second_device_is_a_second_borrow_not_a_second_bus() {
        // Two devices share one controller; each frames its own address.
        let (ctrl, _) = controller_with(&[0xAA]);
        let a = ctrl.device(0x40);
        let b = ctrl.device(0x50);
        a.transfer(&mut [Op::write(&[0x01])]).unwrap();
        assert_eq!(lock(&ctrl.phys).seen.lock().unwrap()[0], 0x40);
        b.transfer(&mut [Op::write(&[0x02])]).unwrap();
        assert_eq!(lock(&ctrl.phys).seen.lock().unwrap()[0], 0x50);
    }
}

// Host stand-ins for the kernel syscalls `api::mutex` and `api::debug::panic`
// bottom out in. On a target the kernel provides these (`#[no_mangle] pub fn`
// in `kernel::syscall`); a host test binary links no kernel, so without these
// the linker cannot resolve `_flint_sys_mutex_lock` and friends. The mutex
// shim is a real per-address spinlock, so the serialization test above
// exercises genuine cross-thread blocking rather than a no-op.
#[cfg(test)]
mod host_syscall_shim {
    extern crate std;
    use core::ffi::c_void;
    use core::sync::atomic::{AtomicBool, Ordering};
    use std::boxed::Box;
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};

    /// One flag per `api::mutex::Mutex` address, created on first lock and
    /// leaked so it outlives every lock/unlock of that mutex.
    fn flag_for(key: usize) -> &'static AtomicBool {
        static TABLE: OnceLock<Mutex<HashMap<usize, &'static AtomicBool>>> = OnceLock::new();
        let table = TABLE.get_or_init(|| Mutex::new(HashMap::new()));
        table.lock().unwrap().entry(key).or_insert_with(|| Box::leak(Box::new(AtomicBool::new(false))))
    }

    #[no_mangle]
    extern "Rust" fn _flint_sys_mutex_lock(m: *const c_void) -> bool {
        let flag = flag_for(m as usize);
        while flag.swap(true, Ordering::Acquire) {
            std::thread::yield_now();
        }
        true
    }

    #[no_mangle]
    extern "Rust" fn _flint_sys_mutex_unlock(m: *const c_void) {
        flag_for(m as usize).store(false, Ordering::Release);
    }

    #[no_mangle]
    extern "Rust" fn _flint_sys_yield() {
        std::thread::yield_now();
    }

    #[no_mangle]
    extern "Rust" fn _flint_sys_panic(args: &core::fmt::Arguments<'_>) -> ! {
        std::panic!("{args}")
    }
}
