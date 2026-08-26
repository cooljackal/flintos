// SPDX-License-Identifier: Apache-2.0
//! Single-buffered USB device controller. No descriptors, CDC or kernel policy.
//! Register layout: RP2040 datasheet 4.1; sequencing cross-checked with Pico SDK
//! 2.1.1 / TinyUSB dcd_rp2040.c and rp2040_usb.c. E5 reserves GPIO15/16 on B0/B1;
//! E15 defers bulk IN in the final 200 us of a frame. No host/isochronous support.
#![no_std]
use hal::usb::{DeviceController, EndpointType, Error, Event};

#[cfg(target_arch = "arm")]
mod hardware;

/// A controller is exclusively owned and used through `&mut`, including its ISR.
pub struct Rp2040Usb {
    #[cfg(target_arch = "arm")]
    inner: hardware::Controller,
}

impl Rp2040Usb {
    /// Board opens once; a second open is refused. No GPIO signal rewiring.
    pub fn open() -> Result<Self, Error> {
        #[cfg(target_arch = "arm")]
        {
            Ok(Self {
                inner: hardware::Controller::open()?,
            })
        }
        #[cfg(not(target_arch = "arm"))]
        {
            Err(Error::Unsupported)
        }
    }
}

impl DeviceController for Rp2040Usb {
    fn poll(&mut self) -> Result<Option<Event>, Error> {
        #[cfg(target_arch = "arm")]
        {
            self.inner.poll()
        }
        #[cfg(not(target_arch = "arm"))]
        {
            Err(Error::Unsupported)
        }
    }
    fn configure(&mut self, endpoint: u8, kind: EndpointType) -> Result<(), Error> {
        #[cfg(target_arch = "arm")]
        {
            self.inner.configure(endpoint, kind)
        }
        #[cfg(not(target_arch = "arm"))]
        {
            let _ = (endpoint, kind);
            Err(Error::Unsupported)
        }
    }
    fn close_data_endpoints(&mut self) {
        #[cfg(target_arch = "arm")]
        self.inner.close_data_endpoints();
    }
    fn set_address(&mut self, address: u8) {
        #[cfg(target_arch = "arm")]
        self.inner.set_address(address);
        #[cfg(not(target_arch = "arm"))]
        let _ = address;
    }
    fn write_in(&mut self, endpoint: u8, data: &[u8]) -> Result<(), Error> {
        #[cfg(target_arch = "arm")]
        {
            self.inner.write_in(endpoint, data)
        }
        #[cfg(not(target_arch = "arm"))]
        {
            let _ = (endpoint, data);
            Err(Error::Unsupported)
        }
    }
    fn arm_out(&mut self, endpoint: u8) -> Result<(), Error> {
        #[cfg(target_arch = "arm")]
        {
            self.inner.arm_out(endpoint)
        }
        #[cfg(not(target_arch = "arm"))]
        {
            let _ = endpoint;
            Err(Error::Unsupported)
        }
    }
    fn read_out(&mut self, endpoint: u8, data: &mut [u8]) -> Result<usize, Error> {
        #[cfg(target_arch = "arm")]
        {
            self.inner.read_out(endpoint, data)
        }
        #[cfg(not(target_arch = "arm"))]
        {
            let _ = (endpoint, data);
            Err(Error::Unsupported)
        }
    }
    fn set_stall(&mut self, endpoint: u8, stalled: bool) -> Result<(), Error> {
        #[cfg(target_arch = "arm")]
        {
            self.inner.set_stall(endpoint, stalled)
        }
        #[cfg(not(target_arch = "arm"))]
        {
            let _ = (endpoint, stalled);
            Err(Error::Unsupported)
        }
    }
    fn stalled(&self, endpoint: u8) -> bool {
        #[cfg(target_arch = "arm")]
        {
            self.inner.stalled(endpoint)
        }
        #[cfg(not(target_arch = "arm"))]
        {
            let _ = endpoint;
            false
        }
    }
}

/// Fixed allocation: EP0 shares the mandated buffer, each other direction owns
/// 64 bytes. 16 endpoints fit in 4 KiB, with no runtime allocator.
#[cfg(any(target_arch = "arm", test))]
fn old_revision(revision: u32) -> bool {
    revision < 2
}

#[cfg(any(target_arch = "arm", test))]
fn buffer_offset(endpoint: u8) -> Result<u32, Error> {
    if endpoint & 0x70 != 0 {
        return Err(Error::InvalidEndpoint);
    }
    let number = u32::from(endpoint & 15);
    Ok(if number == 0 {
        0x100
    } else {
        0x180 + (2 * (number - 1) + u32::from(endpoint & 0x80 == 0)) * 64
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn b0_and_b1_need_old_silicon_workarounds() {
        assert!(old_revision(0));
        assert!(old_revision(1));
        assert!(!old_revision(2));
        assert!(!old_revision(3));
    }
    #[test]
    fn buffers_are_disjoint_and_within_dpram() {
        assert_eq!(buffer_offset(0), Ok(0x100));
        assert_eq!(buffer_offset(0x80), Ok(0x100));
        let mut previous = 0x140;
        for ep in 1..16 {
            for addr in [ep | 0x80, ep] {
                let offset = buffer_offset(addr).unwrap();
                assert_eq!(offset, previous + 64);
                assert!(offset + 64 <= 4096);
                previous = offset;
            }
        }
        assert_eq!(buffer_offset(0x90), Err(Error::InvalidEndpoint));
    }
}
