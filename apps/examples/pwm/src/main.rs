// SPDX-License-Identifier: Apache-2.0

//! Drives LEDC and measures its own output.
//!
//! PWM has an awkward property for bring-up: the failure modes are invisible
//! without an oscilloscope. A duty stuck at zero, a frequency out by 16×, a
//! channel pointed at the wrong timer — all of them look like "the pin is
//! doing something" from software.
//!
//! So this measures. The board opens an LEDC channel on a pad and leaves the
//! pad's input path enabled; the app samples the pin thousands of times and
//! counts how often it reads high. At 25% duty the count should be near a
//! quarter. That catches every failure above without any instrument attached.
//!
//! The whole Layer-1 bring-up — claim the timer and channel, gate the clock,
//! route the signal, leave the pad readable — lives behind `board::pwm`, so
//! this app names no SoC crate and no physical driver, and reaches register
//! level through none of the escapes the layer guard bans. Dropping to that
//! level would cost one driver dependency, not a different program.
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

use api::task::{self, Task};

// The board owns the LEDC bring-up (`board::pwm`) and the pin read-back
// (`board::read_pwm_pin`). Manifest facts an app reads directly still come from
// `kernel::board::active`.
use kernel::board::active as manifest;

// The board is selected with `--features kernel/board-...` now, not an
// application feature, so this guard keys on a board fact -- the board's own
// name -- rather than a feature this crate no longer declares. Only the Atom
// boards route the Grove port's SDA pin this app drives; both report a name
// with "ATOM" in it, and no other board does.
const _: () = assert!(
    board_name_contains("ATOM"),
    "`pwm` drives the Grove port's SDA pin, which only the M5Stack Atom boards \
     declare.\n\n\tmake flash APP=pwm BOARD=board-m5-atom-matrix\n\n\
     To run it elsewhere, point PWM_GPIO at a free pin on your board."
);

kernel::flint_app!(main, abi = 2);

/// Substring test over `manifest::BOARD_NAME`, usable in a `const` context.
const fn board_name_contains(needle: &str) -> bool {
    let hay = manifest::BOARD_NAME.as_bytes();
    let ndl = needle.as_bytes();
    if ndl.is_empty() {
        return true;
    }
    if ndl.len() > hay.len() {
        return false;
    }
    let mut i = 0;
    while i + ndl.len() <= hay.len() {
        let mut j = 0;
        while j < ndl.len() && hay[i + j] == ndl[j] {
            j += 1;
        }
        if j == ndl.len() {
            return true;
        }
        i += 1;
    }
    false
}

/// The pin LEDC drives and the app reads back.
const PWM_GPIO: u8 = manifest::GROVE_SDA_GPIO;

/// 5 kHz at 13-bit — the combination every ESP32 PWM example uses, so it is
/// the one most easily compared against another implementation.
const FREQ_HZ: u32 = 5_000;
const RES_BITS: u8 = 13;

/// Samples per measurement. Deliberately not a multiple of the PWM period, so
/// sampling cannot lock to one phase and read the same point every time.
const SAMPLES: u32 = 20_001;

fn main() {
    if Task::new("pwm", pwm).spawn().is_none() {
        api::log_error!("could not start the pwm task");
    }
}

fn pwm() {
    // The board claims LEDC channel 0 on timer 0, gates the clock, routes the
    // signal onto the pad, and leaves the pad readable -- everything the app
    // used to open-code at register level.
    let ch = match board::pwm(PWM_GPIO, FREQ_HZ, RES_BITS) {
        Ok(ch) => ch,
        Err(e) => {
            api::log_error!("could not configure LEDC: {:?}", e);
            task::exit();
        }
    };

    let div = board::pwm_divider_for(FREQ_HZ, RES_BITS).unwrap_or(0);
    api::log_info!(
        "LEDC ch0 timer0 on GPIO {}: asked {} Hz, get {} Hz at {}-bit",
        PWM_GPIO,
        FREQ_HZ,
        board::pwm_freq_for(div, RES_BITS),
        RES_BITS
    );

    // Sweep, and check each step against what was asked for. A monotonic
    // sequence catches a stuck duty that a single reading would not.
    loop {
        for pct in [0u8, 10, 25, 50, 75, 90, 100] {
            if ch.set_percent(pct).is_none() {
                api::log_error!("{}% was refused", pct);
                continue;
            }
            // Let the new duty take effect before sampling.
            task::sleep_ms(20);

            let measured = measure_duty_percent();
            let readback = ch.duty();
            let expected = board::pwm_duty_for_percent(pct, RES_BITS);

            // The register must hold what was asked, undoing the <<4. If this
            // disagrees the fault is the encoding, not the wiring.
            if readback != expected {
                api::log_error!(
                    "{}%: duty register holds {}, expected {}",
                    pct, readback, expected
                );
            }

            // Sampling is statistical, so allow a few points of slack. A wrong
            // shift would be out by 16x, and a wrong timer by far more.
            let ok = measured.abs_diff(pct as u32) <= 6;
            api::log_info!(
                "asked {:>3}%  measured {:>3}%  duty reg {:>5}  {}",
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
fn measure_duty_percent() -> u32 {
    let mut high = 0u32;
    for _ in 0..SAMPLES {
        if board::read_pwm_pin(PWM_GPIO) {
            high += 1;
        }
    }
    (high * 100 + SAMPLES / 2) / SAMPLES
}
