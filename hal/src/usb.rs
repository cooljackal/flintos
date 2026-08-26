// SPDX-License-Identifier: Apache-2.0
//! USB device packets, not an addressed SPI/I2C transaction or a byte stream.
//! Controllers own packet memory; a class driver turns packets into a stream.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Busy,
    InvalidEndpoint,
    PacketTooLarge,
    Hardware,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointType {
    Bulk,
    Interrupt,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Reset,
    Setup([u8; 8]),
    Out(u8),
    InComplete(u8),
    Suspend,
    Resume,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResetTarget {
    Application,
    Bootloader,
}

/// Exclusive, full-speed device controller. Every call is bounded and nonblocking.
/// Endpoint addresses include bit 7 for IN. EP0 is always open, 64 bytes.
/// `poll` consumes one event; call periodically even without interrupts (errata).
/// Reset/SETUP cancel old EP0 work before returning their event. Reset closes
/// all other endpoints. Buffer ownership transfers only on successful calls.
pub trait DeviceController: Send {
    fn poll(&mut self) -> Result<Option<Event>, Error>;
    fn configure(&mut self, endpoint: u8, kind: EndpointType) -> Result<(), Error>;
    fn close_data_endpoints(&mut self);
    fn set_address(&mut self, address: u8);
    fn write_in(&mut self, endpoint: u8, data: &[u8]) -> Result<(), Error>;
    fn arm_out(&mut self, endpoint: u8) -> Result<(), Error>;
    /// Consume one OUT packet; insufficient space is an error, never truncation.
    fn read_out(&mut self, endpoint: u8, data: &mut [u8]) -> Result<usize, Error>;
    /// Stall/un-stall and reset that endpoint's data toggle on clear.
    fn set_stall(&mut self, endpoint: u8, stalled: bool) -> Result<(), Error>;
    fn stalled(&self, endpoint: u8) -> bool;
}
