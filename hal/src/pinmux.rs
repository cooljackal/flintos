// SPDX-License-Identifier: Apache-2.0

//! Pin routing: connecting a peripheral signal to a physical pin.
//!
//! This is the one hardware operation that has no portable implementation, only
//! a portable *contract*. Every SoC family solves it differently:
//!
//! - **ESP32** has a GPIO matrix — nearly any signal can reach nearly any pin,
//!   via a routing table, plus a small set of IO_MUX "native" pins that bypass
//!   the matrix for lower latency.
//! - **STM32** has alternate functions — each pad has a fixed, short list of
//!   functions and you select one by index. A signal that is not on that pad's
//!   list cannot reach it at all.
//! - **NXP / i.MX** has IOMUXC, a third model again.
//!
//! A driver that wants SDA on GPIO 21 should not have to know which of those it
//! is talking to, and a board manifest cannot express any of them. So the SoC
//! layer implements [`PinMux`], the driver calls [`PinMux::route`], and a
//! request the silicon cannot satisfy comes back as
//! `BusError::InvalidConfig` rather than as a peripheral quietly wired to
//! nothing.

use crate::bus::BusResult;

// ── Signals ─────────────────────────────────────────────────────────────────

/// A peripheral signal that can be routed to a pin.
///
/// The `u8` is the *controller instance*, not a pin: `Signal::I2cSda(0)` is
/// I2C0's SDA line. Which controllers exist is a property of the SoC, so
/// routing a signal for an instance the chip does not have is an error, not a
/// compile failure — a board manifest is data, and data can be wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// UART transmit, output.
    UartTx(u8),
    /// UART receive, input.
    UartRx(u8),
    /// UART clear-to-send, input.
    UartCts(u8),
    /// UART request-to-send, output.
    UartRts(u8),
    /// I2C data. Bidirectional and open-drain by nature.
    I2cSda(u8),
    /// I2C clock. Bidirectional in a multi-master or clock-stretching bus.
    I2cScl(u8),
    /// SPI controller-out peripheral-in, output.
    SpiMosi(u8),
    /// SPI controller-in peripheral-out, input.
    SpiMiso(u8),
    /// SPI clock, output.
    SpiSck(u8),
    /// SPI chip select, output.
    SpiCs(u8),
    /// RMT channel output — pulse trains, e.g. addressable LEDs.
    RmtOut(u8),
    /// LEDC high-speed PWM channel n (0..8).
    LedcHs(u8),
    /// TWAI (CAN) transmit, output. One controller, so no instance number.
    TwaiTx,
    /// TWAI (CAN) receive, input.
    TwaiRx,
    /// I2S0 serial data out, output.
    I2sTxData,
    /// I2S0 serial data in, input.
    I2sRxData,
}

impl Signal {
    /// The controller instance this signal belongs to.
    pub fn instance(&self) -> u8 {
        match self {
            Signal::UartTx(n)
            | Signal::UartRx(n)
            | Signal::UartCts(n)
            | Signal::UartRts(n)
            | Signal::I2cSda(n)
            | Signal::I2cScl(n)
            | Signal::SpiMosi(n)
            | Signal::SpiMiso(n)
            | Signal::SpiSck(n)
            | Signal::SpiCs(n)
            | Signal::RmtOut(n)
            | Signal::LedcHs(n) => *n,
            // Single-instance controllers.
            Signal::TwaiTx | Signal::TwaiRx | Signal::I2sTxData | Signal::I2sRxData => 0,
        }
    }

    /// Whether the peripheral drives this signal out of the chip.
    ///
    /// I2C signals are both: the controller drives them low and releases them
    /// to be pulled high, and it reads them back for arbitration and clock
    /// stretching. Routing them needs an input path *and* an output path.
    pub fn direction(&self) -> SignalDirection {
        match self {
            Signal::UartTx(_)
            | Signal::UartRts(_)
            | Signal::SpiMosi(_)
            | Signal::SpiSck(_)
            | Signal::SpiCs(_)
            | Signal::RmtOut(_)
            | Signal::LedcHs(_)
            | Signal::TwaiTx
            | Signal::I2sTxData => SignalDirection::Output,
            Signal::UartRx(_)
            | Signal::UartCts(_)
            | Signal::SpiMiso(_)
            | Signal::TwaiRx
            | Signal::I2sRxData => SignalDirection::Input,
            Signal::I2cSda(_) | Signal::I2cScl(_) => SignalDirection::Bidirectional,
        }
    }
}

/// Which way a signal flows through the pad.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignalDirection {
    /// Peripheral reads the pad.
    Input,
    /// Peripheral drives the pad.
    Output,
    /// Both, as on an I2C line.
    Bidirectional,
}

