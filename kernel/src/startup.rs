//! Boot startup — board initialisation, driver setup.
//!
//! Called from FlintMain() before the scheduler starts.

use flint_hal::bus::{BusKind, PhysicalBus};
use crate::board::active;

/// Global UART console driver (used by log/panic).
pub static mut CONSOLE_UART: Option<esp32_uart::Esp32Uart> = None;

/// Initialise board-level hardware.
/// Must be called before the scheduler starts.
pub fn init() {
    // Find the UART console from the board manifest.
    for bus in active::TARGET_BUSES {
        if bus.kind == BusKind::Uart {
            let mut uart = esp32_uart::Esp32Uart::new(bus.base_addr);
            let _ = uart.init(&bus.config);
            unsafe {
                CONSOLE_UART = Some(uart);
            }
            // Print a boot banner.
            console_write(b"Flint RTOS booting...\r\n");
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
