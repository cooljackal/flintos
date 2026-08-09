// SPDX-License-Identifier: Apache-2.0

//! Boot startup — board initialisation, driver setup.
//!
//! Called from FlintMain() before the scheduler starts.

use hal::bus::{BusKind, PhysicalBus};
use crate::board::active;

/// Global UART console driver (used by log/panic).
pub static mut CONSOLE_UART: Option<esp32_uart::Esp32Uart> = None;

/// Initialise board-level hardware.
/// Must be called before the scheduler starts.
pub fn init() {
    // Find the UART console from the board manifest.
    for bus in active::TARGET_BUSES {
        if bus.kind == BusKind::Uart {
            // SAFETY: `base_addr` comes from the board manifest, which is the
            // single source of truth for peripheral addresses, and this is the
            // only place a console UART is constructed.
            let mut uart = unsafe { esp32_uart::Esp32Uart::new(bus.base_addr) };
            let configured = uart.init(&bus.config);
            unsafe {
                CONSOLE_UART = Some(uart);
            }
            if configured.is_err() {
                // The port keeps whatever framing the ROM left, which is
                // usually 115200 8N1 and therefore still readable. Say so
                // rather than printing a banner that implies the requested
                // configuration was applied.
                crate::debug::fault::raw_print(
                    "[FLINT] WARNING: UART init rejected the board config; \
                     console is running at the bootloader's settings\r\n",
                );
            }
            // Print a boot banner.
            console_write(b"FlintOS booting...\r\n");
            break;
        }
    }
}

/// Write bytes to the console UART.
pub fn console_write(data: &[u8]) {
    unsafe {
        if let Some(ref uart) = CONSOLE_UART {
            uart.write_str(data);
        }
    }
}

/// Write a single byte to the console UART.
pub fn console_putc(c: u8) {
    unsafe {
        if let Some(ref uart) = CONSOLE_UART {
            uart.putc(c);
        }
    }
}
