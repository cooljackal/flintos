// SPDX-License-Identifier: Apache-2.0

//! Hands a calibration to Espressif's PHY blob, on the board, and reports what
//! came back.
//!
//! Step 3.6 wrote the whole bring-up path — the calibration store, the init
//! data table, the eFuse MAC, the radio clocks, [`radio_esp32::phy::enable`] —
//! and every piece of it passes on the host. **None of that is evidence.**
//! Everything else in `radio/esp32` either answers a call *from* the blob or
//! computes a table *for* it; both can be entirely wrong and still compile,
//! link, and pass a test written by whoever got them wrong.
//!
//! This is the first FlintOS binary that calls `register_chipv7_phy` and waits
//! for an answer.
//!
//! # What it found
//!
//! Measured on an ESP32-WROOM (`board-esp32-devkitc`), and every number here
//! came off the board rather than out of a datasheet:
//!
//! - The archives **link**, and only `libphy.a` gets pulled in. Nothing in the
//!   OSI table is needed to bring the PHY up.
//! - `register_chipv7_phy` **accepts our tables** — the 128-byte init data,
//!   the eFuse MAC, the DPORT clock masks. It returns 0.
//! - A **full calibration is ~183 ms**, and re-enabling an already-registered
//!   PHY is **~250 µs**. The refcount, the wakeup path and the close path all
//!   behave.
//! - **The calibration store is a net loss as built.** Reading 1904 bytes back
//!   out of `kvstore` takes **~1.36 s** — seven and a half times the
//!   calibration it exists to avoid. See below.
//!
//! # The kernel bug it found, since fixed
//!
//! Run as an ordinary preemptible task, the bring-up died part-way through the
//! calibration every time, with a double exception: the first fault at a
//! `retw` in some ROM leaf (`xthal_get_ccount`, `ets_delay_us` — it varied),
//! the second inside our own `WindowUnderflow8` loading from junk
//! (`0xffffffed`, `0x00000ca6`). It ran perfectly with interrupts masked,
//! which made it look like something about the blob.
//!
//! It was not. Four of the six window overflow/underflow vectors were wrong,
//! and had been from the day they were written: they used `a1` as the base for
//! the second register group instead of loading the caller's stack pointer
//! from `[a1 - 12]`, so every spill landed one frame too low — and the
//! innermost one landed below the live stack pointer, exactly where the trap
//! entry puts its frame. Masking interrupts removed the trap frame, which is
//! why masking "fixed" it.
//!
//! Nothing in FlintOS's own Rust ever reached those handlers: LLVM compiles a
//! direct call into `call4`, and the two call4 handlers were right. A
//! GCC-built blob is all call8 and call12. See `arch/xtensa/src/asm/vectors.S`
//! and the `call8_windows_survive_preemption` self-test.
//!
//! # What is still not fixed
//!
//! **`kvstore::get` is a full forward scan**, CRC-ing every record. Loading a
//! calibration is eighteen of them — version, MAC, checksum and fifteen chunks
//! — so the cost is quadratic in the size of the log, and every read goes
//! through a cache-off window. Hence 1.36 s. Until that changes, storing the
//! calibration costs more than recalculating it.
//!
//! # Running it
//!
//! ```text
//! make erase                                          # a genuine first boot
//! make flash APP=radioprobe BOARD=... EXTRA_FEATURES=blobs
//! ```
//!
//! `make erase` matters more than it looks. `kvstore` is an append-only log
//! with no compaction, and it appends at the first offset that does not parse
//! as a record. Anything else that has written to the `nvs` partition — an
//! earlier `flashprobe` run writes a raw pattern at offset 0x100 — stops the
//! scan there, and every subsequent `set` then writes onto bytes that are not
//! erased. NOR flash only clears bits, so those writes return `Ok` and land as
//! garbage. That is what "stored" followed by "no stored calibration" on the
//! next boot looks like, and it cost an hour to tell apart from a bug in the
//! save.
//!
//! # What it deliberately does not do
//!
//! It does not transmit. Bringing the PHY up is as far as #65 goes; a radio
//! that registers happily and then transmits badly is #66/#67's problem, and
//! needs a receiver to detect at all.

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

kernel::flint_app!(main, abi = 1);

fn main() {
    // 8 KiB rather than the usual 4, because `register_chipv7_phy` is a blob
    // of unknown frame depth. It is not what fixed the fault -- 4 KiB and
    // 8 KiB fail the same way -- but it removes stack depth from the list of
    // things the next person has to rule out.
    //
    // **Not 16 KiB**, which is `MAX_STACK_SIZE` and ought to be legal: at that
    // size this board stops dead part-way through a log line, before the radio
    // is touched at all -- it dies in whatever it does next, which was the
    // heap in one arrangement and the first flash read in another. Retested
    // after the window vectors were fixed and it is unchanged, so it is a
    // separate bug and still open.
    task::spawn("radioprobe", run, Priority::Normal(2), 8192);
}

