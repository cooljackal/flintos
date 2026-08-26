// SPDX-License-Identifier: Apache-2.0
//! Board-owned USB construction. Apps see a serial service, not a controller.
use hal::usb::{Error, ResetTarget};
use usb_device::{Identity, Serial};

#[cfg(all(feature = "rp2040-drivers", feature = "native-usb"))]
static USB: api::Once<usb_device::UsbSerial<rp2040_usb::Rp2040Usb>> = api::Once::new();

/// Open native USB once from a boot-core task. Keep calling `service` every ms
/// even with the IRQ installed; it completes chip workarounds and queues data.
/// Identity/reset permissions belong to the application, wiring to the board.
pub fn usb_init(identity: Identity) -> Result<&'static dyn Serial, Error> {
    #[cfg(all(feature = "rp2040-drivers", feature = "native-usb"))]
    {
        if USB.get().is_some() {
            return Err(Error::Busy);
        }
        let serial = USB.get_or_try_init(|| {
            Ok(usb_device::UsbSerial::new(
                rp2040_usb::Rp2040Usb::open()?,
                identity,
            ))
        })?;
        // The service's critical section serializes task/ISR and both cores.
        // Errors remain observable to the mandatory periodic task service.
        unsafe {
            api::interrupt::connect(soc_rp2040::IRQ_USBCTRL, usb_irq)
                .map_err(|_| Error::Hardware)?;
        }
        Ok(serial)
    }
    #[cfg(not(all(feature = "rp2040-drivers", feature = "native-usb")))]
    {
        let _ = identity;
        Err(Error::Unsupported)
    }
}

#[cfg(all(feature = "rp2040-drivers", feature = "native-usb"))]
fn usb_irq() {
    if let Some(serial) = USB.get() {
        let _ = serial.service();
    }
}

/// Commit a requested USB reboot. No live dual-core call into the boot ROM.
/// # Safety
/// Terminates both cores. Call in task context only after finishing transfers
/// and any persistent writes. Not implemented on boards without native USB.
pub unsafe fn usb_reset(target: ResetTarget) -> ! {
    #[cfg(all(target_arch = "arm", feature = "rp2040-drivers"))]
    {
        // Hold the kernel's global critical section to exclude watchdog feeds
        // on both cores until the one-millisecond hardware timeout fires.
        static RESET: api::CsCell<()> = api::CsCell::new(());
        RESET.with(|_| {
            unsafe {
                soc_rp2040::watchdog::arm(1, false);
                if target == ResetTarget::Application {
                    soc_rp2040::watchdog::clear_flint_watchdog_marker();
                }
            }
            loop {
                core::hint::spin_loop();
            }
        })
    }
    #[cfg(not(all(target_arch = "arm", feature = "rp2040-drivers")))]
    {
        let _ = target;
        panic!("board has no USB reset implementation")
    }
}
