// SPDX-License-Identifier: Apache-2.0

//! Reads the M5Stack Atom Matrix's onboard IMU over I²C.
//!
//! **The first thing in this tree assembled through all three driver layers.**
//! `bme280` and `ssd1306` have existed for months and are constructed nowhere
//! outside their own tests; `apps/examples/blink` composes a Layer-1 driver
//! with a Layer-3 one and skips Layer 2 entirely. The layer diagram described
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
//!
//! Next: `spitxrx` (SPI) or `uartecho` (UART) — the same shape on a
//! different bus.

#![no_std]
#![no_main]

use api::bus::{BusConfig, BusHandle, BusSpeed};
use api::task;
use api::Once;
use hal::types::Priority;

use bmi270::{Bmi270, Identity};
use esp32_i2c::Esp32I2c;
use i2c_bus::I2cController;
use mpu6886::Mpu6886;

// The board is selected with `--features kernel/board-...` now, not an
// application feature, so this guard keys on the *fact* the app needs -- that
// the board declares an onboard IMU -- rather than on a board name or a feature
// this crate no longer declares. Only the Atom Matrix sets `BOARD.imu`, and it
// is the only board that fits one; this fires on any other board and says why.
const _: () = assert!(
    board::BOARD.imu.is_some(),
    "`imu` reads an onboard IMU, and only the M5Stack Atom Matrix declares one.\n\n\
     \tmake flash APP=imu BOARD=board-m5-atom-matrix\n\n\
     The Atom Lite has no IMU. To run this elsewhere, give that board's manifest \
     an `imu: Some(I2cAttachment { .. })` in its `BOARD`."
);

kernel::flint_app!(main, abi = 2);

use kernel::board::active as board;

/// The controller. I2C0 is otherwise unused in this build.
const I2C_BASE: u32 = soc_esp32::addr::I2C0_BASE;

/// The whole controller stack in one static: the Layer-2 `I2cController` owns
/// its Layer-1 `Esp32I2c` by value, so there is no separate driver static for
/// it to borrow. A per-device handle is a borrow of this, taken below.
static IMU_I2C: Once<I2cController<Esp32I2c>> = Once::new();

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

    let Some(ctrl) = (unsafe { bring_up_controller() }) else {
        api::log_error!("[imu] I2C controller init failed");
        park();
    };

    // Scan first. Without it a silent device and a dead controller look
    // identical from the driver's side, and every I2C bug found on this board
    // so far presented as "returns Ok and does nothing". `scan` stays on the
    // Layer-2 side now -- the app no longer open-codes a raw physical probe.
    scan(ctrl);

    // A device handle addressed to the IMU: this is the Layer-2 `Bus` the
    // logical drivers talk through. `(&device).into()` builds the handle, so
    // the drivers take `new(handle)` with no `BusHandle::new` at the call site.
    let device = ctrl.device(board::IMU_I2C_ADDR);
    let bus = BusHandle::from(&device);

    match Bmi270::new(bus).probe() {
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
fn read_mpu6886(bus: BusHandle) -> ! {
    let dev = Mpu6886::new(bus);

    // The driver owns the reset/wake/configure sequence and its 10 ms datasheet
    // waits now; we only supply *how* to wait. How long to pause depends on the
    // board, not the part -- the Atom needs no more than the minimum.
    if let Err(e) = dev.bring_up(task::sleep_ms) {
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

/// Layers 1 and 2: clock the controller, route the pads, set the bit rate, and
/// wrap it in a Layer-2 `I2cController` that owns it.
///
/// # Safety
/// Claims I2C0 and the IMU's two pads for the life of the program.
// TODO(#109): once the board owns its devices, this becomes `board::imu_bus()`
// / `Esp32I2c::open(&IMU_I2C)` and the hand-rolled `new(base)` + `init` go away.
unsafe fn bring_up_controller() -> Option<&'static I2cController<Esp32I2c>> {
    let mut phys = Esp32I2c::new(I2C_BASE);
    // 100 kHz. The part handles 400 kHz, but bring-up is not the time to find
    // out whether the bus is marginal.
    let config = BusConfig::i2c(board::IMU_SDA_GPIO, board::IMU_SCL_GPIO, BusSpeed::Standard100k);
    // `init` gates the clock on, un-resets the peripheral and routes the pads
    // open-drain -- in that order, because a running controller connected to a
    // still-push-pull pad can short against a device holding the line low.
    hal::bus::PhysicalBus::init(&mut phys, &config).ok()?;
    // The controller takes ownership of the driver; one static holds the stack.
    Some(IMU_I2C.init(I2cController::new(phys)))
}

/// Walk the 7-bit address space and report who acknowledges.
///
/// [`I2cController::scan`] does the probing (a zero-length write per address:
/// present devices ACK, absent ones NAK, no register touched); the app just
/// logs. This used to open-code the walk against the raw physical driver.
fn scan(ctrl: &I2cController<Esp32I2c>) {
    api::log_info!("[imu] scanning 0x08..0x77");
    let found = ctrl.scan(|addr| api::log_info!("[imu]   0x{:02X} responded", addr));
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
