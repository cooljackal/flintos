// SPDX-License-Identifier: Apache-2.0

//! Proves the DMA engine actually moves bytes.
//!
//! A DMA transfer has the same unpleasant property as PWM did: the failure
//! modes are quiet. A descriptor chain with the two 12-bit fields swapped, a
//! link register programmed with the full address instead of its low 20 bits,
//! a host with no channel selected — all of them return success and move
//! nothing. "It didn't crash" is not evidence.
//!
//! So this sends a known pattern and checks it comes back byte for byte.
//!
//! # The loopback needs no wire
//!
//! The GPIO matrix can route a peripheral output to a pad *and* route that
//! same pad's input to a peripheral input. Point SPI2's MOSI at GPIO 16 and
//! SPI2's MISO at GPIO 16, and every byte clocked out arrives back on the same
//! transaction. No jumper, nothing to forget to connect, and the data really
//! does leave the chip's logic and come back through the pad.
//!
//! Order matters: MOSI first, then MISO. Routing a signal sets the pad's input
//! enable according to whether that signal reads or drives, so the read side
//! has to be the one that lands last.
//!
//! # The pins
//!
//! GPIO 22 (loopback), 23 (SCK), 19 (a placeholder MISO, see below). All three
//! are free on the Atom's headers.
//!
//! **Not GPIO 16 or 17.** `board` used to advertise those as free on the
//! grounds that the PICO-D4 has no external PSRAM. It has no *external* flash
//! either — it is a SiP with the flash in the package, and 16/17 are part of
//! how the die reaches it. Routing SPI onto GPIO 16 killed the running image
//! mid-instruction: the console garbled halfway through a line and the chip
//! went silent, with no fault and no reset. That is what `RESERVED_GPIOS`
//! now says.
//!
//! SCK goes to a pad because it must go somewhere; nothing reads it.

#![no_std]
#![no_main]

use core::sync::atomic::{AtomicU32, Ordering};

use api::task;
use hal::bus::{BusConfig, BusSpeed, PhysicalBus, SpiMode};
use hal::pinmux::{PinConfig, PinMux, Signal};
use hal::types::Priority;
use soc_esp32::{addr, dma, intr_map, Esp32PinMux};

kernel::flint_app!(main, abi = 1);

/// The pad MOSI drives and MISO reads.
const LOOPBACK_GPIO: u8 = 22;
/// Somewhere for the clock to go.
const SCK_GPIO: u8 = 23;
/// A placeholder MISO pad, used only to get through `init`.
///
/// `init` insists on three distinct pins — two signals on one pad is a
/// mistake everywhere except in a loopback — so MISO starts here and is
/// folded onto [`LOOPBACK_GPIO`] afterwards. Nothing ever drives this pad:
/// MISO is an input, so GPIO 19 is only ever read.
const MISO_PLACEHOLDER_GPIO: u8 = 19;

/// SPI2's signal instance number.
const SPI2: u8 = 2;

/// Bytes per transfer. Deliberately past the 64-byte FIFO limit: at 64 or
/// under, a "DMA" path that quietly fell back to the data buffer would pass.
const LEN: usize = 512;

/// CPU interrupt the SPI2 source is routed onto.
///
/// 13 is external, level 1, and not claimed by the kernel's own timer or
/// software interrupts. `intr_map::route` refuses anything it could not
/// service, so a wrong choice here is an error rather than a silence.
const SPI_CPU_INT: u8 = 13;

/// The transfer in flight, for the top-half to complete.
static PENDING: AtomicU32 = AtomicU32::new(0);

/// End-of-frame flags as the top-half saw them.
///
/// Captured there because acknowledging clears them: a task reading
/// `dma_int_raw` afterwards sees zero and cannot tell a clean transfer from
/// one that never raised anything.
static ISR_FLAGS: AtomicU32 = AtomicU32::new(0);

/// Top-half. Runs in trap context: acknowledge the peripheral and hand the id
/// to the waiting task. Nothing else belongs here.
fn spi_dma_isr() {
    // Level-triggered at the peripheral. Returning without clearing re-enters
    // this handler immediately and forever.
    let spi = unsafe { esp32_spi::Esp32Spi::new(addr::SPI2_BASE) };
    ISR_FLAGS.store(unsafe { spi.dma_int_raw() }, Ordering::SeqCst);
    unsafe { spi.ack_interrupts() };

    let id = PENDING.swap(0, Ordering::SeqCst);
    if id != 0 {
        kernel::dma_broker::signal_complete(kernel::dma_broker::DmaTransferId::from_raw(id));
    }
}

