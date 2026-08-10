// SPDX-License-Identifier: Apache-2.0

//! RF calibration data, persisted across reboots.
//!
//! Calibrating the PHY takes real time at boot — esp-idf measures a full
//! calibration in the hundreds of milliseconds — so the result is kept and
//! reused. That is the whole point of this module, and it is also 3.6's
//! acceptance criterion: *calibration data persists across a reboot*.
//!
//! # What is stored, and why the checks are what they are
//!
//! esp-idf keeps three things (`esp_phy_init.c`, `load_cal_data_from_nvs_handle`):
//! the calibration blob, the PHY's own calibration-format version, and the
//! station MAC. It rejects the blob unless both of the latter match, and this
//! does the same, for the same reasons:
//!
//! - **Version.** The blob is opaque to us and its layout belongs to the PHY.
//!   Feeding a stale layout to a newer PHY is not a decode error, it is a
//!   radio calibrated with someone else's numbers.
//! - **MAC.** Calibration is per-part. A blob copied onto another board — or
//!   restored from a backup taken on one — is worse than no calibration,
//!   because it looks valid.
//!
//! Rejecting costs one recalibration. Accepting a wrong blob costs range, and
//! presents as a bad antenna rather than as anything pointing here.
//!
//! # Chunking, and why that forces a checksum
//!
//! `esp_phy_calibration_data_t` is 1904 bytes. `kvstore`'s [`MAX_VALUE_LEN`]
//! is 128, so the blob is split across [`CHUNKS`] records rather than raising
//! that limit — `kvstore` sizes stack buffers from it, and a 2 KB buffer on a
//! 4 KiB task stack is a worse problem than a loop.
//!
//! **The split is what makes the checksum load-bearing.** `kvstore` is an
//! append-only log: a save that is interrupted part-way leaves some chunks
//! new and some chunks old, every one of them individually well-formed. No
//! per-record integrity check can see that, because nothing is corrupt — the
//! records simply do not belong to the same calibration.
//!
//! So the checksum covers the whole blob and is **written last**, which makes
//! it the commit point. Interrupted before it, the stored checksum still
//! describes the previous calibration and cannot match the mixture that is
//! now on flash, so the load is rejected and the radio recalibrates. That is
//! the correct outcome and it is reached by construction rather than by luck.

use kvstore::{Error as KvError, Store, Storage, MAX_VALUE_LEN};

/// `sizeof(esp_phy_calibration_data_t)` for the ESP32.
///
/// ```c
/// typedef struct {
///     uint8_t version[4];
///     uint8_t mac[6];
///     uint8_t opaque[1894];
/// } esp_phy_calibration_data_t;
/// ```
///
/// The version and MAC inside the struct are the PHY's business. The copies
/// this module stores separately are ours, and exist so a mismatch can be
/// found without reading 1904 bytes back first.
///
/// Verified against `esp-idf` tag **v4.4**, the revision `tools/fetch-blobs.sh`
/// pins the archives to — `components/esp_phy/include/esp_phy_init.h`, read
/// rather than remembered. 4 + 6 + 1894 = 1904.
///
/// Worth having checked: nothing in the fetched archives states this size.
/// `register_chipv7_phy` takes a pointer and the length is not in the symbol
/// table, so a wrong value here would be silent in the direction that matters
/// — too small, and the PHY writes past the end of a buffer sized from it.
/// If the pinned IDF revision ever moves, this is one of the constants to
/// re-read rather than assume.
pub const CAL_DATA_LEN: usize = 1904;

/// `esp_phy_calibration_mode_t`, from the same header.
///
/// What the caller asks `register_chipv7_phy` to do. `Full` is what a
/// rejected or missing stored calibration means; `Partial` is the normal path
/// once one has been loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum CalibrationMode {
    Partial = 0x0000_0000,
    None = 0x0000_0001,
    Full = 0x0000_0002,
}

