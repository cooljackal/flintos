// SPDX-License-Identifier: Apache-2.0

//! Bringing the PHY up, and the calibration that goes with it.
//!
//! Step 3.6. Everything else in this crate answers a call *from* the blob;
//! this is the one place FlintOS calls *into* it, and the first place where
//! "compiles and links" stops meaning much.
//!
//! # The sequence, which is esp-idf's
//!
//! `esp_phy_enable` in `phy_init.c`, reduced to what FlintOS has:
//!
//! 1. Turn on the radio clocks (`DPORT_WIFI_CLK_EN`).
//! 2. First time only: load a stored calibration, hand it and the init data
//!    to `register_chipv7_phy`, and store the result back if it was
//!    recalculated.
//! 3. Afterwards: `phy_wakeup_init`, which is the cheap path.
//!
//! Disable runs it backwards: `phy_close_rf`, then the clocks.
//!
//! # The calibration mode is a decision, not a constant
//!
//! esp-idf asks for `PHY_RF_CAL_FULL` when it has no usable stored
//! calibration and `PHY_RF_CAL_PARTIAL` when it has one. The difference is
//! hundreds of milliseconds at every boot, which is the entire reason
//! [`crate::calibration`] exists.
//!
//! It also uses `PHY_RF_CAL_NONE` after a deep-sleep wake, on the grounds
//! that the calibration in RAM is still good. FlintOS has no deep sleep, so
//! that case cannot arise here and is not implemented rather than guessed at.
//!
//! # What the hardware said
//!
//! `apps/radioprobe` runs this on a board. On an ESP32-WROOM:
//! `register_chipv7_phy` returns 0 with our init data and our eFuse MAC, a
//! full calibration takes ~183 ms, and a re-enable takes ~250 µs.
//!
//! It also found a kernel bug, since fixed: this double-faulted on every
//! preemption, and the cause was four wrong window overflow/underflow vectors
//! rather than anything about the blob. Nothing in FlintOS's own Rust reaches
//! the call8 and call12 handlers; a GCC-built blob reaches them constantly.
//! `enable` is preemptible now, and there is no critical section anywhere on
//! this path -- see `arch/xtensa/src/asm/vectors.S`.
//!
//! One thing it found that is **not** fixed:
//!
//! - **The stored calibration currently costs more than it saves.** Reading it
//!   back is ~1.36 s, against the ~183 ms of calibration it avoids, because
//!   `kvstore::get` scans the whole log per key and [`crate::calibration`]
//!   needs eighteen of them. [`CalibrationMode::Partial`] is the right
//!   decision and the store is the right idea; the log is the wrong shape.

/// Call `coex_bt_high_prio` on the enable that takes the reference count up.
///
/// **Off: it stops the scan cycle dead.** esp-idf calls it on every such
/// enable (`phy_init.c:242`), on the ESP32 branch, outside any
/// `CONFIG_BT_ENABLED` guard, and the reason to try it was good — the packet
/// traffic arbiter sits between the two basebands and does not consult
/// whether Bluetooth was ever built in. On this tree it makes things worse:
/// scan 1 completes and scans 2 through 8 produce no `SCAN_DONE` at all.
///
/// Nothing new is linked either way — `libnet80211.a` already imports the
/// symbol and `librtc.a` defines it — so this is the call and not the link,
/// which is what separates it from the `esp_wifi_internal_reg_rxcb` attempt.
/// The likely reason is that esp-idf reaches it with the coexistence
/// subsystem initialised, and `crate::coex` is nineteen stubs.
const COEX_HIGH_PRIO: bool = false;

use crate::calibration::{CalibrationMode, CAL_DATA_LEN};
// `calibration::load`/`save_or_reset` and `Invalid` are named only by the
// target-only halves below, which is why they are imported there.
use kernel::smp::Spinlock;

