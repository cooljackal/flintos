// SPDX-License-Identifier: Apache-2.0
//! Native instructions and physical driver types are deliberately absent.
use hal::pio::{Config, Error, Instruction as I, Operation as O, ProgrammableIo};
use portable_atomic::{AtomicU32, Ordering};

// state, nonce, words, blocks, timeout recoveries, collision rejections,
// FIFO-full rejections, FIFO-empty rejections, reopen count, CPU Hz.
#[no_mangle]
static PIO_RESULTS: [AtomicU32; 10] = [const { AtomicU32::new(0) }; 10];
#[no_mangle]
static PIO_NONCE: AtomicU32 = AtomicU32::new(0);
const CFG: Config = Config {
    frequency_hz: 1_000_000,
    input: true,
    output: true,
};
// Serialize a complete word LSB first and collect each physical input bit.
const LOOP: [I; 7] = [
    I::new(O::Pull, 0),
    I::new(O::SetCounter(31), 0),
    I::new(O::OutputBit, 7),
    I::new(O::InputBit, 7),
    I::new(O::JumpDecrement(2), 0),
    I::new(O::Push, 0),
    I::new(O::Jump(0), 0),
];
const STALL: [I; 3] = [
    I::new(O::SetOutput(false), 0),
    I::new(O::WaitInput(true), 0),
    I::new(O::Jump(1), 0),
];

fn fail(code: u32) -> ! {
    PIO_RESULTS[0].store(0xbad1_0000 | code, Ordering::Release);
    loop {
        api::task::sleep_ms(100);
    }
}
fn require<T>(result: Result<T, Error>, code: u32) -> T {
    result.unwrap_or_else(|_| fail(code))
}
fn bump(index: usize) {
    PIO_RESULTS[index].fetch_add(1, Ordering::Relaxed);
}

pub fn run() {
    PIO_RESULTS[9].store(kernel::arch::Tick::cpu_hz(), Ordering::Relaxed);
    PIO_RESULTS[0].store(0x1750_0001, Ordering::Release);
    let start = api::timer::now_ms();
    while PIO_NONCE.load(Ordering::Acquire) == 0 {
        if api::timer::now_ms().wrapping_sub(start) > 60_000 {
            fail(1);
        }
        api::task::sleep_ms(1);
    }
    PIO_RESULTS[1].store(PIO_NONCE.load(Ordering::Acquire), Ordering::Release);
    for block in 0..2 {
        let mut pio = require(board::programmable_io(block), 2);
        if !matches!(board::programmable_io(block), Err(Error::Busy)) {
            fail(3);
        }
        bump(5);
        if !matches!(board::programmable_io(1 - block), Err(Error::Busy)) {
            fail(4);
        }
        bump(5);
        // Exercise all state machines and fragmented instruction allocation.
        let nop = [I::new(O::Nop, 0); 8];
        let no_pins = Config {
            input: false,
            output: false,
            ..CFG
        };
        for m in 0..4 {
            require(pio.configure(m, &nop, no_pins), 5);
        }
        if pio.configure(0, &LOOP, CFG) != Err(Error::Busy) {
            fail(6);
        }
        bump(5);
        pio.reset();
        require(pio.configure(0, &LOOP, CFG), 7);
        if pio.configure(1, &LOOP, CFG) != Err(Error::Busy) {
            fail(8);
        }
        bump(5);
        if pio.try_read(0) != Err(Error::WouldBlock) {
            fail(9);
        }
        bump(7);
        for w in 0..4 {
            require(pio.try_write(0, w), 10);
        }
        if pio.try_write(0, 5) != Err(Error::WouldBlock) {
            fail(11);
        }
        bump(6);
        require(pio.cancel(0), 12); // clear queued test words before real payloads
        require(pio.configure(0, &LOOP, CFG), 13);
        require(pio.start(0), 14);
        for n in 0u32..1000 {
            let word = n.wrapping_mul(0x9e37_79b9).rotate_left(n % 32) ^ 0xa55a_00ff;
            if require(pio.exchange(0, word, 20_000), 15) != word {
                fail(16);
            }
            bump(2);
        }
        require(pio.cancel(0), 17);
        require(pio.configure(0, &STALL, CFG), 18);
        require(pio.start(0), 19);
        if pio.exchange(0, 0, 2_000) != Err(Error::Timeout) {
            fail(20);
        }
        if pio.start(0) != Err(Error::NotConfigured) {
            fail(21);
        }
        // Timeout automatically returned both program RAM and machine ownership.
        require(pio.configure(0, &LOOP, CFG), 22);
        require(pio.start(0), 23);
        if require(pio.exchange(0, 0x1357_9bdf, 20_000), 24) != 0x1357_9bdf {
            fail(25);
        }
        bump(4);
        drop(pio);
        let mut again = require(board::programmable_io(block), 28);
        require(again.configure(3, &LOOP, CFG), 29);
        require(again.start(3), 30);
        if require(again.exchange(3, 0xc001_d00d, 20_000), 31) != 0xc001_d00d {
            fail(32);
        }
        drop(again);
        bump(8);
        bump(3);
    }
    api::log_info!("[FLINT] PIO words=2000 blocks=2 timeout=2 contention=8");
    api::log_info!("[FLINT] PIO fifo-full=2 fifo-empty=2 reopen=2");
    PIO_RESULTS[0].store(0x1750_600d, Ordering::Release);
    loop {
        api::task::sleep_ms(100);
    }
}