/// `sizeof(esp_phy_init_data_t)` — `uint8_t params[128]`, same header, same
/// tag. Recorded here because it is the other half of 3.6 and it is a
/// pleasing accident that it fits one `kvstore` record exactly.
pub const PHY_INIT_DATA_LEN: usize = 128;

/// Bytes per stored chunk. The largest value `kvstore` takes.
pub const CHUNK_LEN: usize = MAX_VALUE_LEN;

/// How many records the blob occupies.
pub const CHUNKS: usize = CAL_DATA_LEN.div_ceil(CHUNK_LEN);

/// Keys. Short deliberately: every byte of a key is a byte of flash in every
/// record that carries it, times [`CHUNKS`].
const KEY_VERSION: &[u8] = b"phy.v";
const KEY_MAC: &[u8] = b"phy.m";
const KEY_SUM: &[u8] = b"phy.s";
/// Chunk keys are `phy.<n>`, built without formatting — this crate is
/// `no_std` and a formatter here would pull in machinery for six bytes.
const KEY_CHUNK_PREFIX: &[u8] = b"phy.";

// The chunking has to cover the blob exactly, and a wrong `CHUNK_LEN` would
// otherwise show up as a runtime read failure on hardware. Checked at compile
// time, so changing `CAL_DATA_LEN` -- which is expected, see its docs -- fails
// the build rather than a test somebody might not run.
const _: () = assert!(CHUNK_LEN <= MAX_VALUE_LEN, "a chunk must fit one record");
const _: () = assert!(PHY_INIT_DATA_LEN <= MAX_VALUE_LEN, "PHY init data fits one record");
const _: () = assert!(CHUNKS * CHUNK_LEN >= CAL_DATA_LEN, "the chunks must cover the blob");
const _: () = assert!(
    (CHUNKS - 1) * CHUNK_LEN < CAL_DATA_LEN,
    "one chunk more than needed: every save would write a record of nothing"
);

/// Why a stored calibration was not usable.
///
/// Every one of these means "recalibrate", and none of them is an error in
/// the sense of something being broken — a first boot reports `Missing`.
/// They are distinguished so the reason can be logged, because "the radio
/// recalibrated again" with no reason attached is unactionable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invalid {
    /// Nothing has been stored yet. The expected state on a first boot.
    Missing,
    /// Stored against a different PHY calibration-format version.
    VersionMismatch { stored: u32, expected: u32 },
    /// Stored against a different station MAC — another board's calibration.
    MacMismatch,
    /// The checksum does not describe the bytes that were read back. With an
    /// append-only log this is what an interrupted save looks like.
    ChecksumMismatch,
    /// A chunk was missing or short. Also an interrupted save, caught earlier.
    Truncated,
    /// The store itself failed.
    Storage(KvError),
}

