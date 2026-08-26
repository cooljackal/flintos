// SPDX-License-Identifier: Apache-2.0
use super::{buffer_offset, DeviceController, EndpointType, Error, Event};
use soc_rp2040::{ctrl, USB_DPRAM_BASE as RAM, USB_REGS_BASE as REGS};

const AVAIL: u32 = 1 << 10;
const STALL: u32 = 1 << 11;
const DATA1: u32 = 1 << 13;
const FULL: u32 = 1 << 15;
const RESET: u32 = 1 << 12;
const SETUP: u32 = 1 << 16;
const SOF: u32 = 1 << 17;
const INTE: u32 = RESET | SETUP | SOF | (1 << 4) | (1 << 14) | (1 << 15);

fn reg(offset: u32) -> *mut u32 {
    (REGS + offset) as *mut u32
}
fn read(offset: u32) -> u32 {
    unsafe { reg(offset).read_volatile() }
}
fn write(offset: u32, value: u32) {
    unsafe { reg(offset).write_volatile(value) }
}
fn clear(offset: u32, value: u32) {
    write(offset + 0x3000, value);
}
fn set(offset: u32, value: u32) {
    write(offset + 0x2000, value);
}
fn slot(ep: u8) -> usize {
    usize::from(ep & 15) * 2 + usize::from(ep & 0x80 == 0)
}
fn buf(ep: u8) -> *mut u32 {
    (RAM + 0x80 + slot(ep) as u32 * 4) as *mut u32
}
fn epctrl(ep: u8) -> *mut u32 {
    (RAM + 8 + (slot(ep) - 2) as u32 * 4) as *mut u32
}
fn now() -> u32 {
    soc_rp2040::timer_us()
}

#[derive(Clone, Copy, Default)]
struct Endpoint {
    enabled: bool,
    bulk: bool,
    busy: bool,
    ready: bool,
    pid: bool,
    deferred: u32,
}

pub struct Controller {
    endpoints: [Endpoint; 32],
    old_chip: bool,
    e5_phase: u8,
    e5_since: u32,
    saved_gpio: u32,
    saved_pad: u32,
    last_sof: u32,
    failed: bool,
}

impl Controller {
    pub fn open() -> Result<Self, Error> {
        if !ctrl::claim_usb() {
            return Err(Error::Busy);
        }
        let old_chip =
            super::old_revision(unsafe { (0x4000_0000 as *const u32).read_volatile() >> 28 });
        // E5 borrows the USB debug mux on GPIO15 and requires GPIO16 not to
        // select that mux. Reserve both, rather than silently disturb an app.
        if old_chip {
            if !ctrl::claim_gpio(15) {
                ctrl::release_usb();
                return Err(Error::Busy);
            }
            if !ctrl::claim_gpio(16) {
                ctrl::release_gpio(15);
                ctrl::release_usb();
                return Err(Error::Busy);
            }
        }
        let mut this = Self {
            endpoints: [Endpoint::default(); 32],
            old_chip,
            e5_phase: 0,
            e5_since: 0,
            saved_gpio: 0,
            saved_pad: 0,
            last_sof: 0,
            failed: false,
        };
        unsafe {
            soc_rp2040::reset(soc_rp2040::RESET_USBCTRL);
            // A debugger reset/image replacement may change the descriptors.
            // Give the host a real disconnect interval before re-enumerating.
            let detached = now();
            while now().wrapping_sub(detached) < 200_000 {
                core::hint::spin_loop();
            }
            if !soc_rp2040::enable_usb_clock() {
                return Err(Error::Hardware);
            }
            ((soc_rp2040::RESETS_BASE + 0x3000) as *mut u32)
                .write_volatile(soc_rp2040::RESET_USBCTRL);
            let start = now();
            while ((soc_rp2040::RESETS_BASE + 8) as *const u32).read_volatile()
                & soc_rp2040::RESET_USBCTRL
                == 0
            {
                if now().wrapping_sub(start) > 10_000 {
                    return Err(Error::Hardware);
                }
            }
            for offset in (0..4096).step_by(4) {
                ((RAM + offset) as *mut u32).write_volatile(0);
            }
        }
        write(0x74, 9); // PHY + software connect.
        write(0x78, 12); // Force VBUS detect (Pico SDK device-mode convention).
        write(0x40, 1); // Device, not host.
        write(0x4c, 1 << 29); // Single-buffer EP0 interrupt.
        this.reset_control();
        write(0x90, INTE);
        set(0x4c, 1 << 16); // Connect only after state and interrupts are ready.
        Ok(this)
    }

