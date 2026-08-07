// SPDX-License-Identifier: Apache-2.0

//! Reads the M5Stack Atom Matrix's onboard IMU over I²C.
//!
//! **The first thing in this tree assembled through all three driver layers.**
//! `bme280` and `ssd1306` have existed for months and are constructed nowhere
//! outside their own tests; `apps/blink` composes a Layer-1 driver with a
//! Layer-3 one and skips Layer 2 entirely. The layer diagram described
//! something that had never run.
//!
//! ```text
//!   Layer 3   mpu6886     knows a part number, no chip
//!   Layer 2   i2c-bus     addressing and framing, no registers
//!   Layer 1   esp32-i2c   the controller's registers
//! ```
//!
//! # Which part is fitted
//!
//! M5Stack shipped this board with an MPU6886 and later revisions with a
//! BMI270. **Both answer at 0x68**, so the address settles nothing — only the
//! ID registers do, and the two parts keep them in different places. The app
//! probes for both and drives whichever answered, rather than assuming.
//!
//! A bus scan runs first, so "nothing is there" stays distinguishable from
//! "something is there and it is not what we expected".

#![no_std]
#![no_main]

use core::ptr::addr_of;

use api::bus::{BusConfig, BusHandle, BusSpeed};
use api::task;
use hal::types::Priority;

use bmi270::{Bmi270, Identity};
use esp32_i2c::Esp32I2c;
use i2c_bus::I2cBus;
use mpu6886::Mpu6886;

#[cfg(not(feature = "board-m5-atom-matrix"))]
compile_error!(
    "`imu` reads an onboard IMU, and only the M5Stack Atom Matrix declares one.\n\n\
     \tmake flash APP=imu BOARD=board-m5-atom-matrix\n\n\
     The Atom Lite has no IMU. To run this elsewhere, give that board's manifest \
     an `IMU_SDA_GPIO`, `IMU_SCL_GPIO` and `IMU_I2C_ADDR`."
);

kernel::flint_app!(main, abi = 1);

use kernel::board::active as board;

/// The controller. I2C0 is otherwise unused in this build.
const I2C_BASE: u32 = soc_esp32::addr::I2C0_BASE;

/// Layers 1 and 2 live here so they outlive the setup calls: a `BusHandle`
/// holds a `&'static dyn Bus`, which is what lets a logical driver be given a
/// bus without knowing its type.
static mut PHYS: Option<Esp32I2c> = None;
static mut BUS: Option<I2cBus> = None;

fn main() {
    task::spawn("imu", imu, Priority::Normal(1), 4096);
}

fn imu() {
    api::log_info!(
        "[imu] I2C0 on SDA={} SCL={}, IMU at 0x{:02X}",
        board::IMU_SDA_GPIO,
        board::IMU_SCL_GPIO,
        board::IMU_I2C_ADDR
    );

    if unsafe { bring_up_controller() }.is_none() {
        api::log_error!("[imu] I2C controller init failed");
        park();
    }

    // Scan first. Without it a silent device and a dead controller look
    // identical from the driver's side, and every I2C bug found on this board
    // so far presented as "returns Ok and does nothing".
    scan();

    let Some(bus) = (unsafe { attach_bus() }) else {
        api::log_error!("[imu] could not attach the bus");
        park();
    };

    match Bmi270::new(BusHandle::new(bus)).probe() {
        Ok(Identity::Mpu6886) => {
            api::log_info!("[imu] MPU6886 (who_am_i 0x19)");
            read_mpu6886(bus)
        }
        Ok(Identity::Bmi270) => {
            api::log_info!("[imu] BMI270 (chip id 0x24) -- no motion driver for it yet");
            park()
        }
        Ok(Identity::Unknown(id)) => {
            api::log_error!(
                "[imu] something answered at 0x{:02X}, but its id register said 0x{:02X}",
                board::IMU_I2C_ADDR,
                id
            );
            park()
        }
        Err(e) => {
            api::log_error!("[imu] no answer from 0x{:02X}: {:?}", board::IMU_I2C_ADDR, e);
            park()
        }
    }
}

