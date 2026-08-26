// SPDX-License-Identifier: Apache-2.0
use nusb::{
    transfer::{ControlIn, ControlOut, ControlType, Recipient},
    MaybeFuture,
};
use std::{error::Error, time::Duration};
type Result<T> = std::result::Result<T, Box<dyn Error>>;
const TIMEOUT: Duration = Duration::from_secs(2);

fn main() {
    if let Err(error) = run() {
        eprintln!("usb-control: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() != 3 || !matches!(args[1].as_str(), "check" | "bootsel" | "reboot") {
        return Err("usage: usb-control check|bootsel|reboot EXACT_USB_LOCATION_PATH".into());
    }
    let devices: Vec<_> = nusb::list_devices()
        .wait()?
        .filter(|d| d.vendor_id() == 0x1209 && d.product_id() == 1 && matches_location(d, &args[2]))
        .collect();
    if devices.len() != 1 {
        return Err(format!(
            "expected one private-test device at that topology; found {}",
            devices.len()
        )
        .into());
    }
    let device = devices[0].open().wait()?;
    let interface = device.claim_interface(2).wait()?;
    if args[1] == "check" {
        let get = |value, length| {
            interface
                .control_in(
                    ControlIn {
                        control_type: ControlType::Standard,
                        recipient: Recipient::Device,
                        request: 6,
                        value,
                        index: 0,
                        length,
                    },
                    TIMEOUT,
                )
                .wait()
        };
        let descriptor = get(0x100, 18)?;
        if descriptor.len() != 18 || descriptor[7] != 64 || descriptor[8..12] != [9, 18, 1, 0] {
            return Err("device descriptor mismatch".into());
        }
        let config = get(0x200, 255)?;
        if config.len() != 84 || config[4] != 3 {
            return Err("configuration descriptor mismatch".into());
        }
        let bos = get(0xf00, 255)?;
        if bos.len() != 33 {
            return Err("BOS descriptor mismatch".into());
        }
        let microsoft = interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: 1,
                    value: 0,
                    index: 7,
                    length: 255,
                },
                TIMEOUT,
            )
            .wait()?;
        if microsoft.len() != 166 || &microsoft[22..28] != b"WINUSB" {
            return Err("Windows binding descriptor mismatch".into());
        }
        // Unsupported descriptor must STALL, not hang/disconnect; the next
        // valid SETUP must clear the stall and return exactly the same bytes.
        if !matches!(get(0xff00, 8), Err(nusb::transfer::TransferError::Stall)) {
            return Err("unsupported descriptor did not return STALL".into());
        }
        if get(0x100, 18)? != descriptor {
            return Err("control endpoint failed to recover after STALL".into());
        }
        println!("USB CONTROL PASS descriptors=4 stall_recovery=1");
    } else {
        interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Interface,
                    request: if args[1] == "bootsel" { 1 } else { 2 },
                    value: 0,
                    index: 2,
                    data: &[],
                },
                TIMEOUT,
            )
            .wait()?;
        println!("USB RESET REQUEST ACKNOWLEDGED"); // Host must still prove disconnect/reconnect.
    }
    Ok(())
}
fn matches_location(device: &nusb::DeviceInfo, expected: &str) -> bool {
    #[cfg(windows)]
    {
        device
            .location_paths()
            .iter()
            .any(|p| p.to_string_lossy().eq_ignore_ascii_case(expected))
    }
    #[cfg(not(windows))]
    {
        let _ = (device, expected);
        false
    }
}
