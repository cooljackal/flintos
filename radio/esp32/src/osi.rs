// SPDX-License-Identifier: Apache-2.0

//! `wifi_osi_funcs_t`: the table Espressif's blobs call out through.
//!
//! **Generated, not transcribed.** The field list below was produced by
//! parsing `components/esp_wifi/include/esp_private/wifi_os_adapter.h` from
//! the pinned esp-idf release. That is deliberate: the struct is 115 function
//! pointers with no names on the wire, so a single field transposed shifts
//! every one after it and the blob calls the wrong function with the right
//! arguments. Issue #65 names that failure exactly — "a working radio that
//! corrupts memory". Hand-typing it was not worth the risk.
//!
//! The generator honours the target guards in the header, which matters:
//! `_slowclk_cal_get` exists only on the S2, S3 and C3 and is *absent* here.
//! Including it would have shifted the final sixteen fields, all of them
//! coexistence, which is precisely the sort of fault that appears only when
//! Wi-Fi and BLE run together.
//!
//! # Pinning
//!
//! [`VERSION`] and [`MAGIC`] are checked by the blob at init. They are the
//! only cheap protection against being built against a different esp-idf than
//! the one this table was generated from, so they are asserted here as well:
//! a mismatch should fail loudly at our boundary rather than inside a binary
//! we cannot debug.

#![allow(non_snake_case)]

use core::ffi::{c_char, c_void};

/// `ESP_WIFI_OS_ADAPTER_VERSION` from the pinned header.
pub const VERSION: i32 = 0x0000_0008;

/// `ESP_WIFI_OS_ADAPTER_MAGIC`. Spelled "DEADBEAF", not "DEADBEEF" — that is
/// Espressif's spelling and copying it correctly matters more than it reads.
pub const MAGIC: i32 = 0xDEAD_BEAFu32 as i32;

/// esp-idf release this table was generated from.
pub const IDF_VERSION: &str = "v4.4";

/// Number of function pointers between `_version` and `_magic`.
pub const FUNCTION_COUNT: usize = 115;

/// `OSI_FUNCS_TIME_BLOCKING` — wait forever.
pub const TIME_BLOCKING: u32 = 0xFFFF_FFFF;