// ── Pad configuration ───────────────────────────────────────────────────────

/// How the pad drives its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinDrive {
    /// Pad drives both high and low.
    PushPull,
    /// Pad drives low and releases high, so several devices can share the line.
    /// Required for I2C.
    OpenDrain,
}

/// Internal pull resistor selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinPull {
    None,
    Up,
    Down,
}

/// Everything about a pad that is not "which signal".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PinConfig {
    pub drive: PinDrive,
    pub pull: PinPull,
}

impl PinConfig {
    /// A plain push-pull pad with no pull resistor. The default for most
    /// signals.
    pub const PUSH_PULL: Self = Self {
        drive: PinDrive::PushPull,
        pull: PinPull::None,
    };

    /// Open-drain with the internal pull-up engaged.
    ///
    /// This is the I2C configuration, but the internal pull-up is weak — tens
    /// of kΩ — and is a convenience for a short bus with one device, not a
    /// substitute for proper external pull-ups. A board with several devices or
    /// any real trace length needs them.
    pub const OPEN_DRAIN_PULLUP: Self = Self {
        drive: PinDrive::OpenDrain,
        pull: PinPull::Up,
    };
}

impl Default for PinConfig {
    fn default() -> Self {
        Self::PUSH_PULL
    }
}

// ── The trait ───────────────────────────────────────────────────────────────

/// Connects peripheral signals to pins, in whatever idiom the SoC uses.
///
/// Implemented once per SoC family (`soc-esp32`, `soc-stm32f4`,
/// ...), never per board and never per driver.
pub trait PinMux {
    /// Whether this silicon could route `signal` to `pin`, without doing it.
    ///
    /// Pure — touches no registers. A driver that needs several pins should
    /// check them all before routing any: routing is not transactional, and a
    /// bus left with one line connected and the other not is a worse state than
    /// one that never started. It is also the only part of routing that can be
    /// tested off-target.
    ///
    /// # Errors
    /// `BusError::InvalidConfig` if the silicon cannot connect that signal to
    /// that pin — an unknown or unbonded pin, a controller instance the chip
    /// does not have, an output signal on an input-only pad, or a pad whose
    /// function list does not include the signal.
    ///
    /// `BusError::InvalidConfig`: crate::bus::BusError::InvalidConfig
    fn can_route(&self, signal: Signal, pin: u8) -> BusResult<()>;

    /// Route `signal` to `pin`, configuring the pad per `config`.
    ///
    /// Fails with the same errors as [`PinMux::can_route`], which it checks
    /// before touching any register.
    fn route(&self, signal: Signal, pin: u8, config: PinConfig) -> BusResult<()>;

    /// Whether `pin` is the signal's IO_MUX-native pad — reachable without
    /// going through a routing matrix.
    ///
    /// Native routing has lower latency and, on some silicon, is the only way
    /// to run a bus at its maximum clock. Drivers may use this to warn, or to
    /// cap a requested speed; they are not required to care.
    fn is_native(&self, signal: Signal, pin: u8) -> bool;
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_is_the_controller_not_the_pin() {
        assert_eq!(Signal::I2cSda(1).instance(), 1);
        assert_eq!(Signal::UartTx(2).instance(), 2);
    }

    #[test]
    fn i2c_lines_are_bidirectional() {
        // The whole reason I2C routing is harder than UART routing: both
        // directions have to be wired, and the pad has to be open-drain.
        assert_eq!(Signal::I2cSda(0).direction(), SignalDirection::Bidirectional);
        assert_eq!(Signal::I2cScl(0).direction(), SignalDirection::Bidirectional);
    }

    #[test]
    fn uart_and_spi_directions() {
        assert_eq!(Signal::UartTx(0).direction(), SignalDirection::Output);
        assert_eq!(Signal::UartRx(0).direction(), SignalDirection::Input);
        assert_eq!(Signal::SpiMosi(0).direction(), SignalDirection::Output);
        assert_eq!(Signal::SpiMiso(0).direction(), SignalDirection::Input);
        assert_eq!(Signal::SpiSck(0).direction(), SignalDirection::Output);
    }

    #[test]
    fn i2c_preset_is_open_drain_with_a_pull_up() {
        assert_eq!(PinConfig::OPEN_DRAIN_PULLUP.drive, PinDrive::OpenDrain);
        assert_eq!(PinConfig::OPEN_DRAIN_PULLUP.pull, PinPull::Up);
    }

    #[test]
    fn default_is_push_pull_unpulled() {
        assert_eq!(PinConfig::default(), PinConfig::PUSH_PULL);
        assert_eq!(PinConfig::default().pull, PinPull::None);
    }
}
