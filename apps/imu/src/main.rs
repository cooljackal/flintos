// SPDX-License-Identifier: Apache-2.0

//! Reads the M5Stack Atom Matrix's onboard IMU over I²C.
//!
//! **This is the first thing in this tree ever assembled through all three
//! driver layers.** `bme280` and `ssd1306` have existed for months and are
//! constructed nowhere outside their own tests; `apps/blink` composes a Layer-1
//! driver with a Layer-3 one and skips Layer 2 entirely. So the layer diagram
//! described something that had never run.
//!
//! ```text
//!   Layer 3   bmi270      knows a part number, no chip
//!   Layer 2   i2c-bus     addressing and framing, no registers
//!   Layer 1   esp32-i2c   the controller's registers
//! ```
//!
//! What it proves, in order of how badly it was needed:
//!
//! - that the three layers compose at all
//! - that an I²C read returns data. `Esp32I2c::read` used to program the READ
//!   commands, wait, and leave the bytes in the RX FIFO — every read this
//!   driver ever did returned nothing
//! - that the address reaches the wire unshifted. The bus layer pre-shifted in
//!   one method and not another, and the physical driver shifted again
//!
//! Both of those were found by reading and fixed without hardware. This is
//! what says whether the fixes were right.
//!
//! # Which chip is actually there
//!
//! M5Stack shipped this board with an MPU6886 and later revisions with a
//! BMI270. **Both answer at 0x68**, so the address settles nothing. The app
//! reports what the ID registers say rather than assuming, and a scan runs
//! first so "nothing responded" is distinguishable from "something responded
//! and it is not what we expected".

#![no_std]
#![no_main]

use api::bus::{BusConfig, BusHandle, BusSpeed};
use api::task;
use hal::types::Priority;

use bmi270::{Bmi270, Identity};
use esp32_i2c::Esp32I2c;
use i2c_bus::I2cBus;

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

/// Layer 1 lives here so it can outlive the setup call: a `BusHandle` holds a
/// `&'static dyn Bus`, which is what lets a logical driver be handed a bus
/// without knowing its type.
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

    let Some(()) = (unsafe { bring_up_controller() }) else {
        api::log_error!("[imu] I2C controller init failed");
        park();
    };

    // Scan first. Without it, a silent device and a dead controller look
    // identical from the driver's point of view, and the last three hardware
    // bugs in this tree were all "it returns Ok and does nothing".
    scan();

    let Some(imu) = (unsafe { attach_driver() }) else {
        api::log_error!("[imu] could not attach the device driver");
        park();
    };

    match imu.probe() {
        Ok(Identity::Bmi270) => {
            api::log_info!("[imu] BMI270 present, chip id 0x24");
            match imu.internal_status() {
                // Zero is expected: nothing has uploaded the config blob, and
                // this driver does not. Say so rather than looking like a
                // failure.
                Ok(s) => api::log_info!(
                    "[imu] internal_status=0x{:02X} (0 = config blob not loaded, expected)",
                    s
                ),
                Err(e) => api::log_error!("[imu] internal_status read failed: {:?}", e),
            }
        }
        Ok(Identity::Mpu6886) => api::log_info!(
            "[imu] MPU6886 present (who_am_i 0x19) -- this board is an earlier revision"
        ),
        Ok(Identity::Unknown(id)) => api::log_error!(
            "[imu] something answered at 0x{:02X} but chip id was 0x{:02X}",
            board::IMU_I2C_ADDR,
            id
        ),
        Err(e) => api::log_error!("[imu] no answer from 0x{:02X}: {:?}", board::IMU_I2C_ADDR, e),
    }

    // Keep reading, so a one-off success is distinguishable from a stable bus.
    let mut n = 0u32;
    loop {
        task::sleep_ms(2000);
        n += 1;
        match imu.chip_id() {
            Ok(id) => api::log_info!("[imu] read {} chip_id=0x{:02X}", n, id),
            Err(e) => api::log_error!("[imu] read {} failed: {:?}", n, e),
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
        // 100 kHz. The IMU handles 400 kHz, but bring-up is not the time
        // to find out whether the bus is marginal.
        speed: BusSpeed::Standard100k,
    };
    // `init` gates the clock on, un-resets the peripheral and routes the pads
    // open-drain through the GPIO matrix -- in that order, because a running
    // controller connected to a still-push-pull pad can short against a device
    // holding the line low.
    hal::bus::PhysicalBus::init(&mut phys, &config).ok()?;
    PHYS = Some(phys);
    Some(())
}

/// Layers 2 and 3: address the device, then hand it to the driver.
///
/// # Safety
/// Borrows the statics for the rest of the program.
unsafe fn attach_driver() -> Option<Bmi270> {
    let phys: &'static dyn hal::bus::PhysicalBus = (*core::ptr::addr_of!(PHYS)).as_ref()?;
    BUS = Some(I2cBus::new(phys, board::IMU_I2C_ADDR));
    let bus: &'static dyn hal::bus::Bus = (*core::ptr::addr_of!(BUS)).as_ref()?;
    Some(Bmi270::new(BusHandle::new(bus)))
}

/// Walk the 7-bit address space and report who acknowledges.
///
/// A zero-length write is the conventional probe: it addresses the device and
/// stops, so a present device ACKs and an absent one NAKs without any register
/// being touched.
fn scan() {
    let Some(phys) = (unsafe { (*core::ptr::addr_of!(PHYS)).as_ref() }) else {
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