/// The build that cannot do anything, and says so rather than looking idle.
///
/// Without `blobs` there is no archive to call, so the calls below are not
/// compiled at all — a `cfg`-ed body rather than a `compile_error!`, because
/// `make check-all` builds every application in the workspace and has no
/// `.blobs/` directory to build against.
#[cfg(not(feature = "blobs"))]
fn run() {
    loop {
        api::log_warn!("[radio] built without the blobs; there is nothing to probe");
        api::log_warn!("[radio]   make blobs");
        api::log_warn!("[radio]   make flash APP=radioprobe BOARD=... EXTRA_FEATURES=blobs");
        task::sleep_ms(5000);
    }
}

#[cfg(feature = "blobs")]
fn run() {
    use radio_esp32::phy;

    // Let the console settle, so the first line is not half-eaten by the boot
    // banner on a monitor that has just attached.
    task::sleep_ms(200);
    api::log_info!("[radio] probe starting");

    // The blob before anything else. Needs no clock, no calibration and no
    // memory, so if this comes back wrong nothing below is worth reading.
    //
    // The version *string* is empty here and only fills in later: `nm` says
    // `phy_version_str` is a common symbol, so it is a 40-byte buffer the blob
    // writes rather than a literal it returns. It is printed again after the
    // registration, which is where it becomes readable.
    api::log_info!("[radio] rf cal version: {}", unsafe { phy::rf_cal_version() });

    // The store is optional and reported rather than fatal: without it the PHY
    // still comes up, it just recalibrates on every boot. That is the whole
    // difference this probe is here to measure, so say plainly which of the
    // two runs this is going to be.
    if unsafe { radio_esp32::nvs::init() } {
        api::log_info!("[radio] nvs open; a calibration can persist across reset");
    } else {
        api::log_warn!("[radio] nvs unavailable; every boot will calibrate in full");
    }

    // Printed because it is what the calibration is bound to: a stored
    // calibration carrying a different MAC is refused, and if this address is
    // wrong then every second run silently pays for a full calibration.
    let m = unsafe { soc_esp32::efuse::base_mac() };
    api::log_info!(
        "[radio] mac {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    );

    // The heap. `register_chipv7_phy` is handed a 1904-byte calibration buffer
    // from it, and `alloc` against an uninitialised pool returns null — which
    // would surface as `PhyError::OutOfMemory` on a board with 100 KB free.
    api::log_info!("[radio] reclaiming the heap");
    let heap = unsafe { kernel::heap::init_from_map() };
    api::log_info!("[radio] heap: {} bytes reclaimed", heap);

    match probe() {
        Ok(()) => api::log_info!("[radio] PASS"),
        Err(e) => api::log_error!("[radio] FAIL: {}", e),
    }

    loop {
        task::sleep_ms(1000);
    }
}

/// One key/value round trip through the same store the calibration uses.
///
/// Called either side of the PHY bring-up. `kvstore` writes through, so a
/// `set` that returns `Ok` and a `get` that cannot find it again means the
/// write never reached the part — and doing it twice says whether that was
/// already true before the radio was touched.
///
/// It earned its place: the calibration save reported success and the next
/// boot found nothing, which reads as a bug in the save. This said the store
/// was already broken *before* the PHY ran, which is a different problem with
/// a different fix (see the note about `make erase` in the module docs).
#[cfg(feature = "blobs")]
fn kv_round_trip(tag: &str, key: &[u8]) {
    let outcome = radio_esp32::nvs::with_store(
        |s| {
            if let Err(e) = s.set(key, b"landed") {
                return Some(Err(e));
            }
            let mut back = [0u8; 16];
            Some(s.get(key, &mut back).map(|n| &back[..n] == b"landed"))
        },
        None,
    );
    match outcome {
        Some(Ok(true)) => api::log_info!("[radio] kvstore round trip {}: ok", tag),
        Some(Ok(false)) => api::log_error!("[radio] kvstore round trip {}: wrong value", tag),
        Some(Err(e)) => api::log_error!("[radio] kvstore round trip {}: {:?}", tag, e),
        None => api::log_error!("[radio] kvstore round trip {}: no store", tag),
    }
}

/// Read the calibration back through the same path a later boot would use.
#[cfg(feature = "blobs")]
fn verify_stored() {
    use radio_esp32::calibration::{self, Invalid, CAL_DATA_LEN};
    use radio_esp32::phy;

    let version = unsafe { phy::rf_cal_version() };
    let mac = unsafe { soc_esp32::efuse::base_mac() };

    // On the heap for the same reason `phy::register_once` puts it there.
    let buf = unsafe { kernel::heap::alloc(CAL_DATA_LEN, 4) };
    if buf.is_null() {
        api::log_error!("[radio] no room to read the calibration back");
        return;
    }
    let out: &mut [u8; CAL_DATA_LEN] = unsafe { &mut *(buf as *mut [u8; CAL_DATA_LEN]) };

    let t0 = kernel::clock::now_us();
    let r = radio_esp32::nvs::with_store(
        |s| calibration::load(s, version, &mac, out),
        Err(Invalid::Missing),
    );
    let t1 = kernel::clock::now_us();
    api::log_info!("[radio] calibration load took {} us", t1 - t0);
    match r {
        Ok(()) => api::log_info!("[radio] read-back OK: it is on flash in this boot"),
        Err(e) => api::log_error!("[radio] read-back failed ({:?}): the save did not land", e),
    }
    unsafe { kernel::heap::free(buf, kernel::heap::Caps::Internal) };
}

/// The three calls, timed.
///
/// Split out so the reporting above is not interleaved with the sequence being
/// measured, and so every failure leaves through one place.
#[cfg(feature = "blobs")]
fn probe() -> Result<(), &'static str> {
    use radio_esp32::phy;
    use soc_esp32::dport::RADIO_CLK_WIFI;

    kv_round_trip("before the PHY", b"probe.a");

    // The expensive one. Everything #65 built is on the other side of this
    // call, and until now none of it has run.
    //
    // **Preemptible, and that is the test.** This ran inside `cs_with` for as
    // long as it took to find out why it double-faulted otherwise, and the
    // answer was the call8/call12 window vectors rather than anything about
    // the blob. With those fixed, the tick lands inside `register_chipv7_phy`
    // some hundreds of times across ~190 ms and the calibration survives it.
    // Masking again would hide the only regression test that exists for it.
    let t0 = kernel::clock::now_us();
    let first = unsafe { phy::enable(RADIO_CLK_WIFI) };
    let t1 = kernel::clock::now_us();
    match first {
        Ok(()) => {
            api::log_info!("[radio] enable: {} us (refs={})", t1 - t0, phy::refs());
            // Still empty, and printed anyway because that is the finding.
            // esp-idf logs this string *before* registering and gets
            // "4670,719f9f6,..."; here it is empty both before and after, on a
            // registration that otherwise succeeds. `phy_version_str` is a
            // common symbol -- a 40-byte buffer the blob fills -- and it is
            // correctly allocated in DRAM, so something writes it that we are
            // not calling. Cosmetic, but it is the one thing that would
            // identify which archive `make blobs` fetched.
            api::log_info!("[radio] phy version: {}", unsafe { phy::version_str() });
        }
        Err(e) => {
            api::log_error!("[radio] enable returned {:?} after {} us", e, t1 - t0);
            return Err("register_chipv7_phy did not accept our tables");
        }
    }

    // Read the calibration straight back, in this boot.
    //
    // The store reports success and the *next* boot finds nothing, which are
    // two different bugs: a write that never reached flash, or one that did
    // and is not found by a fresh `Store::open`. Reading it here separates
    // them, and costs one pass over 1904 bytes.
    kv_round_trip("after the PHY", b"probe.b");
    verify_stored();

    // A second reference, which must take the cheap path: `phy_wakeup_init`
    // rather than another registration. Two orders of magnitude apart, so a
    // refcount that has forgotten it already registered shows up here as a
    // time rather than as a wrong answer.
    let t2 = kernel::clock::now_us();
    unsafe { phy::enable(RADIO_CLK_WIFI) }.map_err(|_| "the second enable failed")?;
    let t3 = kernel::clock::now_us();
    api::log_info!("[radio] re-enable: {} us (refs={})", t3 - t2, phy::refs());
    if phy::refs() != 2 {
        return Err("two enables did not produce two references");
    }
    if t3 - t2 >= t1 - t0 {
        // Not fatal on its own -- a partial calibration on the first call
        // narrows the gap -- but it is the shape of the bug the refcount is
        // there to prevent, so it is worth saying out loud.
        api::log_warn!("[radio] the re-enable was no cheaper; did it register twice?");
    }

    // Down again. The first disable drops a reference and must *not* close the
    // RF; the second must. Nothing here can observe the difference directly,
    // so the check is the count, and the point of doing it at all is that a
    // `phy_close_rf` on a still-referenced PHY is exactly the kind of thing
    // that only misbehaves later.
    unsafe { phy::disable(RADIO_CLK_WIFI) };
    if phy::refs() != 1 {
        return Err("the first disable dropped more than one reference");
    }
    unsafe { phy::disable(RADIO_CLK_WIFI) };
    if phy::refs() != 0 {
        return Err("the last disable left a reference behind");
    }
    api::log_info!("[radio] closed; refs=0");

    // And back up, from cold. The registration is done once for the life of
    // the boot, so this must be the cheap path even though the RF was closed
    // in between -- which is what `registered` outliving `refs` is for.
    let t4 = kernel::clock::now_us();
    unsafe { phy::enable(RADIO_CLK_WIFI) }.map_err(|_| "re-enable after close failed")?;
    let t5 = kernel::clock::now_us();
    api::log_info!("[radio] enable after close: {} us", t5 - t4);
    unsafe { phy::disable(RADIO_CLK_WIFI) };

    Ok(())
}