/// Configure the MPU6886 and stream readings.
fn read_mpu6886(bus: &'static dyn hal::bus::Bus) -> ! {
    let dev = Mpu6886::new(BusHandle::new(bus));

    // The driver does not wait -- how long to pause after a reset depends on
    // this board, not on the part, so the sequencing is ours. 10 ms is the
    // datasheet minimum and the Atom needs no more.
    let brought_up = dev.reset().and_then(|()| {
        task::sleep_ms(10);
        dev.wake()
    }).and_then(|()| {
        task::sleep_ms(10);
        dev.configure()
    });
    if let Err(e) = brought_up {
        api::log_error!("[imu] bring-up failed: {:?}", e);
        park();
    }
    api::log_info!("[imu] configured: +/-8 g, +/-2000 dps");

    let mut n = 0u32;
    loop {
        task::sleep_ms(500);
        n += 1;
        match (dev.accel(), dev.gyro(), dev.temperature_milli_c()) {
            (Ok(a), Ok(g), Ok(t)) => {
                let a = a.to_milli_g();
                let g = g.to_milli_dps();
                api::log_info!(
                    "[imu] {} accel {} {} {} mg | gyro {} {} {} mdps | {} mC",
                    n, a.x, a.y, a.z, g.x, g.y, g.z, t
                );
            }
            _ => api::log_error!("[imu] {} read failed", n),
        }
    }
}

/// Layer 1: clock the controller, route the pads, set the bit rate.
///
/// # Safety
/// Claims I2C0 and the IMU's two pads for the life of the program.
unsafe fn bring_up_controller() -> Option<()> {
    let mut phys = Esp32I2c::new(I2C_BASE);
    let config = BusConfig::I2c {
        sda: board::IMU_SDA_GPIO,
        scl: board::IMU_SCL_GPIO,
        // 100 kHz. The part handles 400 kHz, but bring-up is not the time to
        // find out whether the bus is marginal.
        speed: BusSpeed::Standard100k,
    };
    // `init` gates the clock on, un-resets the peripheral and routes the pads
    // open-drain -- in that order, because a running controller connected to a
    // still-push-pull pad can short against a device holding the line low.
    hal::bus::PhysicalBus::init(&mut phys, &config).ok()?;
    PHYS = Some(phys);
    Some(())
}

/// Layer 2: address the device.
///
/// # Safety
/// Borrows the statics for the rest of the program.
unsafe fn attach_bus() -> Option<&'static dyn hal::bus::Bus> {
    let phys: &'static dyn hal::bus::PhysicalBus = (*addr_of!(PHYS)).as_ref()?;
    BUS = Some(I2cBus::new(phys, board::IMU_I2C_ADDR));
    let bus: &'static I2cBus = (*addr_of!(BUS)).as_ref()?;
    Some(bus)
}

/// Walk the 7-bit address space and report who acknowledges.
///
/// A zero-length write is the conventional probe: it addresses the device and
/// stops, so a present device ACKs and an absent one NAKs without any register
/// being touched.
fn scan() {
    let Some(phys) = (unsafe { (*addr_of!(PHYS)).as_ref() }) else {
        return;
    };
    api::log_info!("[imu] scanning 0x08..0x77");
    let mut found = 0;
    for addr in 0x08..=0x77u8 {
        // `tx[0]` is the 7-bit address, unshifted -- the physical driver adds
        // the R/W bit. See `hal::PhysicalBus::raw_transfer`.
        if hal::bus::PhysicalBus::raw_transfer(phys, &[addr], &mut []).is_ok() {
            api::log_info!("[imu]   0x{:02X} responded", addr);
            found += 1;
        }
    }
    if found == 0 {
        api::log_error!(
            "[imu] nothing responded. Pins, pull-ups or clock gating -- not the device driver."
        );
    } else {
        api::log_info!("[imu] {} device(s) on the bus", found);
    }
}

fn park() -> ! {
    loop {
        task::sleep_ms(1000);
    }
}
