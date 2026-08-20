// SPDX-License-Identifier: Apache-2.0

//! UART through the Layer-2 `Bus`, looped TX→RX on-chip.
//!
//! **The UART porting template.** `apps/imu` shows the three-layer stack for
//! I²C; this is its UART counterpart — Layer 1 (`esp32-uart`) under Layer 2
//! (`uart-bus`), driven through the transfer-list [`Bus`] API. There is no
//! Layer-3 device here: the point is the bus, so it echoes to itself.
//!
//! ```text
//!   Layer 2   uart-bus     the transfer-list Bus (Op lists)
//!   Layer 1   esp32-uart   the controller's registers
//! ```
//!
//! # No wire needed
//!
//! UART2 (never UART0 — that is the console) is put in its internal loopback
//! mode (CONF0 bit 14), which routes TX→RX on-chip. The pins are still routed
//! for real, so bring-up is exercised, but the data does not depend on an
//! analog pad edge — which for an async UART matters, since the receiver frames
//! on start-bit edges rather than a shared clock.
//!
//! Enabling the receiver latches one spurious byte into the RX FIFO; the bring-
//! up drains it, and each round drains any residue before the exchange, so the
//! echo stays byte-aligned. See `kernel::selftest_uart` for the full story.
//!
//! # Porting to a real link
//!
//! Drop `set_loopback`, route TX/RX to the real pins, and either drive the
//! `Bus` directly as below or hand a Layer-3 driver a `BusHandle::new(&uart_bus)`
//! exactly as `apps/imu` does. The Layer-1/Layer-2 bring-up is unchanged.

#![no_std]
#![no_main]

use core::ptr::addr_of;

use api::bus::{Bus, BusConfig, Op, PhysicalBus, UartDataBits, UartParity, UartStopBits};
use api::task;
use hal::types::Priority;
use soc_esp32::addr;
use uart_bus::UartBus;

kernel::flint_app!(main, abi = 1);

use kernel::board::active as board;

/// UART2. UART0 is the console the harness reads; UART1's pads clash with the
/// SPI flash on many modules, so UART2 is the safe spare.
const UART_BASE: u32 = addr::UART2_BASE;

/// Layers 1 and 2 outlive the setup calls. `PHYS` is kept typed (not just as a
/// `&dyn`) so the loop can drain the RX FIFO between rounds.
static mut PHYS: Option<esp32_uart::Esp32Uart> = None;
static mut BUS: Option<UartBus> = None;

fn main() {
    task::spawn("uartecho", run, Priority::Normal(1), 4096);
}

fn run() {
    // The pins are only routed, not wired; still, a board that declares no free
    // pads has nothing safe to route onto, so it cannot run this.
    let (Some(tx_pin), Some((rx_pin, _))) =
        (board::LOOPBACK_SCRATCH_GPIO, board::LOOPBACK_AUX_GPIOS)
    else {
        api::log_error!(
            "[uartecho] this board declares no free loopback GPIOs; \
             build for board-esp32-devkitc"
        );
        park();
    };

    api::log_info!("[uartecho] UART2 internal loopback, TX pad GPIO{}", tx_pin);

    if unsafe { bring_up(tx_pin, rx_pin) }.is_none() {
        api::log_error!("[uartecho] UART bring-up failed");
        park();
    }

    let (Some(bus), Some(phys)) =
        (unsafe { (*addr_of!(BUS)).as_ref() }, unsafe { (*addr_of!(PHYS)).as_ref() })
    else {
        park();
    };

    let mut round = 0u8;
    loop {
        task::sleep_ms(1000);

        // Discard any byte the receiver latched while the line was idle, so the
        // echo this round reads back is the byte it sent, not a leftover.
        while phys.getc().is_some() {}

        let mut tx = [0u8; 8];
        for (i, b) in tx.iter_mut().enumerate() {
            *b = round.wrapping_add(i as u8).wrapping_mul(37).wrapping_add(11);
        }
        let mut rx = [0x3Cu8; 8];

        match bus.transfer(&mut [Op::exchange(&tx, &mut rx)]) {
            Ok(()) if rx == tx => api::log_info!("[uartecho] round {}: {:?} echoed OK", round, tx),
            Ok(()) => api::log_error!("[uartecho] round {}: sent {:?}, got {:?}", round, tx, rx),
            Err(e) => api::log_error!("[uartecho] round {}: transfer failed: {:?}", round, e),
        }
        round = round.wrapping_add(1);
    }
}

/// Layer 1 + Layer 2 bring-up: init the port, loop it internally, drain the
/// enable-time spurious byte, and wrap it in a `UartBus`.
///
/// # Safety
/// Claims UART2 and the two pads for the life of the program, and stores the
/// driver and bus in statics the loop borrows.
unsafe fn bring_up(tx_pin: u8, rx_pin: u8) -> Option<()> {
    let mut phys = esp32_uart::Esp32Uart::new(UART_BASE);
    PhysicalBus::init(
        &mut phys,
        &BusConfig::Uart {
            tx: tx_pin,
            rx: rx_pin,
            baud: 115_200,
            data_bits: UartDataBits::Bits8,
            parity: UartParity::None,
            stop_bits: UartStopBits::Stop1,
        },
    )
    .ok()?;

    // Route TX→RX internally: a clean digital path, no pad edge to mis-frame.
    phys.set_loopback(true);
    PHYS = Some(phys);

    // Absorb the spurious byte the receiver latches when it comes up.
    let phys_ref: &'static esp32_uart::Esp32Uart = (*addr_of!(PHYS)).as_ref()?;
    while phys_ref.getc().is_some() {}

    BUS = Some(UartBus::new(phys_ref));
    Some(())
}

fn park() -> ! {
    loop {
        task::sleep_ms(1000);
    }
}
