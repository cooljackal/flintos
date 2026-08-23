// SPDX-License-Identifier: Apache-2.0

//! TWAI (CAN) self-test. Included by [`crate::selftest`].
//!
//! The controller transmits a frame and receives its own copy, with no bus and
//! no second node — self-test mode plus a self-reception request, with TX and
//! RX routed to the same pad so the driven bits reach the receiver. A frame that
//! comes back byte-for-byte exercises bit timing, framing, the CRC, and both
//! buffers on one pin.
//!
//! Needs a GPIO nothing else drives — see `board::active::LOOPBACK_SCRATCH_GPIO`.
//! A board that declares none skips this.

use super::Check;

/// Send a known frame as a self-reception and require it back unchanged.
#[cfg(target_os = "none")]
pub(crate) fn twai_self_reception_round_trips(pin: u8) -> Check {
    use esp32_twai::{Frame, Mode, Twai};

    let twai = unsafe { Twai::new(pin, pin, Mode::SelfTest) }
        .map_err(|_| "the TWAI loopback pin would not route")?;

    let sent = Frame { id: 0x2AB, len: 4, data: [0xDE, 0xAD, 0xBE, 0xEF, 0, 0, 0, 0] };
    let got = twai.self_reception(&sent)
        .map_err(|_| "no frame came back — the self-reception never completed")?;

    {
        use crate::debug::fault::{raw_dec, raw_print};
        raw_print("[FLINT]   twai id=");
        raw_dec(got.id as u32);
        raw_print(" len=");
        raw_dec(got.len as u32);
        raw_print("\r\n");
    }

    if got != sent {
        return Err("the self-received frame did not match what was sent");
    }
    Ok(())
}

// Host stand-in: there is no TWAI controller to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn twai_self_reception_round_trips(_pin: u8) -> Check {
    Ok(())
}