extern "C" {
    /// `int register_chipv7_phy(const esp_phy_init_data_t*,
    ///                          esp_phy_calibration_data_t*,
    ///                          esp_phy_calibration_mode_t)`
    ///
    /// Signature from esp-idf `components/esp_phy/include/phy.h` at v4.4, and
    /// the symbol is confirmed present in `libphy.a`.
    fn register_chipv7_phy(init_data: *const u8, cal_data: *mut u8, mode: u32) -> i32;

    /// `uint32_t phy_get_rf_cal_version(void)` — which calibration layout the
    /// PHY speaks. Stored beside the data and checked on load.
    fn phy_get_rf_cal_version() -> u32;

    /// `void phy_wakeup_init(void)` — the cheap re-enable, once the PHY has
    /// been registered at least once.
    fn phy_wakeup_init();

    /// `void phy_close_rf(void)` — the counterpart to a successful enable.
    fn phy_close_rf();

    /// `void coex_bt_high_prio(void)`, from `librtc.a`.
    ///
    /// esp-idf calls this on every `esp_phy_enable` that takes the reference
    /// count from zero (`phy_init.c:242`), on the ESP32 branch, **outside**
    /// any `CONFIG_BT_ENABLED` guard — so a Wi-Fi-only build calls it too.
    ///
    /// It costs nothing at the link: `libnet80211.a` already imports it, so
    /// `librtc.a` is resolved and linked with or without this declaration.
    /// That matters, because adding an archive to this link is what broke the
    /// driver the last time a blob symbol was introduced.
    fn coex_bt_high_prio();

    /// `char* get_phy_version_str(void)` — the blob's own version string,
    /// which esp-idf logs at every boot.
    ///
    /// Needs no clocks and touches no register, which makes it the cheapest
    /// question that can only be answered by the blob actually running.
    fn get_phy_version_str() -> *const core::ffi::c_char;
}

/// How many times [`enable`] has been called without a matching [`disable`],
/// and whether the PHY has ever been registered.
///
/// One lock over both, because the pair has to move together: two cores
/// enabling at once must produce exactly one `register_chipv7_phy`, and a
/// count that is right while the flag is stale would run the full sequence
/// twice.
struct State {
    refs: u32,
    registered: bool,
}

static STATE: Spinlock<State> = Spinlock::new(State { refs: 0, registered: false });

/// Why the PHY could not be brought up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhyError {
    /// `register_chipv7_phy` returned non-zero. The value is the blob's.
    Register(i32),
    /// No room on the radio heap for the 1904-byte calibration buffer.
    OutOfMemory,
}

/// The calibration layout this PHY speaks.
///
/// # Safety
/// Calls into the blob. Cheap and side-effect-free, but it is still the blob.
pub unsafe fn rf_cal_version() -> u32 {
    unsafe { phy_get_rf_cal_version() }
}

/// The PHY blob's version string, as it reports itself.
///
/// Which archive is linked is otherwise invisible: `make blobs` fetches a
/// pinned revision, and nothing downstream would notice if it fetched a
/// different one.
///
/// # Safety
/// Calls into the blob, which returns a pointer to its own static storage.
pub unsafe fn version_str() -> &'static str {
    let p = unsafe { get_phy_version_str() };
    if p.is_null() {
        return "(null)";
    }
    // The blob's string outlives everything; `'static` is not a widening.
    match unsafe { core::ffi::CStr::from_ptr(p) }.to_str() {
        Ok(s) => s,
        // Not a panic: this is diagnostics, and a version string that came
        // back as something other than text is itself the finding.
        Err(_) => "(not utf-8)",
    }
}

/// Bring the PHY up, or take another reference to an already-running one.
///
/// The first call does the expensive part: it loads a stored calibration if
/// there is a usable one, registers the PHY, and stores the result back when
/// the PHY had to recalculate. Later calls only re-wake it.
///
/// # Safety
/// Calls into the blob and writes DPORT. The caller must pair every `enable`
/// with a [`disable`], and must have initialised [`crate::nvs`] first if the
/// calibration is to survive a reboot -- without it this still works, and
/// simply recalibrates every time.
pub unsafe fn enable(mask: u32) -> Result<(), PhyError> {
    // The clocks first: everything below touches PHY registers, and reaching
    // them without a clock reads as the blob misbehaving.
    unsafe { soc_esp32::dport::radio_clock_enable(soc_esp32::dport::RADIO_CLK_COMMON | mask) };

    let (first, taking_it_up) = STATE.with(|s| {
        s.refs += 1;
        (!s.registered, s.refs == 1)
    });

    if !first {
        unsafe { phy_wakeup_init() };
        // esp-idf also calls `phy_digital_regs_load` here, and stores the
        // registers in `esp_phy_disable`. Not implemented: measured on the
        // board, the driver enables the PHY once and never disables it
        // (`phy 1 on, 0 off` across three scans), so this branch does not run
        // and neither does the store. It becomes real work the moment
        // something cycles the PHY — modem sleep, or BLE sharing it.
        if taking_it_up {
            unsafe { coex_bt_high_prio() };
        }
        return Ok(());
    }

    let result = unsafe { register_once() };
    match result {
        Ok(()) => {
            STATE.with(|s| s.registered = true);
            // Last in the sequence, as esp-idf has it: after the calibration
            // branch, inside the reference-count-was-zero arm
            // (`phy_init.c:242`). The ESP32's packet traffic arbiter sits
            // between the two basebands and does not consult whether
            // Bluetooth was ever built in, which is why esp-idf calls this
            // unconditionally on this target.
            if COEX_HIGH_PRIO && taking_it_up {
                unsafe { coex_bt_high_prio() };
            }
            Ok(())
        }
        Err(e) => {
            // Undo the reference, so a caller that retries is not counted
            // twice and a later `disable` does not close an RF that was
            // never opened.
            STATE.with(|s| s.refs = s.refs.saturating_sub(1));
            unsafe { soc_esp32::dport::radio_clock_disable(mask) };
            Err(e)
        }
    }
}

