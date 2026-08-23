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
//! # What the app still does, and what the driver now owns
//!
//! Since #112 the descriptor build, the interrupt enable, the id bookkeeping
//! and the engine start all live in [`esp32_spi::Esp32Spi::exchange_async`];
//! the app is left with claim, open, fill, exchange, await, compare. The one
//! piece that cannot move into the driver is the interrupt top-half: it names
//! the kernel's broker (`signal_complete`), which a physical driver may not.
//! It reads the same `&'static` driver the task built, through [`api::sync::Once`],
//! rather than fabricating a fresh one in the trap.
//!
//! # The loopback needs no wire
//!
//! The GPIO matrix can route a peripheral output to a pad *and* route that
//! same pad's input to a peripheral input. Point SPI2's MOSI at GPIO 22 and
//! SPI2's MISO at GPIO 22, and every byte clocked out arrives back on the same
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
//! are free on the Atom's headers. SCK goes to a pad because it must go
//! somewhere; nothing reads it.

#![no_std]
#![no_main]

use api::dma;
use api::sync::Once;
use api::task;
use hal::bus::{BusSpeed, SpiConfig, SpiMode};
use hal::pinmux::{PinConfig, PinMux, Signal};
use hal::types::Priority;
use soc_esp32::ctrl::{SpiCtrl, SpiPort};
use soc_esp32::{addr, Esp32PinMux};

use esp32_spi::Esp32Spi;

kernel::flint_app!(main, abi = 2);

/// The pad MOSI drives and MISO reads.
const LOOPBACK_GPIO: u8 = 22;
/// Somewhere for the clock to go.
const SCK_GPIO: u8 = 23;
/// A placeholder MISO pad, used only to get through `open`.
///
/// `open` insists on three distinct pins — two signals on one pad is a
/// mistake everywhere except in a loopback — so MISO starts here and is
/// folded onto [`LOOPBACK_GPIO`] afterwards. Nothing ever drives this pad:
/// MISO is an input, so GPIO 19 is only ever read.
const MISO_PLACEHOLDER_GPIO: u8 = 19;

/// SPI2's signal instance number.
const SPI2: u8 = 2;

/// Bytes per transfer. Deliberately past the 64-byte FIFO limit: at 64 or
/// under, a "DMA" path that quietly fell back to the data buffer would pass.
const LEN: usize = 512;

/// The driver, built once in the task and read from the interrupt top-half.
static SPI: Once<Esp32Spi> = Once::new();

/// End-of-frame flags as the top-half saw them.
///
/// Captured there because acknowledging clears them: a task reading
/// `dma_int_raw` afterwards sees zero and cannot tell a clean transfer from
/// one that never raised anything.
static ISR_FLAGS: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Top-half. Runs in trap context: read the flags for the judge, acknowledge
/// the peripheral, and hand the transfer id to the waiting task. Nothing else
/// belongs here.
fn spi_dma_isr() {
    use core::sync::atomic::Ordering;
    let Some(spi) = SPI.get() else { return };
    ISR_FLAGS.store(spi.dma_int_raw(), Ordering::SeqCst);
    // Level-triggered at the peripheral. Returning without clearing re-enters
    // this handler immediately and forever.
    spi.ack_interrupts();
    if let Some(id) = spi.take_pending_dma() {
        kernel::dma_broker::signal_complete(id);
    }
}

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
    // 1. Open SPI2: this claims the controller, gates its clock, routes the
    //    pads and configures it — everything the old `new` + `init` pair did,
    //    but proven single-owner. Stored in a `Once` so the interrupt top-half
    //    reads the very same driver.
    let port = SpiPort {
        ctrl: SpiCtrl::Spi2,
        cfg: SpiConfig {
            mosi: LOOPBACK_GPIO,
            miso: MISO_PLACEHOLDER_GPIO,
            sck: SCK_GPIO,
            max_speed: BusSpeed::MHz(4),
            mode: SpiMode::Mode0,
        },
    };
    let spi = SPI.init(Esp32Spi::open(&port).map_err(|_| "SPI open failed")?);

    // 2. Fold MISO onto the MOSI pad. `open` rejects two signals on one pad on
    //    purpose — it is a mistake everywhere except here — so the loopback is
    //    made by routing directly afterwards. MOSI (drive) first, then MISO
    //    (read), so the read side sets the pad's input enable last.
    let mux = Esp32PinMux::new();
    mux.route(Signal::SpiMosi(SPI2), LOOPBACK_GPIO, PinConfig::PUSH_PULL)
        .map_err(|_| "could not route MOSI")?;
    mux.route(Signal::SpiMiso(SPI2), LOOPBACK_GPIO, PinConfig::PUSH_PULL)
        .map_err(|_| "could not route MISO")?;

    // 3. Buffers from the DMA pool. A buffer on the stack would be outside
    //    internal DRAM as often as not, and the transfer would report success
    //    having moved nothing. The descriptor chains are the driver's problem
    //    now, not the app's.
    let tx_buf = dma::alloc(LEN as u32).map_err(|_| "tx buffer")?;
    let rx_buf = dma::alloc(LEN as u32).map_err(|_| "rx buffer")?;

    // A pattern that catches a stuck byte, a repeated byte and a shifted
    // buffer. A constant fill would pass all three.
    unsafe {
        let p = tx_buf.addr() as *mut u8;
        for i in 0..LEN {
            p.add(i).write_volatile((i as u8).wrapping_mul(31).wrapping_add(7));
        }
        core::ptr::write_bytes(rx_buf.addr() as *mut u8, 0, LEN);
    }

    // 4. Point SPI2's interrupt at a free CPU input and take the handler. The
    //    kernel picks the input — no magic number to collide with anything.
    //    Enabling the peripheral's interrupt is `exchange_async`'s job; routing
    //    it to a CPU is the kernel's.
    unsafe { api::interrupt::connect(addr::IRQ_SPI2, spi_dma_isr) }
        .map_err(|_| "cannot connect the SPI2 interrupt")?;

    // 5. Go, and block. Everything between here and completion — descriptor
    //    build, interrupt enable, id handoff, engine start — is inside the
    //    driver.
    let transfer = spi.exchange_async(&tx_buf, &rx_buf, LEN).map_err(|_| "could not start")?;
    transfer.await_done().map_err(|_| "transfer never completed")?;
    api::log_info!("[spidma] completed by interrupt");

    // 6. Judge it. Every check below has caught a different lie in something.
    use core::sync::atomic::Ordering;
    let raw = ISR_FLAGS.load(Ordering::SeqCst);
    api::log_info!("[spidma] int_raw {:#x}", raw);
    if raw & esp32_spi::SPI_OUT_EOF == 0 {
        return Err("the transmit chain never reached end-of-frame");
    }
    if raw & esp32_spi::SPI_IN_SUC_EOF == 0 {
        return Err("the receive chain never reached end-of-frame");
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

    Ok(())
}
