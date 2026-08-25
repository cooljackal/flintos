// SPDX-License-Identifier: Apache-2.0

//! Raspberry Pi Pico board manifest.

use soc_rp2040 as soc;

pub use crate::wio_rp2040_mini::{
    ADC_EXTERNAL_HIGH_GPIO, CONSOLE_UART, DMA_POOL_BYTES, GPIO_LOOPBACK_IN, GPIO_LOOPBACK_OUT,
    HAS_BT, HAS_WIFI, LOOPBACK_AUX_GPIOS, LOOPBACK_SCRATCH_GPIO, PHY_MAX_TX_POWER_DBM,
    SELFTEST_UART, TARGET_BUSES, TARGET_DEVICES, TARGET_PERIPHERALS, TARGET_SERVICES,
    TICK_PERIOD_US,
};

pub const BOARD_NAME: &str = "Raspberry Pi Pico";
pub const USER_LED_GPIO: u8 = 25;
pub const USER_LED: soc::ctrl::GpioPort = soc::ctrl::GpioPort { pin: USER_LED_GPIO };

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
    fn onboard_led_and_uart_match_the_pico_pinout() {
        assert_eq!(USER_LED.pin, 25);
        assert_eq!(CONSOLE_UART.ctrl, soc::ctrl::UartCtrl::Uart0);
        assert_eq!(SELFTEST_UART.ctrl, soc::ctrl::UartCtrl::Uart1);
        assert_eq!((GPIO_LOOPBACK_OUT.pin, GPIO_LOOPBACK_IN.pin), (2, 3));
    }
}
