// SPDX-License-Identifier: Apache-2.0
//! Portable, allocation-free USB CDC device service. The board constructs it;
//! the controller only handles packets. Call `service` from its IRQ and at least
//! once per millisecond from a task, then handle `take_reset` in task context.
#![no_std]
#![forbid(unsafe_code)]
mod descriptors;
use api::usb::{DeviceController, EndpointType, Error, Event, ResetTarget};
use api::{ByteStream, CsCell, StreamErrors};

#[derive(Clone, Copy)]
pub struct Identity {
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: &'static str,
    pub product: &'static str,
    /// Optional real board identity. Never substitute the firmware version or
    /// a shared fake serial; a host must bind by USB topology when absent.
    pub serial: Option<&'static str>,
    /// Reboot commands are an explicit development policy, not always enabled.
    pub allow_reset: bool,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Status {
    pub configured: bool,
    pub connected: bool,
    pub resets: u32,
    pub stalls: u32,
    pub setups: u32,
    pub received: u32,
    pub transmitted: u32,
    /// All queued CDC bytes and the final packet have been acknowledged.
    pub transmit_idle: bool,
}

/// Board-facing service; applications need not name a physical controller.
pub trait Serial: ByteStream {
    fn service(&self) -> Result<(), Error>;
    fn status(&self) -> Status;
    fn take_reset(&self) -> Option<ResetTarget>;
}

pub struct UsbSerial<C: DeviceController> {
    inner: CsCell<Device<C>>,
}
impl<C: DeviceController> UsbSerial<C> {
    pub fn new(controller: C, identity: Identity) -> Self {
        Self {
            inner: CsCell::new(Device::new(controller, identity)),
        }
    }
    /// At most 16 events and one outgoing data packet per call; no waiting for
    /// a host, an endpoint, or a task. This keeps the kernel critical section short.
    pub fn service(&self) -> Result<(), Error> {
        self.inner.with(Device::service)
    }
    pub fn status(&self) -> Status {
        self.inner.with(|d| d.status())
    }
    /// Reset is published only after the control status packet was acknowledged.
    pub fn take_reset(&self) -> Option<ResetTarget> {
        self.inner.with(|d| d.reset.take())
    }
}
impl<C: DeviceController> ByteStream for UsbSerial<C> {
    fn write(&self, data: &[u8]) -> usize {
        self.inner.with(|d| {
            if d.status().connected {
                d.tx.push(data)
            } else {
                0
            }
        })
    }
    fn read(&self, data: &mut [u8]) -> usize {
        self.inner.with(|d| d.rx.pop(data))
    }
    fn errors(&self) -> StreamErrors {
        StreamErrors::default()
    }
}
impl<C: DeviceController> Serial for UsbSerial<C> {
    fn service(&self) -> Result<(), Error> {
        UsbSerial::service(self)
    }
    fn status(&self) -> Status {
        UsbSerial::status(self)
    }
    fn take_reset(&self) -> Option<ResetTarget> {
        UsbSerial::take_reset(self)
    }
}

struct Ring {
    bytes: [u8; 512],
    start: usize,
    len: usize,
}
impl Ring {
    const fn new() -> Self {
        Self {
            bytes: [0; 512],
            start: 0,
            len: 0,
        }
    }
    fn push(&mut self, data: &[u8]) -> usize {
        let count = data.len().min(512 - self.len);
        for (i, b) in data[..count].iter().enumerate() {
            self.bytes[(self.start + self.len + i) % 512] = *b;
        }
        self.len += count;
        count
    }
    fn peek(&self, out: &mut [u8]) -> usize {
        let count = out.len().min(self.len);
        for (i, b) in out[..count].iter_mut().enumerate() {
            *b = self.bytes[(self.start + i) % 512];
        }
        count
    }
    fn pop(&mut self, out: &mut [u8]) -> usize {
        let count = self.peek(out);
        self.discard(count);
        count
    }
    fn discard(&mut self, count: usize) {
        self.start = (self.start + count) % 512;
        self.len -= count;
    }
    fn clear(&mut self) {
        self.start = 0;
        self.len = 0;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Setup {
    kind: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
}
impl From<[u8; 8]> for Setup {
    fn from(p: [u8; 8]) -> Self {
        Self {
            kind: p[0],
            request: p[1],
            value: u16::from_le_bytes([p[2], p[3]]),
            index: u16::from_le_bytes([p[4], p[5]]),
            length: u16::from_le_bytes([p[6], p[7]]),
        }
    }
}
#[derive(Clone, Copy)]
enum Action {
    None,
    Address(u8),
    Configuration(u8),
    Lines(u16),
    Halt(u8, bool),
    Reset(ResetTarget),
}
#[derive(Clone, Copy)]
enum Control {
    Idle,
    In { len: usize, sent: usize, zlp: bool },
    StatusOut,
    LineCoding,
    StatusIn(Action),
}

struct Device<C: DeviceController> {
    controller: C,
    identity: Identity,
    control: Control,
    reply: [u8; 256],
    line_coding: [u8; 7],
    address: u8,
    configuration: u8,
    lines: u16,
    suspended: bool,
    rx: Ring,
    tx: Ring,
    rx_armed: bool,
    tx_busy: bool,
    tx_zlp: bool,
    reset: Option<ResetTarget>,
    stats: Status,
}
impl<C: DeviceController> Device<C> {
    fn new(controller: C, identity: Identity) -> Self {
        Self {
            controller,
            identity,
            control: Control::Idle,
            reply: [0; 256],
            line_coding: [0, 0xc2, 1, 0, 0, 0, 8],
            address: 0,
            configuration: 0,
            lines: 0,
            suspended: false,
            rx: Ring::new(),
            tx: Ring::new(),
            rx_armed: false,
            tx_busy: false,
            tx_zlp: false,
            reset: None,
            stats: Status::default(),
        }
    }
    fn status(&self) -> Status {
        Status {
            transmit_idle: self.tx.len == 0 && !self.tx_busy && !self.tx_zlp,
            configured: self.configuration != 0,
            connected: self.configuration != 0 && self.lines & 1 != 0 && !self.suspended,
            ..self.stats
        }
    }
    fn clear_data(&mut self) {
        self.rx.clear();
        self.tx.clear();
        self.rx_armed = false;
        self.tx_busy = false;
        self.tx_zlp = false;
    }
    fn service(&mut self) -> Result<(), Error> {
        for _ in 0..16 {
            let Some(event) = self.controller.poll()? else {
                break;
            };
            match event {
                Event::Reset => {
                    self.configuration = 0;
                    self.address = 0;
                    self.lines = 0;
                    self.suspended = false;
                    self.control = Control::Idle;
                    self.reset = None;
                    self.clear_data();
                    self.stats.resets = self.stats.resets.wrapping_add(1);
                }
                Event::Setup(bytes) => {
                    self.stats.setups = self.stats.setups.wrapping_add(1);
                    self.control = Control::Idle;
                    self.setup(bytes.into())?;
                }
                Event::Out(0) => self.control_out()?,
                Event::InComplete(0x80) => self.control_in()?,
                Event::Out(2) if self.configuration != 0 => {
                    let mut packet = [0; 64];
                    let len = self.controller.read_out(2, &mut packet)?;
                    if self.rx.push(&packet[..len]) != len {
                        return Err(Error::Hardware);
                    }
                    self.stats.received = self.stats.received.wrapping_add(len as u32);
                    self.rx_armed = false;
                }
                Event::InComplete(0x82) => self.tx_busy = false,
                Event::Suspend => self.suspended = true,
                Event::Resume => self.suspended = false,
                _ => {}
            }
        }
        if self.configuration != 0
            && !self.rx_armed
            && self.rx.len <= 448
            && !self.controller.stalled(2)
        {
            self.controller.arm_out(2)?;
            self.rx_armed = true;
        }
        if self.status().connected && !self.tx_busy && !self.controller.stalled(0x82) {
            let mut packet = [0; 64];
            let len = self.tx.peek(&mut packet);
            if len != 0 || self.tx_zlp {
                self.controller.write_in(0x82, &packet[..len])?;
                self.tx.discard(len);
                self.tx_busy = true;
                self.tx_zlp = len == 64;
                self.stats.transmitted = self.stats.transmitted.wrapping_add(len as u32);
            }
        }
        Ok(())
    }
    fn stall(&mut self) -> Result<(), Error> {
        self.stats.stalls = self.stats.stalls.wrapping_add(1);
        self.control = Control::Idle;
        self.controller.set_stall(0, true)?;
        self.controller.set_stall(0x80, true)
    }
    fn send_reply(&mut self, available: usize, requested: u16) -> Result<(), Error> {
        if requested == 0 {
            return self.stall();
        }
        let len = available.min(requested as usize);
        self.control = Control::In {
            len,
            sent: 0,
            zlp: len < requested as usize && len % 64 == 0,
        };
        self.next_in()
    }
    fn next_in(&mut self) -> Result<(), Error> {
        if let Control::In { len, sent, zlp } = self.control {
            if sent < len {
                let end = (sent + 64).min(len);
                self.controller.write_in(0x80, &self.reply[sent..end])?;
                self.control = Control::In {
                    len,
                    sent: end,
                    zlp,
                };
            } else if zlp {
                self.controller.write_in(0x80, &[])?;
                self.control = Control::In {
                    len,
                    sent,
                    zlp: false,
                };
            } else {
                self.controller.arm_out(0)?;
                self.control = Control::StatusOut;
            }
        }
        Ok(())
    }
    fn acknowledge(&mut self, action: Action) -> Result<(), Error> {
        self.controller.write_in(0x80, &[])?;
        self.control = Control::StatusIn(action);
        Ok(())
    }
    fn endpoint_valid(&self, index: u16) -> bool {
        matches!(index, 0 | 0x80) || self.configuration != 0 && matches!(index, 2 | 0x81 | 0x82)
    }
    fn setup(&mut self, s: Setup) -> Result<(), Error> {
        match (s.kind, s.request) {
            (0x80, 6) => {
                if let Some(len) =
                    descriptors::descriptor(&self.identity, s.value, s.index, &mut self.reply)
                {
                    return self.send_reply(len, s.length);
                }
            }
            (0, 5)
                if s.index == 0 && s.length == 0 && s.value <= 127 && self.configuration == 0 =>
            {
                return self.acknowledge(Action::Address(s.value as u8))
            }
            (0, 9) if s.index == 0 && s.length == 0 && s.value <= 1 && self.address != 0 => {
                return self.acknowledge(Action::Configuration(s.value as u8))
            }
            (0x80, 8) if s.index == 0 && s.value == 0 && s.length == 1 => {
                self.reply[0] = self.configuration;
                return self.send_reply(1, 1);
            }
            (0x80, 0) if s.index == 0 && s.value == 0 && s.length == 2 => {
                self.reply[..2].fill(0);
                return self.send_reply(2, 2);
            }
            (0x81, 0)
                if s.index < 3 && s.value == 0 && s.length == 2 && self.configuration != 0 =>
            {
                self.reply[..2].fill(0);
                return self.send_reply(2, 2);
            }
            (0x82, 0) if s.value == 0 && s.length == 2 && self.endpoint_valid(s.index) => {
                self.reply[0] = u8::from(self.controller.stalled(s.index as u8));
                self.reply[1] = 0;
                return self.send_reply(2, 2);
            }
            (2, 1 | 3)
                if s.value == 0
                    && s.length == 0
                    && s.index & 15 != 0
                    && self.endpoint_valid(s.index) =>
            {
                return self.acknowledge(Action::Halt(s.index as u8, s.request == 3))
            }
            (0x81, 10)
                if s.index < 3 && s.value == 0 && s.length == 1 && self.configuration != 0 =>
            {
                self.reply[0] = 0;
                return self.send_reply(1, 1);
            }
            (1, 11) if s.index < 3 && s.value == 0 && s.length == 0 && self.configuration != 0 => {
                return self.acknowledge(Action::None)
            }
            (0xa1, 0x21)
                if s.index == 0 && s.value == 0 && s.length == 7 && self.configuration != 0 =>
            {
                self.reply[..7].copy_from_slice(&self.line_coding);
                return self.send_reply(7, 7);
            }
            (0x21, 0x20)
                if s.index == 0 && s.value == 0 && s.length == 7 && self.configuration != 0 =>
            {
                self.controller.arm_out(0)?;
                self.control = Control::LineCoding;
                return Ok(());
            }
            (0x21, 0x22)
                if s.index == 0 && s.length == 0 && s.value <= 3 && self.configuration != 0 =>
            {
                return self.acknowledge(Action::Lines(s.value))
            }
            (0x21, 0x23) if s.index == 0 && s.length == 0 && self.configuration != 0 => {
                return self.acknowledge(Action::None)
            }
            (0xc0, 1) if s.index == 7 && s.value == 0 => {
                let len = descriptors::microsoft(&mut self.reply);
                return self.send_reply(len, s.length);
            }
            (0x41, 1 | 2)
                if s.index == 2
                    && s.value == 0
                    && s.length == 0
                    && self.configuration != 0
                    && self.identity.allow_reset =>
            {
                return self.acknowledge(Action::Reset(if s.request == 1 {
                    ResetTarget::Bootloader
                } else {
                    ResetTarget::Application
                }));
            }
            _ => {}
        }
        self.stall()
    }
    fn control_out(&mut self) -> Result<(), Error> {
        let mut packet = [0; 64];
        let len = self.controller.read_out(0, &mut packet)?;
        match self.control {
            Control::StatusOut if len == 0 => {
                self.control = Control::Idle;
                Ok(())
            }
            Control::LineCoding
                if len == 7 && packet[4] <= 2 && packet[5] <= 4 && (5..=8).contains(&packet[6]) =>
            {
                let baud = u32::from_le_bytes(packet[..4].try_into().unwrap());
                if baud == 0 {
                    return self.stall();
                }
                self.line_coding.copy_from_slice(&packet[..7]);
                self.acknowledge(if baud == 1200 && self.identity.allow_reset {
                    Action::Reset(ResetTarget::Bootloader)
                } else {
                    Action::None
                })
            }
            _ => self.stall(),
        }
    }
    fn control_in(&mut self) -> Result<(), Error> {
        match self.control {
            Control::In { .. } => self.next_in(),
            Control::StatusIn(action) => {
                self.control = Control::Idle;
                match action {
                    Action::None => {}
                    Action::Address(address) => {
                        self.controller.set_address(address);
                        self.address = address;
                    }
                    Action::Configuration(config) => {
                        self.controller.close_data_endpoints();
                        self.configuration = config;
                        self.lines = 0;
                        self.clear_data();
                        if config != 0 {
                            self.controller.configure(0x81, EndpointType::Interrupt)?;
                            self.controller.configure(2, EndpointType::Bulk)?;
                            self.controller.configure(0x82, EndpointType::Bulk)?;
                        }
                    }
                    Action::Lines(lines) => self.lines = lines,
                    Action::Halt(ep, halt) => {
                        self.controller.set_stall(ep, halt)?;
                        if ep == 2 {
                            self.rx_armed = false;
                        }
                        if ep == 0x82 {
                            self.tx_busy = false;
                            self.tx_zlp = false;
                        }
                    }
                    Action::Reset(target) => self.reset = Some(target),
                }
                Ok(())
            }
            _ => self.stall(),
        }
    }
}

#[cfg(test)]
mod tests;
