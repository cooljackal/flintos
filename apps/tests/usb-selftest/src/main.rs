// SPDX-License-Identifier: Apache-2.0
//! Binary framing: 16-byte command, then an exact byte echo payload.
//! HELLO: "F172" + command 0 + 3 reserved zero bytes + 8-byte host nonce.
//! Reply: "F172" + 8 ASCII hex image-ID bytes + the nonce (20 bytes).
//! ECHO command 1: bytes 8..12 are a little-endian length (maximum 1 MiB).
//! Command 2 masks interrupts to prove watchdog recovery; command 3 stalls
//! only this task, with tick live, to prove bounded host/SWD recovery.
//! Fault commands first return the HELLO-format nonce acknowledgment, and wait
//! for its final USB packet ACK before injecting the fault.
#![no_std]
#![no_main]
use api::task;
use api::Priority;
kernel::flint_app!(main, abi = 2);

fn main() {
    task::spawn_on(0, "usb-test", run, Priority::Normal(1), 8192).expect("USB task");
}

fn run() {
    // pid.codes reserves this pair for PRIVATE testing only. Not unique;
    // never redistribute/manufacture devices with it. Host must bind topology.
    let usb = board::usb_init(board::UsbIdentity {
        vid: 0x1209,
        pid: 1,
        manufacturer: "FlintOS",
        product: "FlintOS USB test",
        serial: None,
        allow_reset: true,
    })
    .expect("USB controller");
    api::log_info!("USB READY image={}", env!("FLINT_USB_IMAGE_ID"));
    let mut command = [0; 16];
    let mut command_len = 0;
    let mut remaining = 0usize;
    let mut output = [0; 256];
    let mut out_len = 0;
    let mut out_pos = 0;
    let mut pending_fault = 0;
    loop {
        usb.service().expect("USB service");
        if let Some(target) = usb.take_reset() {
            unsafe { board::usb_reset(target) };
        }
        if pending_fault != 0 && out_pos == out_len && usb.status().transmit_idle {
            if pending_fault == 2 {
                api::log_info!("USB FAULT interrupts-masked");
                #[cfg(target_arch = "arm")]
                unsafe {
                    kernel::watchdog::arm();
                    core::arch::asm!("cpsid i", options(nostack));
                }
                loop {
                    core::hint::spin_loop();
                }
            } else {
                api::log_info!("USB FAULT task-stalled");
                loop {
                    task::sleep_ms(100);
                }
            }
        }
        if !usb.status().connected {
            command_len = 0;
            remaining = 0;
            out_len = 0;
            out_pos = 0;
        } else if out_pos < out_len {
            out_pos += usb.write(&output[out_pos..out_len]);
        } else if remaining != 0 {
            let cap = remaining.min(output.len());
            out_len = usb.read(&mut output[..cap]);
            out_pos = 0;
            remaining -= out_len;
        } else {
            command_len += usb.read(&mut command[command_len..]);
            if command_len == command.len() {
                if &command[..4] != b"F172" {
                    panic!("bad USB command");
                }
                match command[4] {
                    0 | 2 | 3 => {
                        pending_fault = command[4];
                        output[..4].copy_from_slice(b"F172");
                        output[4..12].copy_from_slice(env!("FLINT_USB_IMAGE_ID").as_bytes());
                        output[12..20].copy_from_slice(&command[8..16]);
                        out_len = 20;
                        out_pos = 0;
                    }
                    1 => {
                        remaining = u32::from_le_bytes(command[8..12].try_into().unwrap()) as usize;
                        assert!(remaining <= 1024 * 1024);
                    }
                    _ => panic!("unknown USB command"),
                }
                command_len = 0;
            }
        }
        task::sleep_ms(1);
    }
}
