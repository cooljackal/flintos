// SPDX-License-Identifier: Apache-2.0

//! Seeed Wio RP2040 Mini board manifest.
//!
//! GP13 is Seeed's documented user LED. UART0 GP0/GP1 follows the RP2040
//! function table and the board header pinout.
//!
//! The board's Wi-Fi is an ESP8285 module running its own firmware (Seeed
//! ships it with an AT-command image), attached to the RP2040 over a UART.
//! The RP2040 has no radio of its own, so `HAS_WIFI` is false: the kernel's
//! `radio-wifi` feature brings up a PHY the SoC drives directly, and nothing
//! of the sort exists here. When the module is used it will be a UART bus
//! entry plus a device on it, not a radio. Its wiring is deliberately absent
//! until the schematic connection is pinned and measured.
//!
//! Sources: Seeed Wio RP2040 Mini wiki and the Raspberry Pi RP2040 datasheet,
//! GPIO function table (build 3184e8e, 2025-02-20).

use hal::bus::*;
use soc_rp2040 as soc;

pub const BOARD_NAME: &str = "Seeed Wio RP2040 Mini";
pub const HAS_WIFI: bool = false; // ESP8285 over UART, not a PHY the RP2040 drives
pub const HAS_BT: bool = false;
pub const USER_LED_GPIO: u8 = 13;
pub const TICK_PERIOD_US: u32 = 1_000;
/// The DMA pool's size is fixed by the linker script (`arch/armv6m/rp2040.ld`
/// reserves 8 KiB), *not* by this constant; the runtime reads the real size
/// from the `_dma_pool_start`/`_dma_pool_end` symbols it emits. This value is a
/// manifest fact only, mirroring that reservation. Unlike the ESP32 boards the
/// RP2040 map is not radio-keyed — the chip has no radio of its own.
pub const DMA_POOL_BYTES: usize = 8_192;
pub const ADC_EXTERNAL_HIGH_GPIO: Option<u8> = None;
pub const LOOPBACK_SCRATCH_GPIO: Option<u8> = None;
pub const LOOPBACK_AUX_GPIOS: Option<(u8, u8)> = None;
pub const PHY_MAX_TX_POWER_DBM: i32 = 0;
pub const CONSOLE_UART: soc::ctrl::UartPort = soc::ctrl::UartPort {
    ctrl: soc::ctrl::UartCtrl::Uart0,
    cfg: UartConfig {
        tx: 0,
        rx: 1,
        baud: 115_200,
        data_bits: UartDataBits::Bits8,
        parity: UartParity::None,
        stop_bits: UartStopBits::Stop1,
    },
};
pub const SELFTEST_UART: soc::ctrl::UartPort = soc::ctrl::UartPort {
    ctrl: soc::ctrl::UartCtrl::Uart1,
    cfg: UartConfig {
        tx: 4,
        rx: 5,
        baud: 115_200,
        data_bits: UartDataBits::Bits8,
        parity: UartParity::None,
        stop_bits: UartStopBits::Stop1,
    },
};
pub const USER_LED: soc::ctrl::GpioPort = soc::ctrl::GpioPort { pin: USER_LED_GPIO };

pub const TARGET_BUSES: &[BusMapping] = &[BusMapping {
    name: "uart0",
    kind: BusKind::Uart,
    base_addr: soc::UART0_BASE,
    irq: soc::IRQ_UART0,
    dma_capable: true,
    dma_pool_bytes: 512,
    config: BusConfig::uart_8n1(0, 1, 115_200),
}];

pub const TARGET_DEVICES: &[BusDevice] = &[];

pub const TARGET_PERIPHERALS: &[PeripheralMapping] = &[PeripheralMapping {
    name: "gpio",
    base_addr: soc::IO_BANK0_BASE,
    irq: soc::IRQ_IO_BANK0,
    dma_capable: false,
    dma_pool_bytes: 0,
}];

pub const TARGET_SERVICES: &[ServiceMapping] = &[];

/// This board as one value; see [`crate::Board`]. The user LED is a plain GPIO,
/// not an addressable strip, so `rgb_led` is `None`; the `imu` field is absent
/// on this (non-ESP32) board. No loopback pad is confirmed on unflashed
/// hardware, so `selftest` is all `None`.
pub const BOARD: crate::Board = crate::Board {
    name: BOARD_NAME,
    rgb_led: None,
    grove: None,
    selftest: crate::SelftestPads {
        scratch: LOOPBACK_SCRATCH_GPIO,
        aux: LOOPBACK_AUX_GPIOS,
        spi_slave: None,
        pcnt: None,
        touch: None,
        mcpwm: None,
        adc_external_high: ADC_EXTERNAL_HIGH_GPIO,
    },
    console: crate::ConsolePins {
        tx: 0,
        rx: 1,
        baud: 115_200,
    },
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_led_and_uart_match_the_documented_pinout() {
        assert_eq!(USER_LED_GPIO, 13);
        assert!(matches!(
            TARGET_BUSES[0].config,
            BusConfig::Uart(UartConfig { tx: 0, rx: 1, .. })
        ));
    }

    #[test]
    fn uart_and_gpio_use_rp2040_register_facts() {
        assert_eq!(TARGET_BUSES[0].base_addr, soc::UART0_BASE);
        assert_eq!(TARGET_BUSES[0].irq, soc::IRQ_UART0);
        assert_eq!(TARGET_PERIPHERALS[0].base_addr, soc::IO_BANK0_BASE);
        assert_eq!(TARGET_PERIPHERALS[0].irq, soc::IRQ_IO_BANK0);
    }

    #[test]
    fn the_soc_drives_no_radio() {
        // The ESP8285 is a UART-attached module; `radio-wifi` must not be
        // able to select this board and try to bring up a PHY.
        assert!(!HAS_WIFI);
        assert!(!HAS_BT);
    }
}