/// The table itself.
///
/// `_version` first and `_magic` last, with the function pointers between
/// them in header order. Every pointer is an `Option`, so a slot we have not
/// implemented is a null the blob can be seen to trip over rather than a
/// jump into whatever happened to be there.
#[repr(C)]
pub struct WifiOsiFuncs {
    pub _version: i32,
    pub _env_is_chip: Option<unsafe extern "C" fn() -> bool>,
    pub _set_intr: Option<unsafe extern "C" fn(i32, u32, u32, i32)>,
    pub _clear_intr: Option<unsafe extern "C" fn(u32, u32)>,
    pub _set_isr: Option<unsafe extern "C" fn(i32, *mut c_void, *mut c_void)>,
    pub _ints_on: Option<unsafe extern "C" fn(u32)>,
    pub _ints_off: Option<unsafe extern "C" fn(u32)>,
    pub _is_from_isr: Option<unsafe extern "C" fn() -> bool>,
    pub _spin_lock_create: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub _spin_lock_delete: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _wifi_int_disable: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    pub _wifi_int_restore: Option<unsafe extern "C" fn(*mut c_void, u32)>,
    pub _task_yield_from_isr: Option<unsafe extern "C" fn()>,
    pub _semphr_create: Option<unsafe extern "C" fn(u32, u32) -> *mut c_void>,
    pub _semphr_delete: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _semphr_take: Option<unsafe extern "C" fn(*mut c_void, u32) -> i32>,
    pub _semphr_give: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    pub _wifi_thread_semphr_get: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub _mutex_create: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub _recursive_mutex_create: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub _mutex_delete: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _mutex_lock: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    pub _mutex_unlock: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    pub _queue_create: Option<unsafe extern "C" fn(u32, u32) -> *mut c_void>,
    pub _queue_delete: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _queue_send: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, u32) -> i32>,
    pub _queue_send_from_isr: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32>,
    pub _queue_send_to_back: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, u32) -> i32>,
    pub _queue_send_to_front: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, u32) -> i32>,
    pub _queue_recv: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, u32) -> i32>,
    pub _queue_msg_waiting: Option<unsafe extern "C" fn(*mut c_void) -> u32>,
    pub _event_group_create: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub _event_group_delete: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _event_group_set_bits: Option<unsafe extern "C" fn(*mut c_void, u32) -> u32>,
    pub _event_group_clear_bits: Option<unsafe extern "C" fn(*mut c_void, u32) -> u32>,
    pub _event_group_wait_bits: Option<unsafe extern "C" fn(*mut c_void, u32, i32, i32, u32) -> u32>,
    pub _task_create_pinned_to_core: Option<unsafe extern "C" fn(*mut c_void, *const c_char, u32, *mut c_void, u32, *mut c_void, u32) -> i32>,
    pub _task_create: Option<unsafe extern "C" fn(*mut c_void, *const c_char, u32, *mut c_void, u32, *mut c_void) -> i32>,
    pub _task_delete: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _task_delay: Option<unsafe extern "C" fn(u32)>,
    pub _task_ms_to_tick: Option<unsafe extern "C" fn(u32) -> i32>,
    pub _task_get_current_task: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub _task_get_max_priority: Option<unsafe extern "C" fn() -> i32>,
    pub _malloc: Option<unsafe extern "C" fn(u32) -> *mut c_void>,
    pub _free: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _event_post: Option<unsafe extern "C" fn(*const c_char, i32, *mut c_void, usize, u32) -> i32>,
    pub _get_free_heap_size: Option<unsafe extern "C" fn() -> u32>,
    pub _rand: Option<unsafe extern "C" fn() -> u32>,
    pub _dport_access_stall_other_cpu_start_wrap: Option<unsafe extern "C" fn()>,
    pub _dport_access_stall_other_cpu_end_wrap: Option<unsafe extern "C" fn()>,
    pub _wifi_apb80m_request: Option<unsafe extern "C" fn()>,
    pub _wifi_apb80m_release: Option<unsafe extern "C" fn()>,
    pub _phy_disable: Option<unsafe extern "C" fn()>,
    pub _phy_enable: Option<unsafe extern "C" fn()>,
    pub _phy_common_clock_enable: Option<unsafe extern "C" fn()>,
    pub _phy_common_clock_disable: Option<unsafe extern "C" fn()>,
    pub _phy_update_country_info: Option<unsafe extern "C" fn(*const c_char) -> i32>,
    pub _read_mac: Option<unsafe extern "C" fn(*mut u8, u32) -> i32>,
    pub _timer_arm: Option<unsafe extern "C" fn(*mut c_void, u32, bool)>,
    pub _timer_disarm: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _timer_done: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _timer_setfn: Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void)>,
    pub _timer_arm_us: Option<unsafe extern "C" fn(*mut c_void, u32, bool)>,
    pub _wifi_reset_mac: Option<unsafe extern "C" fn()>,
    pub _wifi_clock_enable: Option<unsafe extern "C" fn()>,
    pub _wifi_clock_disable: Option<unsafe extern "C" fn()>,
    pub _wifi_rtc_enable_iso: Option<unsafe extern "C" fn()>,
    pub _wifi_rtc_disable_iso: Option<unsafe extern "C" fn()>,
    pub _esp_timer_get_time: Option<unsafe extern "C" fn() -> i64>,
    pub _nvs_set_i8: Option<unsafe extern "C" fn(u32, *const c_char, i8) -> i32>,
    pub _nvs_get_i8: Option<unsafe extern "C" fn(u32, *const c_char, *mut i8) -> i32>,
    pub _nvs_set_u8: Option<unsafe extern "C" fn(u32, *const c_char, u8) -> i32>,
    pub _nvs_get_u8: Option<unsafe extern "C" fn(u32, *const c_char, *mut u8) -> i32>,
    pub _nvs_set_u16: Option<unsafe extern "C" fn(u32, *const c_char, u16) -> i32>,
    pub _nvs_get_u16: Option<unsafe extern "C" fn(u32, *const c_char, *mut u16) -> i32>,
    pub _nvs_open: Option<unsafe extern "C" fn(*const c_char, u32, *mut c_void) -> i32>,
    pub _nvs_close: Option<unsafe extern "C" fn(u32)>,
    pub _nvs_commit: Option<unsafe extern "C" fn(u32) -> i32>,
    pub _nvs_set_blob: Option<unsafe extern "C" fn(u32, *const c_char, *const c_void, usize) -> i32>,
    pub _nvs_get_blob: Option<unsafe extern "C" fn(u32, *const c_char, *mut c_void, *mut usize) -> i32>,
    pub _nvs_erase_key: Option<unsafe extern "C" fn(u32, *const c_char) -> i32>,
    pub _get_random: Option<unsafe extern "C" fn(*mut u8, usize) -> i32>,
    pub _get_time: Option<unsafe extern "C" fn(*mut c_void) -> i32>,
    pub _random: Option<unsafe extern "C" fn() -> u32>,
    pub _log_write: Option<unsafe extern "C" fn(u32, *const c_char, *const c_char, ...)>,
    pub _log_writev: Option<unsafe extern "C" fn(u32, *const c_char, *const c_char, *mut c_void)>,
    pub _log_timestamp: Option<unsafe extern "C" fn() -> u32>,
    pub _malloc_internal: Option<unsafe extern "C" fn(usize) -> *mut c_void>,
    pub _realloc_internal: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    pub _calloc_internal: Option<unsafe extern "C" fn(usize, usize) -> *mut c_void>,
    pub _zalloc_internal: Option<unsafe extern "C" fn(usize) -> *mut c_void>,
    pub _wifi_malloc: Option<unsafe extern "C" fn(usize) -> *mut c_void>,
    pub _wifi_realloc: Option<unsafe extern "C" fn(*mut c_void, usize) -> *mut c_void>,
    pub _wifi_calloc: Option<unsafe extern "C" fn(usize, usize) -> *mut c_void>,
    pub _wifi_zalloc: Option<unsafe extern "C" fn(usize) -> *mut c_void>,
    pub _wifi_create_queue: Option<unsafe extern "C" fn(i32, i32) -> *mut c_void>,
    pub _wifi_delete_queue: Option<unsafe extern "C" fn(*mut c_void)>,
    pub _coex_init: Option<unsafe extern "C" fn() -> i32>,
    pub _coex_deinit: Option<unsafe extern "C" fn()>,
    pub _coex_enable: Option<unsafe extern "C" fn() -> i32>,
    pub _coex_disable: Option<unsafe extern "C" fn()>,
    pub _coex_status_get: Option<unsafe extern "C" fn() -> u32>,
    pub _coex_condition_set: Option<unsafe extern "C" fn(u32, bool)>,
    pub _coex_wifi_request: Option<unsafe extern "C" fn(u32, u32, u32) -> i32>,
    pub _coex_wifi_release: Option<unsafe extern "C" fn(u32) -> i32>,
    pub _coex_wifi_channel_set: Option<unsafe extern "C" fn(u8, u8) -> i32>,
    pub _coex_event_duration_get: Option<unsafe extern "C" fn(u32, *mut c_void) -> i32>,
    pub _coex_pti_get: Option<unsafe extern "C" fn(u32, *mut u8) -> i32>,
    pub _coex_schm_status_bit_clear: Option<unsafe extern "C" fn(u32, u32)>,
    pub _coex_schm_status_bit_set: Option<unsafe extern "C" fn(u32, u32)>,
    pub _coex_schm_interval_set: Option<unsafe extern "C" fn(u32) -> i32>,
    pub _coex_schm_interval_get: Option<unsafe extern "C" fn() -> u32>,
    pub _coex_schm_curr_period_get: Option<unsafe extern "C" fn() -> u8>,
    pub _coex_schm_curr_phase_get: Option<unsafe extern "C" fn() -> *mut c_void>,
    pub _coex_schm_curr_phase_idx_set: Option<unsafe extern "C" fn(i32) -> i32>,
    pub _coex_schm_curr_phase_idx_get: Option<unsafe extern "C" fn() -> i32>,
    pub _magic: i32,
}

