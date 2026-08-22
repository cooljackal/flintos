// SPDX-License-Identifier: Apache-2.0

//! Seeed Wio RP2040 Mini board manifest.
//!
//! GP13 is Seeed's documented user LED. UART0 GP0/GP1 follows the RP2040
//! function table and the board header pinout. ESP8285 wiring is deliberately
//! absent until its schematic connection is pinned and measured.
//!
//! Sources: Seeed Wio RP2040 Mini wiki and the Raspberry Pi RP2040 datasheet,
//! GPIO function table (build 3184e8e, 2025-02-20).

use hal::bus::*;
use soc_rp2040 as soc;

pub const BOARD_NAME: &str = "Seeed Wio RP2040 Mini";
pub const HAS_WIFI: bool = true;
pub const HAS_BT: bool = false;
pub const USER_LED_GPIO: u8 = 13;
pub const TICK_PERIOD_US: u32 = 1_000;
pub const DMA_POOL_BYTES: usize = 8_192;
pub const ADC_EXTERNAL_HIGH_GPIO: Option<u8> = None;
pub const LOOPBACK_SCRATCH_GPIO: Option<u8> = None;
pub const LOOPBACK_AUX_GPIOS: Option<(u8, u8)> = None;
pub const PHY_MAX_TX_POWER_DBM: i32 = 0;

pub const TARGET_BUSES: &[BusMapping] = &[BusMapping {
    name: "uart0",
    kind: BusKind::Uart,
    base_addr: soc::UART0_BASE,
    irq: soc::IRQ_UART0,
    dma_capable: true,
    dma_pool_bytes: 512,
    config: BusConfig::Uart {
        tx: 0,
        rx: 1,
        baud: 115_200,
        data_bits: UartDataBits::Bits8,
        parity: UartParity::None,
        stop_bits: UartStopBits::Stop1,
    },
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_led_and_uart_match_the_documented_pinout() {
        assert_eq!(USER_LED_GPIO, 13);
        assert!(matches!(
            TARGET_BUSES[0].config,
            BusConfig::Uart { tx: 0, rx: 1, .. }
        ));
    }

    #[test]
    fn uart_and_gpio_use_rp2040_register_facts() {
        assert_eq!(TARGET_BUSES[0].base_addr, soc::UART0_BASE);
        assert_eq!(TARGET_BUSES[0].irq, soc::IRQ_UART0);
        assert_eq!(TARGET_PERIPHERALS[0].base_addr, soc::IO_BANK0_BASE);
        assert_eq!(TARGET_PERIPHERALS[0].irq, soc::IRQ_IO_BANK0);
    }
}