    fn reset_control(&mut self) {
        for ep in [0x80, 0] {
            // Match the SDK: abort only an active endpoint on B2+. E2 makes
            // abort unusable on B0/B1, whose best-effort buffer clear cannot
            // guarantee hardware cancellation of an in-flight packet.
            if !self.old_chip && self.endpoints[slot(ep)].busy {
                let mask = 1 << slot(ep);
                set(0x60, mask);
                let start = now();
                while read(0x64) & mask == 0 {
                    if now().wrapping_sub(start) > 100 {
                        self.failed = true;
                        break;
                    }
                }
                clear(0x64, mask);
                clear(0x60, mask);
            }
            unsafe {
                buf(ep).write_volatile(DATA1 | (1 << 12));
            }
            self.endpoints[slot(ep)] = Endpoint {
                enabled: true,
                pid: true,
                ..Endpoint::default()
            };
        }
        clear(0x58, 3);
    }

    fn validate(&self, ep: u8, input: bool) -> Result<usize, Error> {
        buffer_offset(ep)?;
        if (ep & 0x80 != 0) != input || !self.endpoints[slot(ep)].enabled {
            return Err(Error::InvalidEndpoint);
        }
        Ok(slot(ep))
    }

    fn arm_buffer(&mut self, ep: u8, value: u32) {
        unsafe {
            buf(ep).write_volatile(value & !AVAIL);
            // Datasheet 4.1.2.5.1: control writes must precede ownership by
            // at least one USB cycle. Twelve CPU cycles cover this at 125 MHz,
            // matching the SDK's conservative delay. Not a compiler fence.
            core::arch::asm!(".rept 12", "nop", ".endr", options(nomem, nostack));
            buf(ep).write_volatile(value);
        }
    }

    fn restore_e5(&mut self) {
        if self.e5_phase == 2 {
            write(0x74, 9);
            clear(0x80, 4); // Undo DP pullup override, not the normal pullup.
            unsafe {
                (0x4001_407c as *mut u32).write_volatile(self.saved_gpio);
                (0x4001_c040 as *mut u32).write_volatile(self.saved_pad);
            }
        }
        self.e5_phase = 0;
    }

    fn service_e5(&mut self) -> Result<(), Error> {
        let elapsed = now().wrapping_sub(self.e5_since);
        if self.e5_phase == 1 {
            if read(0x50) & 12 != 0 {
                unsafe {
                    self.saved_gpio = (0x4001_407c as *const u32).read_volatile();
                    self.saved_pad = (0x4001_c040 as *const u32).read_volatile();
                    // GPIO15: pulls both on, OE forced low, input forced high,
                    // USB debug function. GPIO16 stays outside function 8.
                    (0x4001_c040 as *mut u32).write_volatile(self.saved_pad | 12);
                    (0x4001_407c as *mut u32).write_volatile(
                        (self.saved_gpio & !((3 << 12) | (3 << 16) | 31))
                            | (2 << 12)
                            | (3 << 16)
                            | 8,
                    );
                }
                set(0x7c, 2);
                set(0x80, 4);
                write(0x74, 12);
                self.e5_since = now();
                self.e5_phase = 2;
            } else if elapsed > 100_000 {
                return Err(Error::Hardware);
            }
        } else if self.e5_phase == 2 && elapsed >= 1_000 {
            if read(0x50) & (1 << 16) != 0 {
                self.restore_e5();
            } else if elapsed > 10_000 {
                return Err(Error::Hardware);
            }
        }
        Ok(())
    }
}

