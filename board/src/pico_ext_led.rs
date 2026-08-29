// SPDX-License-Identifier: Apache-2.0

//! Raspberry Pi Pico (external LED on GP16).
//!
//! Scaffolded from `board-raspberry-pi-pico` by `make new-board`.
//!
//! Every fact is a clone of that board; edit only what differs here (a pin).

use soc_rp2040 as soc;

pub use crate::wio_rp2040_mini::{
    ADC_EXTERNAL_HIGH_GPIO, CONSOLE_UART, DMA_POOL_BYTES, EXPANSION_I2C, EXPANSION_SPI,
    FLASH_BYTES, NVS_PARTITION,
    GPIO_LOOPBACK_IN, GPIO_LOOPBACK_OUT,
    PIO_PORTS,
    HAS_BT, HAS_WIFI, LOOPBACK_AUX_GPIOS, LOOPBACK_SCRATCH_GPIO, PHY_MAX_TX_POWER_DBM,
    I2C_SELFTEST_MASTER, I2C_SELFTEST_SLAVE, PWM_LOOPBACK_OUT, SELFTEST_UART, SPI_SELFTEST,
    TARGET_BUSES, TARGET_DEVICES, TARGET_PERIPHERALS, TARGET_SERVICES, TICK_PERIOD_US,
};

pub const BOARD_NAME: &str = "Raspberry Pi Pico (external LED on GP16)";
// The one fact this board changes: the user LED is an external LED on GP16,
// not the Pico's onboard GP25.
pub const USER_LED_GPIO: u8 = 16;
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

