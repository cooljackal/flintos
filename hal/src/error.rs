// SPDX-License-Identifier: Apache-2.0

//! One error type an application can `?` into.
//!
//! Every subsystem keeps its own small enum — a bus timeout is not a Wi-Fi
//! refusal, and a driver should say precisely what went wrong. But an
//! application that talks to three of them does not want three `map_err`
//! closures per function. [`Error`] is the sum of those enums, with a `From`
//! per subsystem so `?` converts, plus the handful of failures that belong to
//! no subsystem in particular (`NotInitialised`, `WrongDevice`, ...).
//!
//! Drivers outside `hal` add `impl From<TheirError> for hal::Error` at the
//! bottom of their crate; a driver with nothing more specific to say maps to
//! [`Error::Other`] with a static message.

use core::fmt;

use crate::bus::BusError;
use crate::dma::DmaError;
use crate::wifi::WifiError;

/// Why a peripheral's interrupt could not be delivered to a handler.
///
/// Returned by the kernel's `interrupt::connect`; the enum lives here so a
/// driver can name it without naming the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    /// The crossbar refused the pairing — no such source, or a CPU input the
    /// kernel could not service.
    Route,
    /// That CPU input already has a handler. Deliberately not silent: a second
    /// registration would be unreachable, because dispatch stops at the first
    /// match.
    AlreadyRegistered,
}

impl fmt::Display for ConnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Route => f.write_str("no interrupt route for that source"),
            Self::AlreadyRegistered => f.write_str("CPU interrupt already has a handler"),
        }
    }
}

/// The one error an application sees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A bus transfer failed.
    Bus(BusError),
    /// A Wi-Fi station call was refused.
    Wifi(WifiError),
    /// The DMA broker refused or lost a transfer.
    Dma(DmaError),
    /// A peripheral interrupt could not be wired to its handler.
    Interrupt(ConnectError),
    /// The subsystem was used before it was brought up.
    NotInitialised,
    /// The hardware, or this driver, cannot do what was asked.
    Unsupported,
    /// A device answered with an identity other than the one expected —
    /// typically a chip-id register.
    WrongDevice {
        /// The id the driver was written for.
        expected: u8,
        /// The id the device reported.
        found: u8,
    },
    /// Anything else, with a static message for the log.
    Other(&'static str),
}

/// A result whose error is [`Error`].
pub type Result<T> = core::result::Result<T, Error>;

impl From<BusError> for Error {
    fn from(e: BusError) -> Self {
        Self::Bus(e)
    }
}

impl From<WifiError> for Error {
    fn from(e: WifiError) -> Self {
        Self::Wifi(e)
    }
}

impl From<DmaError> for Error {
    fn from(e: DmaError) -> Self {
        Self::Dma(e)
    }
}

impl From<ConnectError> for Error {
    fn from(e: ConnectError) -> Self {
        Self::Interrupt(e)
    }
}

impl From<&'static str> for Error {
    fn from(msg: &'static str) -> Self {
        Self::Other(msg)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bus(e) => write!(f, "bus: {e}"),
            Self::Wifi(e) => write!(f, "wifi: {e}"),
            Self::Dma(e) => write!(f, "dma: {e}"),
            Self::Interrupt(e) => write!(f, "interrupt: {e}"),
            Self::NotInitialised => f.write_str("not initialised"),
            Self::Unsupported => f.write_str("unsupported"),
            Self::WrongDevice { expected, found } => {
                write!(f, "wrong device: expected id {expected:#04x}, found {found:#04x}")
            }
            Self::Other(msg) => f.write_str(msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn via_question_mark() -> Result<()> {
        Err(BusError::Timeout)?;
        Ok(())
    }

    #[test]
    fn question_mark_converts_subsystem_errors() {
        assert_eq!(via_question_mark(), Err(Error::Bus(BusError::Timeout)));
        assert_eq!(Error::from(WifiError::Busy), Error::Wifi(WifiError::Busy));
        assert_eq!(Error::from(DmaError::Timeout), Error::Dma(DmaError::Timeout));
        assert_eq!(Error::from(ConnectError::Route), Error::Interrupt(ConnectError::Route));
        assert_eq!(Error::from("boom"), Error::Other("boom"));
    }

    /// `no_std`: render into a fixed buffer rather than a `String`.
    struct Buf([u8; 64], usize);
    impl fmt::Write for Buf {
        fn write_str(&mut self, s: &str) -> fmt::Result {
            let end = self.1 + s.len();
            self.0[self.1..end].copy_from_slice(s.as_bytes());
            self.1 = end;
            Ok(())
        }
    }

    fn render(e: Error) -> Buf {
        let mut b = Buf([0; 64], 0);
        fmt::Write::write_fmt(&mut b, format_args!("{e}")).unwrap();
        b
    }

    #[test]
    fn display_names_the_subsystem() {
        let b = render(Error::Bus(BusError::Timeout));
        assert_eq!(core::str::from_utf8(&b.0[..b.1]).unwrap(), "bus: Timeout");

        let b = render(Error::WrongDevice { expected: 0x60, found: 0x58 });
        assert_eq!(
            core::str::from_utf8(&b.0[..b.1]).unwrap(),
            "wrong device: expected id 0x60, found 0x58"
        );
    }
}