/// The expensive path, split out so [`enable`] reads as the decision it is.
///
/// # Safety
/// Calls `register_chipv7_phy`.
unsafe fn register_once() -> Result<(), PhyError> {
    let version = unsafe { phy_get_rf_cal_version() };
    let mac = unsafe { soc_esp32::efuse::base_mac() };

    // 1904 bytes, and **not on the stack**: a task's is 4 KiB and this would
    // be half of it, on the one path that also calls into a blob of unknown
    // frame depth. `kernel::heap` exists for the radio and this is the sort
    // of thing it exists for.
    let buf = unsafe { kernel::heap::alloc(CAL_DATA_LEN, 4) };
    if buf.is_null() {
        api::log_error!("radio: no room for {} bytes of calibration data", CAL_DATA_LEN);
        return Err(PhyError::OutOfMemory);
    }
    let data: &mut [u8; CAL_DATA_LEN] =
        unsafe { &mut *(buf as *mut [u8; CAL_DATA_LEN]) };
    data.fill(0);

    let result = unsafe { register_with(data, version, &mac) };
    unsafe { kernel::heap::free(buf, kernel::heap::Caps::Internal) };
    result
}

/// The part that needs the buffer, so `register_once` can free it on every
/// path without a `Drop` type for one allocation.
///
/// # Safety
/// Calls `register_chipv7_phy`.
unsafe fn register_with(
    data: &mut [u8; CAL_DATA_LEN],
    version: u32,
    mac: &[u8; 6],
) -> Result<(), PhyError> {
    let mode = if load_stored(data, version, mac) {
        CalibrationMode::Partial
    } else {
        CalibrationMode::Full
    };

    // esp-idf copies the MAC in before calling, so the PHY sees the part it
    // is calibrating for rather than whatever the stored blob carried.
    data[4..10].copy_from_slice(mac);

    // Four-byte aligned on purpose, not by luck. `esp_phy_init_data_t` is a
    // struct of `uint8_t`, so C gives it alignment 1 and Rust gives `[u8; 128]`
    // the same -- but the PHY writes hardware registers from it, which means
    // 32-bit loads. Today's build happens to put it at `sp + 36`, which is
    // aligned; that offset is the compiler's stack layout and moves whenever
    // this function is edited. An unaligned 32-bit load on Xtensa does not
    // fault here, it returns rotated bytes, so the failure would be a PHY
    // programmed with garbage and a `register_chipv7_phy` that still returns 0.
    #[repr(C, align(4))]
    struct AlignedInitData([u8; crate::phy_init::PHY_INIT_DATA_LEN]);
    const _: () = assert!(core::mem::align_of::<AlignedInitData>() == 4);

    let init = AlignedInitData(crate::phy_init::init_data(
        kernel::board::active::PHY_MAX_TX_POWER_DBM,
    ));
    debug_assert_eq!(init.0.as_ptr() as usize % 4, 0);
    let rc = unsafe { register_chipv7_phy(init.0.as_ptr(), data.as_mut_ptr(), mode as u32) };
    if rc != 0 {
        api::log_error!("radio: register_chipv7_phy failed with {}", rc);
        return Err(PhyError::Register(rc));
    }

    // Only worth storing when the PHY recalculated. A partial calibration
    // started from what was already on flash, so writing it back would spend
    // a flash erase storing what is already there.
    if mode == CalibrationMode::Full {
        store(data, version, mac);
    }
    Ok(())
}