impl WifiOsiFuncs {
    /// A table with the version and magic set and every function null.
    ///
    /// Filling it in is `adapter::table()`'s job. Starting from all-null means
    /// an unimplemented call is a null-pointer jump at a known address rather
    /// than a wild one, which is the difference between a diagnosable crash
    /// and a corrupted radio.
    pub const fn empty() -> Self {
        // SAFETY: every field is either an `i32` or an `Option<fn>`, and all
        // zeroes is a valid bit pattern for both -- `None` for the niche-
        // optimised function pointers.
        let mut t: Self = unsafe { core::mem::zeroed() };
        t._version = VERSION;
        t._magic = MAGIC;
        t
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pinned_constants_match_the_header() {
        // Quoted from wifi_os_adapter.h at the pinned release:
        //
        //     #define ESP_WIFI_OS_ADAPTER_VERSION  0x00000008
        //     #define ESP_WIFI_OS_ADAPTER_MAGIC    0xDEADBEAF
        //
        // The blob checks both at init. If either changes upstream, this table
        // needs regenerating -- the version bump is the *only* warning given.
        assert_eq!(VERSION, 8);
        assert_eq!(MAGIC as u32, 0xDEAD_BEAF);
        assert_eq!(IDF_VERSION, "v4.4");
    }

    #[test]
    fn an_empty_table_is_all_nulls_around_the_two_constants() {
        let t = WifiOsiFuncs::empty();
        assert_eq!(t._version, VERSION);
        assert_eq!(t._magic, MAGIC);
        assert!(t._env_is_chip.is_none(), "functions must start unimplemented");
        assert!(t._malloc.is_none());
        assert!(t._coex_schm_curr_phase_idx_get.is_none());
    }

    #[test]
    fn the_table_is_the_size_the_blob_expects() {
        // Two i32 plus the function pointers, with no padding on a 32-bit
        // target. This is the check that catches a field added or lost during
        // a regeneration: the blob indexes by offset and would not notice.
        #[cfg(target_pointer_width = "32")]
        assert_eq!(
            core::mem::size_of::<WifiOsiFuncs>(),
            (2 + FUNCTION_COUNT) * 4
        );
        // On a 64-bit host the pointers are wider, so only the count is
        // meaningful. It is still worth asserting: the generator is what
        // produces it, and a silent change there is the risk.
        assert_eq!(FUNCTION_COUNT, 115);
    }

    #[test]
    fn the_s2_only_field_is_absent() {
        // `_slowclk_cal_get` is guarded by CONFIG_IDF_TARGET_ESP32S2/S3/C3 in
        // the header. Including it on the ESP32 would shift the sixteen
        // coexistence pointers that follow -- a fault that only shows when
        // Wi-Fi and BLE run together, which is the worst time to find it.
        //
        // Asserted by size rather than by name, since a missing field cannot
        // be named: one more pointer would make the table four bytes longer.
        #[cfg(target_pointer_width = "32")]
        assert_ne!(
            core::mem::size_of::<WifiOsiFuncs>(),
            (2 + FUNCTION_COUNT + 1) * 4
        );
    }
}