/// Run the 8-byte FIFO loopback before the DMA one.
///
/// Off, and not because it fails. It passes — and then costs the DMA transfer
/// that follows exactly 8 bytes, which arrive at 504 of 512. The SPI's own
/// data buffer holds what the FIFO transfer left, and the receive DMA takes
/// that before it takes the wire.
///
/// Worth keeping as a switch: turned on, it proves the loopback is connected
/// before the descriptor chain is implicated at all, which is how the
/// full-duplex bug below was found. Just do not read the byte count while it
/// is on.
const FIFO_PRECHECK: bool = false;

fn main() {
    task::spawn("spidma", run, Priority::Normal(2), 8192);
}

fn run() {
    task::sleep_ms(200);
    api::log_info!("[spidma] {} bytes, SPI2 looped back on GPIO{}", LEN, LOOPBACK_GPIO);

    match attempt() {
        Ok(()) => api::log_info!("[spidma] PASS"),
        Err(e) => api::log_error!("[spidma] FAIL: {}", e),
    }
    loop {
        task::sleep_ms(1000);
    }
}

fn attempt() -> Result<(), &'static str> {
    // 1. A channel. Without one the crossbar points the host at nothing and
    //    every transfer succeeds while moving zero bytes.
    let channel = unsafe { dma::claim(dma::Host::Spi2) }.map_err(|_| "no DMA channel")?;
    api::log_info!("[spidma] channel {} claimed", channel.number());

    // 2. The host itself, through the ordinary init path.
    let mut spi = unsafe { esp32_spi::Esp32Spi::new(addr::SPI2_BASE) };
    spi.init(&BusConfig::Spi {
        mosi: LOOPBACK_GPIO,
        miso: MISO_PLACEHOLDER_GPIO,
        sck: SCK_GPIO,
        max_speed: BusSpeed::MHz(4),
        mode: SpiMode::Mode0,
    })
    .map_err(|_| "SPI init failed")?;

    // 3. Now fold MISO onto the MOSI pad. `init` rejects that on purpose —
    //    two signals on one pad is a mistake everywhere except here — so the
    //    loopback is made by routing directly, after init has done the rest.
    let mux = Esp32PinMux::new();
    mux.route(Signal::SpiMosi(SPI2), LOOPBACK_GPIO, PinConfig::PUSH_PULL)
        .map_err(|_| "could not route MOSI")?;
    mux.route(Signal::SpiMiso(SPI2), LOOPBACK_GPIO, PinConfig::PUSH_PULL)
        .map_err(|_| "could not route MISO")?;

    // 3b. Prove the loopback itself with the FIFO path before blaming DMA.
    //     If these 8 bytes do not come back, the problem is SPI or the pin
    //     routing and the descriptor chain is not implicated at all.
    if FIFO_PRECHECK {
        let tx = [0xA5u8, 0x00, 0xFF, 0x5A, 0x01, 0x02, 0x04, 0x08];
        let mut rx = [0u8; 8];
        spi.transfer(&tx, &mut rx).map_err(|_| "FIFO loopback transfer failed")?;
        api::log_info!("[spidma] fifo sent {:?}", tx);
        api::log_info!("[spidma] fifo got  {:?}", rx);
        if rx != tx {
            return Err("FIFO loopback did not return what it sent");
        }
        api::log_info!("[spidma] FIFO loopback verified");
    }

    // 4. Buffers and descriptor chains, all from the DMA pool. A buffer on the
    //    stack would be outside SRAM2 as often as not, and the transfer would
    //    report success having moved nothing.
    let tx_buf = kernel::dma_broker::alloc(LEN as u32).map_err(|_| "tx buffer")?;
    let rx_buf = kernel::dma_broker::alloc(LEN as u32).map_err(|_| "rx buffer")?;
    let descs = dma::descriptors_needed(LEN as u32) as usize;
    let desc_bytes = (descs * core::mem::size_of::<dma::Descriptor>()) as u32;
    let tx_desc = kernel::dma_broker::alloc(desc_bytes).map_err(|_| "tx descriptors")?;
    let rx_desc = kernel::dma_broker::alloc(desc_bytes).map_err(|_| "rx descriptors")?;

    // A pattern that catches a stuck byte, a repeated byte and a shifted
    // buffer. A constant fill would pass all three.
    unsafe {
        let p = tx_buf.addr() as *mut u8;
        for i in 0..LEN {
            p.add(i).write_volatile((i as u8).wrapping_mul(31).wrapping_add(7));
        }
        core::ptr::write_bytes(rx_buf.addr() as *mut u8, 0, LEN);
    }

    let (tx_head, rx_head) = unsafe {
        let tx = core::slice::from_raw_parts_mut(tx_desc.addr() as *mut dma::Descriptor, descs);
        let rx = core::slice::from_raw_parts_mut(rx_desc.addr() as *mut dma::Descriptor, descs);
        let t = dma::build_chain(tx, tx_buf.addr(), LEN as u32, dma::Direction::Transmit)
            .map_err(|_| "tx chain")?;
        let r = dma::build_chain(rx, rx_buf.addr(), LEN as u32, dma::Direction::Receive)
            .map_err(|_| "rx chain")?;
        (t, r)
    };

    // 5. Point SPI2's interrupt at a CPU input and take the handler. Enabling
    //    the peripheral's interrupt without routing it is a transfer whose
    //    completion never arrives, which is indistinguishable from one that
    //    never finished.
    unsafe { intr_map::route(addr::IRQ_SPI2, SPI_CPU_INT) }
        .map_err(|_| "cannot route SPI2 interrupt")?;
    if !kernel::interrupt::register(SPI_CPU_INT, spi_dma_isr) {
        return Err("CPU interrupt already has a handler");
    }
    unsafe { kernel::arch::registers::enable_interrupt(SPI_CPU_INT as u32) };
    unsafe { spi.dma_int_enable(esp32_spi::SPI_IN_SUC_EOF) };

    // 6. Go, and block. The id is published before the engine starts: the
    //    transfer can complete before the next instruction retires, and a
    //    top-half that found no id would drop the completion on the floor.
    let id = kernel::dma_broker::begin(&rx_buf).map_err(|_| "could not begin")?;
    PENDING.store(id.raw(), Ordering::SeqCst);
    unsafe { spi.start_dma(tx_head, rx_head, LEN) }.map_err(|_| "could not start")?;

    kernel::dma_broker::await_transfer(id, 100).map_err(|_| "transfer never completed")?;
    api::log_info!("[spidma] completed by interrupt");

    let raw = ISR_FLAGS.load(Ordering::SeqCst);
    let received = unsafe {
        let rx = core::slice::from_raw_parts(rx_desc.addr() as *const dma::Descriptor, descs);
        dma::received_len(rx)
    };
    api::log_info!("[spidma] int_raw {:#x}, {} bytes reported", raw, received);

    // 6. Judge it. Every check below has caught a different lie in something.
    if raw & esp32_spi::SPI_OUT_EOF == 0 {
        return Err("the transmit chain never reached end-of-frame");
    }
    if raw & esp32_spi::SPI_IN_SUC_EOF == 0 {
        return Err("the receive chain never reached end-of-frame");
    }
    if received != LEN as u32 {
        api::log_error!("[spidma] expected {} bytes, engine reported {}", LEN, received);
        return Err("short transfer");
    }

    let (mut mismatches, mut first_bad) = (0u32, usize::MAX);
    let mut all_zero = true;
    for i in 0..LEN {
        let (sent, got) = unsafe {
            (
                (tx_buf.addr() as *const u8).add(i).read_volatile(),
                (rx_buf.addr() as *const u8).add(i).read_volatile(),
            )
        };
        if got != 0 {
            all_zero = false;
        }
        if sent != got {
            mismatches += 1;
            if first_bad == usize::MAX {
                first_bad = i;
                api::log_error!("[spidma] byte {}: sent {:#04x}, got {:#04x}", i, sent, got);
            }
        }
    }

    // An all-zero buffer is the signature of a transfer that never happened,
    // and it is worth naming separately: "512 mismatches" reads like a data
    // corruption problem when it is really nothing arriving at all.
    if all_zero {
        return Err("nothing arrived — the receive buffer is still zero");
    }
    if mismatches != 0 {
        api::log_error!("[spidma] {} of {} bytes differ", mismatches, LEN);
        return Err("data came back wrong");
    }

    unsafe { dma::release(channel) };
    Ok(())
}