impl DeviceController for Controller {
    fn poll(&mut self) -> Result<Option<Event>, Error> {
        if self.failed {
            return Err(Error::Hardware);
        }
        if self.service_e5().is_err() {
            self.restore_e5();
            clear(0x4c, 1 << 16);
            self.failed = true;
            return Err(Error::Hardware);
        }
        let ints = read(0x98);
        if ints & RESET != 0 {
            self.restore_e5();
            write(0, 0);
            self.close_data_endpoints();
            self.reset_control();
            clear(0x50, (1 << 19) | (1 << 17));
            if self.old_chip {
                self.e5_since = now();
                self.e5_phase = 1;
            }
            return Ok(Some(Event::Reset));
        }
        if ints & SOF != 0 {
            let _frame = read(0x48); // Reading SOF_RD clears DEV_SOF.
            self.last_sof = now();
            for ep in 1..16 {
                let addr = ep | 0x80;
                let pending = self.endpoints[slot(addr)].deferred;
                if pending != 0 {
                    self.arm_buffer(addr, pending);
                    self.endpoints[slot(addr)].deferred = 0;
                }
            }
        }
        if ints & SETUP != 0 {
            let mut setup = [0; 8];
            for (i, byte) in setup.iter_mut().enumerate() {
                *byte = unsafe { ((RAM + i as u32) as *const u8).read_volatile() };
            }
            self.reset_control();
            clear(0x50, 1 << 17);
            return Ok(Some(Event::Setup(setup)));
        }
        let status = read(0x58);
        if status != 0 {
            let index = status.trailing_zeros() as usize;
            clear(0x58, 1 << index);
            let ep = (index / 2) as u8;
            self.endpoints[index].busy = false;
            if index & 1 == 0 {
                return Ok(Some(Event::InComplete(ep | 0x80)));
            }
            self.endpoints[index].ready = true;
            return Ok(Some(Event::Out(ep)));
        }
        if ints & (1 << 14) != 0 {
            clear(0x50, 1 << 4);
            return Ok(Some(Event::Suspend));
        }
        if ints & (1 << 15) != 0 {
            clear(0x50, 1 << 11);
            return Ok(Some(Event::Resume));
        }
        Ok(None)
    }
    fn configure(&mut self, ep: u8, kind: EndpointType) -> Result<(), Error> {
        let offset = buffer_offset(ep)?;
        if ep & 15 == 0 {
            return Err(Error::InvalidEndpoint);
        }
        if self.endpoints[slot(ep)].enabled {
            return Err(Error::Busy);
        }
        let transfer_type = match kind {
            EndpointType::Bulk => 2,
            EndpointType::Interrupt => 3,
        };
        self.endpoints[slot(ep)] = Endpoint {
            enabled: true,
            bulk: kind == EndpointType::Bulk,
            ..Endpoint::default()
        };
        unsafe {
            buf(ep).write_volatile(0);
            epctrl(ep).write_volatile((1 << 31) | (1 << 29) | (transfer_type << 26) | offset);
        }
        Ok(())
    }
    fn close_data_endpoints(&mut self) {
        for ep in 1..16 {
            for addr in [ep, ep | 0x80] {
                unsafe {
                    epctrl(addr).write_volatile(0);
                    buf(addr).write_volatile(0);
                }
                self.endpoints[slot(addr)] = Endpoint::default();
            }
        }
        clear(0x58, !3);
    }
    fn set_address(&mut self, address: u8) {
        write(0, u32::from(address & 127));
    }
    fn write_in(&mut self, ep: u8, data: &[u8]) -> Result<(), Error> {
        let index = self.validate(ep, true)?;
        if data.len() > 64 {
            return Err(Error::PacketTooLarge);
        }
        if self.endpoints[index].busy || self.stalled(ep) {
            return Err(Error::Busy);
        }
        let offset = buffer_offset(ep)?;
        for (i, byte) in data.iter().enumerate() {
            unsafe {
                ((RAM + offset + i as u32) as *mut u8).write_volatile(*byte);
            }
        }
        let value = data.len() as u32
            | AVAIL
            | FULL
            | (1 << 14)
            | if self.endpoints[index].pid { DATA1 } else { 0 };
        self.endpoints[index].pid = !self.endpoints[index].pid;
        self.endpoints[index].busy = true;
        let since_sof = now().wrapping_sub(self.last_sof);
        if self.endpoints[index].bulk && (800..=998).contains(&since_sof) {
            self.endpoints[index].deferred = value;
        } else {
            self.arm_buffer(ep, value);
        }
        Ok(())
    }
    fn arm_out(&mut self, ep: u8) -> Result<(), Error> {
        let index = self.validate(ep, false)?;
        if self.endpoints[index].busy || self.endpoints[index].ready || self.stalled(ep) {
            return Err(Error::Busy);
        }
        let value = 64 | AVAIL | if self.endpoints[index].pid { DATA1 } else { 0 };
        self.endpoints[index].pid = !self.endpoints[index].pid;
        self.endpoints[index].busy = true;
        self.arm_buffer(ep, value);
        Ok(())
    }
    fn read_out(&mut self, ep: u8, data: &mut [u8]) -> Result<usize, Error> {
        let index = self.validate(ep, false)?;
        if !self.endpoints[index].ready {
            return Err(Error::Busy);
        }
        let len = unsafe { buf(ep).read_volatile() as usize & 0x3ff };
        if len > 64 || len > data.len() {
            return Err(Error::PacketTooLarge);
        }
        let offset = buffer_offset(ep)?;
        for (i, byte) in data[..len].iter_mut().enumerate() {
            *byte = unsafe { ((RAM + offset + i as u32) as *const u8).read_volatile() };
        }
        self.endpoints[index].ready = false;
        Ok(len)
    }
    fn set_stall(&mut self, ep: u8, stalled: bool) -> Result<(), Error> {
        self.validate(ep, ep & 0x80 != 0)?;
        if ep & 15 == 0 && stalled {
            set(0x68, if ep & 0x80 != 0 { 1 } else { 2 });
        }
        unsafe {
            buf(ep).write_volatile(if stalled { STALL } else { 0 });
        }
        let state = &mut self.endpoints[slot(ep)];
        state.busy = false;
        state.ready = false;
        state.deferred = 0;
        state.pid = false;
        clear(0x58, 1 << slot(ep));
        Ok(())
    }
    fn stalled(&self, ep: u8) -> bool {
        buffer_offset(ep).is_ok() && unsafe { buf(ep).read_volatile() & STALL != 0 }
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        write(0x90, 0);
        clear(0x4c, 1 << 16);
        self.restore_e5();
        unsafe {
            soc_rp2040::reset(soc_rp2040::RESET_USBCTRL);
        }
        if self.old_chip {
            ctrl::release_gpio(15);
            ctrl::release_gpio(16);
        }
        ctrl::release_usb();
    }
}