/// FNV-1a, 32-bit.
///
/// Not a CRC: the job here is to notice a *mixture* of two calibrations, not
/// to characterise bit errors on a wire, and any decent avalanche does that.
/// Chosen because it is eight lines that can be checked by eye against the
/// spec, which matters more here than the strength difference.
fn checksum(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for &b in data {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

/// The key for chunk `n`, written into `buf`.
fn chunk_key(n: usize, buf: &mut [u8; 8]) -> &[u8] {
    buf[..KEY_CHUNK_PREFIX.len()].copy_from_slice(KEY_CHUNK_PREFIX);
    let mut len = KEY_CHUNK_PREFIX.len();
    if n >= 10 {
        buf[len] = b'0' + (n / 10) as u8;
        len += 1;
    }
    buf[len] = b'0' + (n % 10) as u8;
    len += 1;
    &buf[..len]
}

/// Read a `u32` value stored little-endian.
fn get_u32<S: Storage>(store: &Store<S>, key: &[u8]) -> Result<Option<u32>, KvError> {
    let mut buf = [0u8; MAX_VALUE_LEN];
    match store.get(key, &mut buf) {
        Ok(4) => Ok(Some(u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]))),
        Ok(_) => Ok(None), // wrong width: treat as absent rather than guess
        Err(KvError::NotFound) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Load a previously stored calibration into `out`.
///
/// `expected_version` is the PHY's current calibration-format version, which
/// on hardware comes from the blob's own `phy_get_rf_cal_version()`.
/// `expected_mac` is this part's station MAC.
///
/// The version and MAC are checked *before* the blob is read back, so a
/// mismatch costs four small reads rather than 1904 bytes.
pub fn load<S: Storage>(
    store: &Store<S>,
    expected_version: u32,
    expected_mac: &[u8; 6],
    out: &mut [u8; CAL_DATA_LEN],
) -> Result<(), Invalid> {
    let stored_version = match get_u32(store, KEY_VERSION).map_err(Invalid::Storage)? {
        Some(v) => v,
        None => return Err(Invalid::Missing),
    };
    if stored_version != expected_version {
        return Err(Invalid::VersionMismatch {
            stored: stored_version,
            expected: expected_version,
        });
    }

    let mut mac = [0u8; MAX_VALUE_LEN];
    match store.get(KEY_MAC, &mut mac) {
        Ok(6) => {}
        Ok(_) => return Err(Invalid::MacMismatch),
        Err(KvError::NotFound) => return Err(Invalid::Missing),
        Err(e) => return Err(Invalid::Storage(e)),
    }
    if &mac[..6] != expected_mac {
        return Err(Invalid::MacMismatch);
    }

    let stored_sum = match get_u32(store, KEY_SUM).map_err(Invalid::Storage)? {
        Some(s) => s,
        None => return Err(Invalid::Missing),
    };

    let mut key = [0u8; 8];
    let mut buf = [0u8; MAX_VALUE_LEN];
    for n in 0..CHUNKS {
        let at = n * CHUNK_LEN;
        let want = CHUNK_LEN.min(CAL_DATA_LEN - at);
        match store.get(chunk_key(n, &mut key), &mut buf) {
            Ok(got) if got == want => out[at..at + want].copy_from_slice(&buf[..want]),
            Ok(_) => return Err(Invalid::Truncated),
            Err(KvError::NotFound) => return Err(Invalid::Truncated),
            Err(e) => return Err(Invalid::Storage(e)),
        }
    }

    // Last, because it is the thing that decides. Every check above can pass
    // on a half-written save; this one cannot.
    if checksum(out) != stored_sum {
        return Err(Invalid::ChecksumMismatch);
    }
    Ok(())
}

/// Store a calibration, replacing whatever was there.
///
/// **Order is load-bearing.** The chunks go down first, then the version and
/// MAC, and the checksum last of all. That makes the checksum the commit
/// point: an interruption anywhere before it leaves the previous checksum on
/// flash, which cannot describe the new mixture, so [`load`] rejects it and
/// the radio recalibrates. Reordering these lines silently converts a
/// recalibration into a radio running on half of one calibration and half of
/// another.
pub fn save<S: Storage>(
    store: &mut Store<S>,
    version: u32,
    mac: &[u8; 6],
    data: &[u8; CAL_DATA_LEN],
) -> Result<(), KvError> {
    let mut key = [0u8; 8];
    for n in 0..CHUNKS {
        let at = n * CHUNK_LEN;
        let end = (at + CHUNK_LEN).min(CAL_DATA_LEN);
        store.set(chunk_key(n, &mut key), &data[at..end])?;
    }
    store.set(KEY_VERSION, &version.to_le_bytes())?;
    store.set(KEY_MAC, mac)?;
    store.set(KEY_SUM, &checksum(data).to_le_bytes())?;
    Ok(())
}

/// [`save`], erasing the store and retrying once if it is full.
///
/// `kvstore` is append-only with no compaction, so repeated saves eventually
/// return [`KvError::Full`]. Calibration is the one kind of stored data where
/// erasing everything is a reasonable answer to that: it is regenerable —
/// the cost is one recalibration — and a radio that stops being able to save
/// its calibration degrades quietly, every boot, forever.
///
/// **It erases the whole store, not just these keys**, so anything else
/// sharing this partition goes too. That is why it is a separate function
/// rather than the behaviour of `save`: the caller has to decide it is
/// acceptable, and today the `nvs` partition holds nothing else.
pub fn save_or_reset<S: Storage>(
    store: &mut Store<S>,
    version: u32,
    mac: &[u8; 6],
    data: &[u8; CAL_DATA_LEN],
) -> Result<(), KvError> {
    match save(store, version, mac, data) {
        Err(KvError::Full) => {
            store.erase_all()?;
            save(store, version, mac, data)
        }
        other => other,
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// Big enough for several saves, so the full/erase path is reachable.
    const CAP: usize = 32 * 1024;

    struct Fake {
        bytes: [u8; CAP],
    }

    impl Fake {
        fn new() -> Self {
            Fake { bytes: [0xFF; CAP] }
        }
    }

    impl Storage for Fake {
        const SECTOR_SIZE: u32 = 4096;
        fn capacity(&self) -> u32 {
            CAP as u32
        }
        fn read(&self, offset: u32, buf: &mut [u8]) -> Result<(), KvError> {
            let o = offset as usize;
            if o + buf.len() > CAP {
                return Err(KvError::Io);
            }
            buf.copy_from_slice(&self.bytes[o..o + buf.len()]);
            Ok(())
        }
        fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), KvError> {
            let o = offset as usize;
            if o + data.len() > CAP {
                return Err(KvError::Io);
            }
            self.bytes[o..o + data.len()].copy_from_slice(data);
            Ok(())
        }
        fn erase_all(&mut self) -> Result<(), KvError> {
            self.bytes = [0xFF; CAP];
            Ok(())
        }
    }

    fn store() -> Store<Fake> {
        let mut f = Fake::new();
        f.erase_all().unwrap();
        Store::open(f).unwrap()
    }

    /// Something with structure, so a chunk written to the wrong offset shows
    /// up rather than blending into a field of zeros.
    fn pattern(seed: u8) -> [u8; CAL_DATA_LEN] {
        let mut d = [0u8; CAL_DATA_LEN];
        for (i, b) in d.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(31).wrapping_add(seed);
        }
        d
    }

    const MAC: [u8; 6] = [0xC0, 0x49, 0xEF, 0xD1, 0x13, 0xCC];

    #[test]
    fn a_calibration_survives_a_round_trip() {
        // The acceptance criterion, minus the reboot: what went in comes back
        // out, byte for byte, across the chunk boundaries.
        let mut s = store();
        let data = pattern(7);
        save(&mut s, 42, &MAC, &data).unwrap();

        let mut out = [0u8; CAL_DATA_LEN];
        load(&s, 42, &MAC, &mut out).unwrap();
        assert_eq!(out, data);
    }

    #[test]
    fn the_last_chunk_is_short_and_still_round_trips() {
        // 1904 is not a multiple of 128: the final chunk carries 112 bytes.
        // An off-by-one here reads 16 bytes of the previous chunk into the
        // tail and the checksum is the only thing that would notice.
        assert_eq!(CHUNKS, 15);
        assert_eq!(CAL_DATA_LEN % CHUNK_LEN, 112);

        let mut s = store();
        let data = pattern(3);
        save(&mut s, 1, &MAC, &data).unwrap();
        let mut out = [0u8; CAL_DATA_LEN];
        load(&s, 1, &MAC, &mut out).unwrap();
        assert_eq!(&out[CAL_DATA_LEN - 112..], &data[CAL_DATA_LEN - 112..]);
    }

    #[test]
    fn nothing_stored_reports_missing_rather_than_failing() {
        // First boot. Not an error, and it has to be distinguishable from one.
        let s = store();
        let mut out = [0u8; CAL_DATA_LEN];
        assert_eq!(load(&s, 1, &MAC, &mut out), Err(Invalid::Missing));
    }

    #[test]
    fn a_different_phy_version_is_refused() {
        let mut s = store();
        save(&mut s, 100, &MAC, &pattern(1)).unwrap();
        let mut out = [0u8; CAL_DATA_LEN];
        assert_eq!(
            load(&s, 101, &MAC, &mut out),
            Err(Invalid::VersionMismatch { stored: 100, expected: 101 })
        );
    }

    #[test]
    fn another_boards_calibration_is_refused() {
        // The case that matters most, because it is the one that looks valid:
        // a blob restored onto different silicon calibrates the radio with
        // another part's numbers and presents as a bad antenna.
        let mut s = store();
        save(&mut s, 1, &MAC, &pattern(1)).unwrap();

        let mut other = MAC;
        other[5] ^= 0x01;
        let mut out = [0u8; CAL_DATA_LEN];
        assert_eq!(load(&s, 1, &other, &mut out), Err(Invalid::MacMismatch));
    }

    #[test]
    fn a_save_interrupted_before_the_checksum_is_refused() {
        // The failure chunking introduces, and the reason the checksum is
        // written last. Every record here is individually well-formed; what
        // is wrong is that they are not all from the same calibration.
        let mut s = store();
        save(&mut s, 1, &MAC, &pattern(1)).unwrap();

        // A second save that gets as far as the chunks and stops -- exactly
        // what `save` does up to its last line.
        let second = pattern(2);
        let mut key = [0u8; 8];
        for n in 0..CHUNKS {
            let at = n * CHUNK_LEN;
            let end = (at + CHUNK_LEN).min(CAL_DATA_LEN);
            s.set(chunk_key(n, &mut key), &second[at..end]).unwrap();
        }

        let mut out = [0u8; CAL_DATA_LEN];
        assert_eq!(
            load(&s, 1, &MAC, &mut out),
            Err(Invalid::ChecksumMismatch),
            "a mixture of two calibrations must not load"
        );
    }

    #[test]
    fn a_save_interrupted_mid_chunk_is_refused() {
        // Stopping earlier still: only some chunks replaced. The stored
        // checksum belongs to neither calibration.
        let mut s = store();
        save(&mut s, 1, &MAC, &pattern(1)).unwrap();

        let second = pattern(2);
        let mut key = [0u8; 8];
        for n in 0..CHUNKS / 2 {
            let at = n * CHUNK_LEN;
            let end = (at + CHUNK_LEN).min(CAL_DATA_LEN);
            s.set(chunk_key(n, &mut key), &second[at..end]).unwrap();
        }

        let mut out = [0u8; CAL_DATA_LEN];
        assert_eq!(load(&s, 1, &MAC, &mut out), Err(Invalid::ChecksumMismatch));
    }

    #[test]
    fn a_missing_chunk_is_caught_before_the_checksum() {
        // Version and MAC present, chunks not. Reported as truncated rather
        // than as a checksum failure, because the two want different
        // explanations in a log.
        let mut s = store();
        s.set(KEY_VERSION, &1u32.to_le_bytes()).unwrap();
        s.set(KEY_MAC, &MAC).unwrap();
        s.set(KEY_SUM, &0u32.to_le_bytes()).unwrap();

        let mut out = [0u8; CAL_DATA_LEN];
        assert_eq!(load(&s, 1, &MAC, &mut out), Err(Invalid::Truncated));
    }

    #[test]
    fn resaving_replaces_rather_than_accumulates() {
        // The store is append-only, so the second save shadows the first.
        // What must not happen is the *old* one loading afterwards.
        let mut s = store();
        save(&mut s, 1, &MAC, &pattern(1)).unwrap();
        save(&mut s, 1, &MAC, &pattern(2)).unwrap();

        let mut out = [0u8; CAL_DATA_LEN];
        load(&s, 1, &MAC, &mut out).unwrap();
        assert_eq!(out, pattern(2));
    }

    #[test]
    fn a_full_store_is_erased_and_the_save_retried() {
        // Append-only with no compaction: saves eventually fill it. Losing
        // the calibration costs one recalibration, and a radio that can never
        // save again degrades quietly on every boot instead.
        let mut s = store();
        let mut saves = 0;
        loop {
            match save(&mut s, 1, &MAC, &pattern(1)) {
                Ok(()) => saves += 1,
                Err(KvError::Full) => break,
                Err(e) => panic!("unexpected {e:?}"),
            }
            assert!(saves < 100, "the store never filled");
        }
        assert!(saves > 0, "not even one save fitted");

        save_or_reset(&mut s, 1, &MAC, &pattern(9)).unwrap();
        let mut out = [0u8; CAL_DATA_LEN];
        load(&s, 1, &MAC, &mut out).unwrap();
        assert_eq!(out, pattern(9));
    }

    #[test]
    fn chunk_keys_are_distinct_and_ordered() {
        // `phy.1` and `phy.10` must not collide, which a naive single-digit
        // scheme would allow at chunk 10.
        let mut seen = [[0u8; 8]; CHUNKS];
        let mut lens = [0usize; CHUNKS];
        for n in 0..CHUNKS {
            let mut k = [0u8; 8];
            let key = chunk_key(n, &mut k);
            lens[n] = key.len();
            seen[n][..key.len()].copy_from_slice(key);
        }
        for i in 0..CHUNKS {
            for j in i + 1..CHUNKS {
                assert!(
                    !(lens[i] == lens[j] && seen[i][..lens[i]] == seen[j][..lens[j]]),
                    "chunks {i} and {j} share a key"
                );
            }
        }
        let mut k = [0u8; 8];
        assert_eq!(chunk_key(0, &mut k), b"phy.0");
        assert_eq!(chunk_key(9, &mut k), b"phy.9");
        assert_eq!(chunk_key(10, &mut k), b"phy.10");
        assert_eq!(chunk_key(14, &mut k), b"phy.14");
    }

    #[test]
    fn the_checksum_notices_a_single_flipped_bit() {
        // It only has to catch a mixture, but a checksum that misses one bit
        // is not catching much.
        let a = pattern(1);
        let mut b = a;
        b[CAL_DATA_LEN / 2] ^= 0x01;
        assert_ne!(checksum(&a), checksum(&b));

        // And the published FNV-1a vector, so the constants are not a guess.
        assert_eq!(checksum(b""), 0x811C_9DC5);
        assert_eq!(checksum(b"a"), 0xE40C_292C);
        assert_eq!(checksum(b"foobar"), 0xBF9C_F968);
    }

    #[test]
    fn every_key_fits_and_the_blob_is_the_size_idf_says() {
        // Read out of esp-idf v4.4's esp_phy_init.h, the revision the blobs
        // are pinned to: version[4] + mac[6] + opaque[1894].
        assert_eq!(CAL_DATA_LEN, 4 + 6 + 1894);
        assert_eq!(CAL_DATA_LEN, 1904, "sizeof(esp_phy_calibration_data_t)");
        assert_eq!(PHY_INIT_DATA_LEN, 128, "sizeof(esp_phy_init_data_t)");
        // The enum values are the ABI, not an ordering we chose.
        assert_eq!(CalibrationMode::Partial as u32, 0);
        assert_eq!(CalibrationMode::None as u32, 1);
        assert_eq!(CalibrationMode::Full as u32, 2);
        for key in [KEY_VERSION, KEY_MAC, KEY_SUM] {
            assert!(!key.is_empty() && key.len() <= kvstore::MAX_KEY_LEN);
        }
    }
}
