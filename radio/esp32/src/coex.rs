// SPDX-License-Identifier: Apache-2.0

//! The nineteen `_coex_*` entries: Wi-Fi/Bluetooth coexistence.
//!
//! # What coexistence is, and why these are all zero
//!
//! The ESP32 has **one radio**. Wi-Fi and Bluetooth share the antenna, the RF
//! front end and the PHY's common clocks, so when both are up something has to
//! decide which one transmits in any given microsecond. That arbiter is
//! `libcoexist.a`, and these nineteen entries are how the Wi-Fi driver asks it
//! for a slot, tells it a channel changed, and reads back the schedule.
//!
//! FlintOS runs one radio today — BLE is #66 and nothing brings up the
//! controller — so there is nothing to arbitrate. Both references answer that
//! case the same way, and neither by leaving the entries null: esp-idf's
//! `esp_adapter.c` and NuttX's `esp32_wifi_adapter.c` each wrap every one in
//! `#if CONFIG_SW_COEXIST_ENABLE` and, in the `#else`, return zero, return
//! NULL, or do nothing.
//!
//! So **these are not stubs**. They are the arm of a conditional that both
//! references compile when coexistence is off, transcribed rather than
//! invented. `0` from `_coex_init` is `ESP_OK` — the driver is being told
//! coexistence started fine, which is true, because there is no second radio
//! for it to fail to arbitrate with.
//!
//! # What changes when BLE lands
//!
//! Every function here becomes a call into `libcoexist.a`: `coex_init`,
//! `coex_enable`, `coex_wifi_request` and so on are already in the archive
//! this crate links. That is the whole change — the signatures below are the
//! archive's, so the bodies get a call and the module doc loses this section.
//!
//! Until then, the danger worth naming: **if the Bluetooth controller is ever
//! started without these being wired up, the two radios will transmit over
//! each other.** The symptom is packet loss on both, under load, with nothing
//! in either driver's logs. `radio-bt` is the feature that would do it, and
//! `kernel::radio` is where that gate lives.

use core::ffi::c_void;

/// `_coex_init`. `ESP_OK`: there is one radio, and it is arbitrated.
pub(crate) unsafe extern "C" fn init() -> i32 {
    0
}

/// `_coex_deinit`.
pub(crate) unsafe extern "C" fn deinit() {}

/// `_coex_enable`. `ESP_OK`, as [`init`].
pub(crate) unsafe extern "C" fn enable() -> i32 {
    0
}

/// `_coex_disable`.
pub(crate) unsafe extern "C" fn disable() {}

/// `_coex_status_get`. A bitmap of what the arbiter currently has granted;
/// empty, because it has granted nothing and refused nothing.
pub(crate) unsafe extern "C" fn status_get() -> u32 {
    0
}

/// `_coex_condition_set`.
pub(crate) unsafe extern "C" fn condition_set(_type: u32, _dissatisfy: bool) {}

/// `_coex_wifi_request(event, latency, duration)`. Wi-Fi asking for the
/// antenna. Granted — nothing else wants it.
pub(crate) unsafe extern "C" fn wifi_request(_event: u32, _latency: u32, _duration: u32) -> i32 {
    0
}

/// `_coex_wifi_release`.
pub(crate) unsafe extern "C" fn wifi_release(_event: u32) -> i32 {
    0
}

/// `_coex_wifi_channel_set`. The arbiter tracks the Wi-Fi channel so it can
/// tell Bluetooth which frequencies to avoid; with no Bluetooth there is
/// nothing to tell.
pub(crate) unsafe extern "C" fn wifi_channel_set(_primary: u8, _secondary: u8) -> i32 {
    0
}

/// `_coex_event_duration_get(event, out)`.
///
/// **Returns `ESP_OK` without writing `out`**, which is exactly what both
/// references do in the coexistence-off arm — and is worth flagging, because a
/// caller that trusts the return value reads whatever was in its own variable.
/// The Wi-Fi driver initialises it first; this matches the behaviour it was
/// tested against rather than improving on it.
pub(crate) unsafe extern "C" fn event_duration_get(_event: u32, _duration: *mut c_void) -> i32 {
    0
}

/// `_coex_pti_get`. Packet Traffic Indication — the priority Bluetooth should
/// weigh a Wi-Fi request against.
///
/// Zero **whether or not coexistence is enabled**: esp-idf's wrapper has no
/// `#if` around it at all. The one entry in this file that is not a
/// conditional arm.
pub(crate) unsafe extern "C" fn pti_get(_event: u32, _pti: *mut u8) -> i32 {
    0
}

/// `_coex_schm_status_bit_clear`.
pub(crate) unsafe extern "C" fn schm_status_bit_clear(_type: u32, _status: u32) {}

/// `_coex_schm_status_bit_set`.
pub(crate) unsafe extern "C" fn schm_status_bit_set(_type: u32, _status: u32) {}

/// `_coex_schm_interval_set`.
pub(crate) unsafe extern "C" fn schm_interval_set(_interval: u32) -> i32 {
    0
}

/// `_coex_schm_interval_get`. No schedule, so no interval.
pub(crate) unsafe extern "C" fn schm_interval_get() -> u32 {
    0
}

/// `_coex_schm_curr_period_get`.
pub(crate) unsafe extern "C" fn schm_curr_period_get() -> u8 {
    0
}

/// `_coex_schm_curr_phase_get`. **Null, not zero-the-integer** — this one
/// returns a pointer to the arbiter's current phase descriptor, and there is
/// no schedule to point into.
pub(crate) unsafe extern "C" fn schm_curr_phase_get() -> *mut c_void {
    core::ptr::null_mut()
}

/// `_coex_schm_curr_phase_idx_set`.
pub(crate) unsafe extern "C" fn schm_curr_phase_idx_set(_idx: i32) -> i32 {
    0
}

/// `_coex_schm_curr_phase_idx_get`.
pub(crate) unsafe extern "C" fn schm_curr_phase_idx_get() -> i32 {
    0
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_answers_esp_ok_or_nothing() {
        // Not a tautology test: the thing it pins is that `_coex_init` and
        // `_coex_enable` report *success*. Returning an error code would be
        // the plausible-looking mistake -- "coexistence is not implemented, so
        // say so" -- and the Wi-Fi driver aborts init on a non-zero here.
        unsafe {
            assert_eq!(init(), 0);
            assert_eq!(enable(), 0);
            assert_eq!(wifi_request(0, 0, 0), 0);
            assert_eq!(wifi_release(0), 0);
            assert_eq!(wifi_channel_set(1, 0), 0);
            assert_eq!(schm_interval_set(0), 0);
            assert_eq!(schm_curr_phase_idx_set(0), 0);
            assert_eq!(pti_get(0, core::ptr::null_mut()), 0);
        }
    }

    #[test]
    fn the_phase_pointer_is_null_rather_than_a_dangling_zero() {
        // Distinguished from the integer zeros above because the caller
        // dereferences this one if it is not null.
        assert!(unsafe { schm_curr_phase_get() }.is_null());
    }
}