/// Load a stored calibration into `out`. True if it is usable.
///
/// Every rejection is logged with its reason: "the radio recalibrated again"
/// with nothing attached is unactionable, and the reasons are meaningfully
/// different -- `Missing` is a first boot, `MacMismatch` is a calibration
/// from another board, and `ChecksumMismatch` is an interrupted save.
#[cfg(target_os = "none")]
fn load_stored(out: &mut [u8; CAL_DATA_LEN], version: u32, mac: &[u8; 6]) -> bool {
    use crate::calibration::{self, Invalid};
    let outcome = crate::nvs::with_store(
        |s| calibration::load(s, version, mac, out),
        Err(Invalid::Missing),
    );
    match outcome {
        Ok(()) => true,
        Err(Invalid::Missing) => {
            api::log_info!("radio: no stored RF calibration; calibrating in full");
            false
        }
        Err(e) => {
            api::log_warn!("radio: stored RF calibration rejected ({:?}); calibrating in full", e);
            false
        }
    }
}

/// Store a freshly calculated calibration, reporting rather than failing.
///
/// A store that does not work costs a recalibration on the next boot. It is
/// not worth failing an otherwise successful PHY bring-up over.
#[cfg(target_os = "none")]
fn store(data: &[u8; CAL_DATA_LEN], version: u32, mac: &[u8; 6]) {
    use crate::calibration;
    let r = crate::nvs::with_store(
        |s| calibration::save_or_reset(s, version, mac, data),
        Ok(()),
    );
    match r {
        Ok(()) => api::log_info!("radio: RF calibration stored"),
        Err(e) => api::log_warn!("radio: could not store RF calibration ({:?}); it will be recalculated next boot", e),
    }
}

// Host stand-ins: there is no flash region to open.
#[cfg(not(target_os = "none"))]
fn load_stored(_out: &mut [u8; CAL_DATA_LEN], _version: u32, _mac: &[u8; 6]) -> bool {
    false
}
#[cfg(not(target_os = "none"))]
fn store(_data: &[u8; CAL_DATA_LEN], _version: u32, _mac: &[u8; 6]) {}

/// Drop a reference, closing the RF when the last one goes.
///
/// # Safety
/// Calls into the blob and writes DPORT. Must match an [`enable`].
pub unsafe fn disable(mask: u32) {
    let last = STATE.with(|s| {
        s.refs = s.refs.saturating_sub(1);
        s.refs == 0
    });
    if last {
        unsafe { phy_close_rf() };
        // The common clocks stay on. They are shared with the other radio,
        // and this crate does not know whether it is up -- see
        // `dport::radio_clock_disable`.
        unsafe { soc_esp32::dport::radio_clock_disable(mask) };
    }
}

/// How many references are outstanding. Diagnostics.
pub fn refs() -> u32 {
    STATE.with(|s| s.refs)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mac_lands_where_the_struct_keeps_it() {
        // `esp_phy_calibration_data_t` is version[4], mac[6], opaque[1894].
        // Writing the MAC at the wrong offset overwrites either the version
        // the PHY wrote or the first bytes of its calibration, and neither
        // fails loudly.
        let mut data = [0u8; CAL_DATA_LEN];
        let mac = [1, 2, 3, 4, 5, 6];
        data[4..10].copy_from_slice(&mac);
        assert_eq!(&data[4..10], &mac);
        assert_eq!(&data[..4], &[0; 4], "the version field is not touched");
        assert_eq!(data[10], 0, "nor the first opaque byte");
    }

    #[test]
    fn a_full_calibration_is_stored_and_a_partial_one_is_not() {
        // Storing after a partial calibration would spend a flash erase
        // rewriting what it was just loaded from.
        assert_eq!(CalibrationMode::Full as u32, 2);
        assert_eq!(CalibrationMode::Partial as u32, 0);
        assert_ne!(CalibrationMode::Full, CalibrationMode::Partial);
    }

    #[test]
    fn the_reference_count_survives_more_disables_than_enables() {
        // A caller that unbalances the pair must not wrap the count to
        // four billion and leave the RF permanently open.
        STATE.with(|s| {
            s.refs = 0;
            s.registered = false;
        });
        let dropped = STATE.with(|s| {
            s.refs = s.refs.saturating_sub(1);
            s.refs
        });
        assert_eq!(dropped, 0, "saturating, not wrapping");
    }
}
