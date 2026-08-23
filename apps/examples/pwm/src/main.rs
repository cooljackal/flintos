// SPDX-License-Identifier: Apache-2.0

//! Drives LEDC and measures its own output.
//!
//! PWM has an awkward property for bring-up: the failure modes are invisible
//! without an oscilloscope. A duty stuck at zero, a frequency out by 16×, a
//! channel pointed at the wrong timer — all of them look like "the pin is
//! doing something" from software.
//!
//! So this measures. The LEDC output is routed to a pad, the pad's input path
//! is left enabled, and the app samples the pin thousands of times and counts
//! how often it reads high. At 25% duty the count should be near a quarter.
//! That catches every failure above without any instrument attached.
//!
//! # The pin
//!
//! The Grove port's SDA line. It is a connector with nothing on it unless you
//! plugged something in — **if you have, unplug it before running this**, since
//! the app drives the line. It is not the IMU's bus (GPIO 25/21) and not the
//! LED (GPIO 27).
//!
//! GPIO 12 would have been the obvious alternative, being the onboard IR LED,
//! but it is a strapping pin: held high at reset it selects a flash voltage
//! that can brick the module. Not a pin to drive during bring-up.
//!
//! Next: the porting templates — `imu` (I²C), `spitxrx` (SPI) or `uartecho`
//! (UART). They are peers; pick the bus your device speaks.

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;
use hal::{PinConfig, PinMux, Signal};

use esp32_gpio::{Esp32Gpio, PinLevel};
use esp32_ledc::{Channel, Timer};
use soc_esp32::dport::{self, ClockBit};
use soc_esp32::pinmux::Esp32PinMux;
use soc_esp32::{addr, io_mux};

#[cfg(not(any(feature = "board-m5-atom-lite", feature = "board-m5-atom-matrix")))]
compile_error!(
    "`pwm` drives the Grove port's SDA pin, which only the M5Stack Atom boards \
     declare.\n\n\tmake flash APP=pwm BOARD=board-m5-atom-matrix\n\n\
     To run it elsewhere, point PWM_GPIO at a free pin on your board."
);

kernel::flint_app!(main, abi = 2);

use kernel::board::active as board;

/// The pin LEDC drives and the app reads back.
const PWM_GPIO: u8 = board::GROVE_SDA_GPIO;

/// High-speed channel 0 on timer 0. Nothing else claims either.
const CHANNEL: u8 = 0;
const TIMER: u8 = 0;

/// 5 kHz at 13-bit — the combination every ESP32 PWM example uses, so it is
/// the one most easily compared against another implementation.
const FREQ_HZ: u32 = 5_000;
const RES_BITS: u8 = 13;

/// Samples per measurement. Deliberately not a multiple of the PWM period, so
/// sampling cannot lock to one phase and read the same point every time.
const SAMPLES: u32 = 20_001;

fn main() {
    task::spawn("pwm", pwm, Priority::Normal(1), 4096);
}

fn pwm() {
    let Some((ch, gpio)) = (unsafe { bring_up() }) else {
        api::log_error!("[pwm] could not configure LEDC");
        loop {
            task::sleep_ms(1000);
        }
    };

    let div = esp32_ledc::divider_for(FREQ_HZ, RES_BITS).unwrap_or(0);
    api::log_info!(
        "[pwm] LEDC ch{} timer{} on GPIO {}: asked {} Hz, get {} Hz at {}-bit",
        CHANNEL,
        TIMER,
        PWM_GPIO,
        FREQ_HZ,
        esp32_ledc::freq_for(div, RES_BITS),
        RES_BITS
    );

    // Sweep, and check each step against what was asked for. A monotonic
    // sequence catches a stuck duty that a single reading would not.
    loop {
        for pct in [0u8, 10, 25, 50, 75, 90, 100] {
            if unsafe { ch.set_percent(pct) }.is_none() {
                api::log_error!("[pwm] {}% was refused", pct);
                continue;
            }
            // Let the new duty take effect before sampling.
            task::sleep_ms(20);

            let measured = measure_duty_percent(&gpio);
            let readback = unsafe { ch.duty() };
            let expected = esp32_ledc::duty_for_percent(pct, RES_BITS);

            // The register must hold what was asked, undoing the <<4. If this
            // disagrees the fault is the encoding, not the wiring.
            if readback != expected {
                api::log_error!(
                    "[pwm] {}%: duty register holds {}, expected {}",
                    pct, readback, expected
                );
            }

            // Sampling is statistical, so allow a few points of slack. A wrong
            // shift would be out by 16x, and a wrong timer by far more.
            let ok = measured.abs_diff(pct as u32) <= 6;
            api::log_info!(
                "[pwm] asked {:>3}%  measured {:>3}%  duty reg {:>5}  {}",
                pct,
                measured,
                readback,
                if ok { "ok" } else { "OUT OF RANGE" }
            );
            task::sleep_ms(400);
        }
    }
}

/// Sample the pad and return the percentage of reads that were high.
fn measure_duty_percent(gpio: &Esp32Gpio) -> u32 {
    let mut high = 0u32;
    for _ in 0..SAMPLES {
        if matches!(gpio.read(PWM_GPIO), Ok(PinLevel::High)) {
            high += 1;
        }
    }
    (high * 100 + SAMPLES / 2) / SAMPLES
}

/// Clock LEDC, route it to the pad, and leave the pad readable.
///
/// # Safety
/// Claims LEDC channel 0, timer 0 and `PWM_GPIO` for the life of the program.
unsafe fn bring_up() -> Option<(Channel, Esp32Gpio)> {
    dport::enable(ClockBit::LEDC);

    let timer = Timer::new(TIMER, FREQ_HZ, RES_BITS)?;
    let ch = Channel::new(CHANNEL, &timer, RES_BITS, 0)?;

    Esp32PinMux::new()
        .route(Signal::LedcHs(CHANNEL), PWM_GPIO, PinConfig::PUSH_PULL)
        .ok()?;

    // `route` disables the input path for an output-only signal, which is
    // right in general and wrong here: this app reads back the pin it drives.
    // Re-enable it. The pad keeps its GPIO function and the matrix connection;
    // only FUN_IE changes.
    io_mux::configure(PWM_GPIO, io_mux::gpio_function(PWM_GPIO), true, hal::PinPull::None).ok()?;

    Some((ch, Esp32Gpio::new(addr::GPIO_BASE)))
}
