// SPDX-License-Identifier: Apache-2.0

//! A key/value store that survives being interrupted.
//!
//! Nothing in FlintOS outlives a reboot today. Calibration constants, a device
//! identity, a Wi-Fi credential later — all of it needs somewhere to live, and
//! that somewhere is flash, which has two awkward properties: you can only
//! clear bits by erasing a whole sector, and the power can go away in the
//! middle of a write.
//!
//! # Append-only, because that is what makes power loss survivable
//!
//! Writing a key does not modify the old one. It appends a new entry and the
//! newest wins. A write interrupted halfway leaves a torn entry at the end of
//! the log, whose checksum fails, and the scan stops there — every key written
//! before it is exactly as it was. There is no window in which a completed
//! earlier write can be lost, because nothing ever overwrites it.
//!
//! The cost is space: the log fills with superseded entries and eventually
//! needs compacting. [`Store::used`] and [`Error::Full`] make that the
//! caller's decision rather than a surprise.
//!
//! # No storage here
//!
//! This crate is `lib/`: no registers, no chip. It talks to a [`Storage`],
//! which a driver implements over real flash. That is what lets the whole of
//! the interesting behaviour — tearing, corruption, exhaustion — be tested
//! against a fake, at every byte offset, which no on-target test could do.

#![no_std]

/// Flash, as this store needs to see it.
///
/// Deliberately small. Everything about wear levelling, cache disabling and
/// which core is stalled belongs to the implementation, not here.
pub trait Storage {
    /// Bytes in an erase unit. Writes may be finer; erases may not.
    const SECTOR_SIZE: u32;

    /// Total bytes available.
    fn capacity(&self) -> u32;

    /// Fill `buf` from `offset`.
    fn read(&self, offset: u32, buf: &mut [u8]) -> Result<(), Error>;

    /// Write `data` at `offset`.
    ///
    /// The caller guarantees the region was erased and is untouched since.
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Error>;

    /// Erase the whole store.
    fn erase_all(&mut self) -> Result<(), Error>;
}

/// What went wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// No room for another entry. Compact or erase.
    Full,
    /// The key is longer than [`MAX_KEY_LEN`], or empty.
    BadKey,
    /// The value is longer than [`MAX_VALUE_LEN`].
    BadValue,
    /// No such key.
    NotFound,
    /// The caller's buffer is smaller than the stored value.
    BufferTooSmall,
    /// The storage refused the operation.
    Io,
}

/// Longest key. One byte holds the length.
pub const MAX_KEY_LEN: usize = 32;
/// Longest value.
pub const MAX_VALUE_LEN: usize = 128;

/// Marks the start of an entry.
///
/// Erased flash reads as `0xFF`, so the magic must not be `0xFFFF` — that is
/// how the scan tells "end of the log" from "an entry begins here".
const MAGIC: u16 = 0xA5C3;

/// magic(2) + key_len(1) + val_len(1) + crc(4)
const HEADER_LEN: usize = 8;

/// Entries start on a 4-byte boundary, because some flash controllers will
/// not write a partial word and it costs three bytes to avoid finding out.
const ALIGN: u32 = 4;

const fn align_up(v: u32) -> u32 {
    (v + ALIGN - 1) & !(ALIGN - 1)
}

/// Bytes one entry occupies, header and padding included.
pub const fn entry_len(key_len: usize, value_len: usize) -> u32 {
    align_up((HEADER_LEN + key_len + value_len) as u32)
}

/// CRC-32, the usual reflected polynomial, computed without a table.
///
/// A checksum over the header *and* the payload, so a torn write is caught
/// wherever it was torn — including in the header, where a plausible-looking
/// length would otherwise send the scan into the middle of the next entry.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

/// The most an entry can occupy, header included. One read's worth.
const MAX_ENTRY: usize = HEADER_LEN + MAX_KEY_LEN + MAX_VALUE_LEN;

/// What a single read of an entry established about it.
struct Entry {
    /// Bytes to advance to reach the next entry.
    total: u32,
    key_len: usize,
    val_len: usize,
}

/// Bytes the encoded entry at the start of `buf` occupies.
fn encoded_len(buf: &[u8]) -> usize {
    entry_len(buf[2] as usize, buf[3] as usize) as usize
}

/// Offset of the entry in `buf` whose key is `key`, if any.
///
/// `buf` is a run of encoded entries, as [`Store::compact`] builds. Walking it
/// rather than indexing is what keeps compaction allocation-free.
fn find_key(buf: &[u8], key: &[u8]) -> Option<usize> {
    let mut at = 0;
    while at + HEADER_LEN <= buf.len() {
        let klen = buf[at + 2] as usize;
        let total = encoded_len(&buf[at..]);
        if klen == key.len() && &buf[at + HEADER_LEN..at + HEADER_LEN + klen] == key {
            return Some(at);
        }
        at += total;
    }
    None
}

/// The log, over some [`Storage`].
pub struct Store<S: Storage> {
    storage: S,
    /// Where the next entry goes. Everything before it has been scanned.
    tail: u32,
}

impl<S: Storage> Store<S> {
    /// Open a store, scanning it to find the end of the log.
    pub fn open(storage: S) -> Result<Self, Error> {
        let mut s = Self { storage, tail: 0 };
        s.tail = s.scan_tail()?;
        Ok(s)
    }

    /// Bytes of the store in use, superseded entries included.
    pub fn used(&self) -> u32 {
        self.tail
    }

    /// Bytes still free.
    pub fn free(&self) -> u32 {
        self.storage.capacity().saturating_sub(self.tail)
    }

    /// Throw everything away.
    pub fn erase_all(&mut self) -> Result<(), Error> {
        self.storage.erase_all()?;
        self.tail = 0;
        Ok(())
    }

    /// Walk the log to the first entry that is absent or damaged.
    ///
    /// Stopping at the first bad entry is the whole recovery strategy. In an
    /// append-only log nothing valid can follow something torn, because the
    /// torn thing is what was being written when the power went.
    fn scan_tail(&self) -> Result<u32, Error> {
        let mut buf = [0u8; MAX_ENTRY];
        let mut at = 0;
        loop {
            match self.entry_into(at, &mut buf)? {
                Some(e) => at += e.total,
                None => return Ok(at),
            }
        }
    }

    /// Read the whole entry at `at` into `buf`, in **one** `Storage::read`.
    ///
    /// This exists because the obvious decomposition — read the header, then
    /// read the body, then let the caller read the key, then read the value —
    /// costs four reads per entry, and on this hardware a read is not a cheap
    /// thing to do four times. Every one goes through a window with the
    /// instruction cache switched off, so the fixed cost per call dwarfs the
    /// bytes moved: reading 168 bytes once beats reading 8 bytes four times by
    /// most of a factor of four.
    ///
    /// It reads a fixed `MAX_ENTRY` rather than the entry's own length,
    /// because the length is in the header and the header is what is being
    /// read. Overshooting into the next entry, or into erased flash, costs
    /// nothing; it is clamped to the region so it cannot overshoot into
    /// somebody else's partition.
    ///
    /// `buf` is the caller's so this can be reused across a whole scan without
    /// putting 168 bytes on the stack per entry.
    fn entry_into(&self, at: u32, buf: &mut [u8; MAX_ENTRY]) -> Result<Option<Entry>, Error> {
        let cap = self.storage.capacity();
        if at + HEADER_LEN as u32 > cap {
            return Ok(None);
        }
        let want = MAX_ENTRY.min((cap - at) as usize);
        self.storage.read(at, &mut buf[..want])?;

        let magic = u16::from_le_bytes([buf[0], buf[1]]);
        if magic != MAGIC {
            return Ok(None);
        }
        let key_len = buf[2] as usize;
        let val_len = buf[3] as usize;
        if key_len == 0 || key_len > MAX_KEY_LEN || val_len > MAX_VALUE_LEN {
            // A corrupt header with a believable magic. Treat it as the end
            // rather than trusting the lengths and walking off into whatever
            // follows.
            return Ok(None);
        }
        let total = entry_len(key_len, val_len);
        if at + total > cap {
            return Ok(None);
        }
        // The lengths passed their bounds check, so the body is inside
        // MAX_ENTRY; it can still fall outside what the clamp above allowed us
        // to read, at the very end of the region.
        let n = key_len + val_len;
        if HEADER_LEN + n > want {
            return Ok(None);
        }

        let stored_crc = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if crc_of(key_len, val_len, &buf[HEADER_LEN..HEADER_LEN + n]) != stored_crc {
            return Ok(None);
        }
        Ok(Some(Entry { total, key_len, val_len }))
    }

    /// Store `value` under `key`, superseding any earlier value.
    pub fn set(&mut self, key: &[u8], value: &[u8]) -> Result<(), Error> {
        if key.is_empty() || key.len() > MAX_KEY_LEN {
            return Err(Error::BadKey);
        }
        if value.len() > MAX_VALUE_LEN {
            return Err(Error::BadValue);
        }
        let total = entry_len(key.len(), value.len());
        if self.tail + total > self.storage.capacity() {
            return Err(Error::Full);
        }

        let mut body = [0u8; MAX_KEY_LEN + MAX_VALUE_LEN];
        body[..key.len()].copy_from_slice(key);
        body[key.len()..key.len() + value.len()].copy_from_slice(value);
        let n = key.len() + value.len();
        let crc = crc_of(key.len(), value.len(), &body[..n]);

        let mut buf = [0xFFu8; HEADER_LEN + MAX_KEY_LEN + MAX_VALUE_LEN + ALIGN as usize];
        buf[0..2].copy_from_slice(&MAGIC.to_le_bytes());
        buf[2] = key.len() as u8;
        buf[3] = value.len() as u8;
        buf[4..8].copy_from_slice(&crc.to_le_bytes());
        buf[HEADER_LEN..HEADER_LEN + n].copy_from_slice(&body[..n]);

        self.storage.write(self.tail, &buf[..total as usize])?;
        self.tail += total;
        Ok(())
    }

    /// Read the newest value for `key` into `out`, returning its length.
    pub fn get(&self, key: &[u8], out: &mut [u8]) -> Result<usize, Error> {
        // One read per entry, and the value carried out of the same read that
        // matched the key. The previous version read the header inside
        // `entry_at`, read it again here, read the key a third time and the
        // value a fourth — four flash operations per entry, each paying for
        // its own cache-off window, for a scan that visits every entry in the
        // log. Loading a 1904-byte blob out of fifteen chunked records took
        // 1.36 s on an ESP32, against the 183 ms of radio calibration the
        // record existed to avoid.
        let mut buf = [0u8; MAX_ENTRY];
        let mut found: Option<usize> = None;
        let mut value = [0u8; MAX_VALUE_LEN];
        let mut at = 0;

        // Forward, keeping the last match: newer entries are later, and the
        // log has no back-pointers to walk the other way.
        while let Some(e) = self.entry_into(at, &mut buf)? {
            if e.key_len == key.len() && &buf[HEADER_LEN..HEADER_LEN + e.key_len] == key {
                let from = HEADER_LEN + e.key_len;
                value[..e.val_len].copy_from_slice(&buf[from..from + e.val_len]);
                found = Some(e.val_len);
            }
            at += e.total;
        }

        let val_len = found.ok_or(Error::NotFound)?;
        // Checked after the scan, exactly as before: it is the *last* entry
        // for the key that decides, and an earlier one being too big for
        // `out` says nothing about the one that counts.
        if out.len() < val_len {
            return Err(Error::BufferTooSmall);
        }
        out[..val_len].copy_from_slice(&value[..val_len]);
        Ok(val_len)
    }

    /// Rewrite the log keeping only the newest entry for each key.
    ///
    /// # Why this exists
    ///
    /// The log is append-only: a `set` on an existing key appends a second
    /// entry and the old one stays. Nothing reclaimed it, so a store written
    /// to on every boot grew until `set` returned [`Error::Full`] — and the
    /// Wi-Fi driver, handed `ESP_ERR_NVS_NOT_ENOUGH_SPACE`, did not come
    /// back. Measured at +36 bytes and +116 us per boot before this existed.
    ///
    /// # The weaker guarantee, stated plainly
    ///
    /// esp-idf's `nvs_flash` and Zephyr's `subsys/fs/nvs` both compact by
    /// *sector rotation*: live entries are copied to a fresh sector before the
    /// old one is erased, so every entry is readable from flash at every
    /// instant and a power cut costs nothing.
    ///
    /// **This cannot do that**, because [`Storage`] has no per-sector erase —
    /// only [`Storage::erase_all`]. So the live set is copied into `scratch`,
    /// the whole store is erased, and the entries are written back. **A reset
    /// inside that window loses the store.** That is acceptable for what it
    /// holds today — an RF calibration, regenerable in ~183 ms — and would not
    /// be for anything that cannot be recomputed. Fixing it properly means
    /// adding `erase_sector` to [`Storage`] and rotating; the trait is the
    /// blocker, not the algorithm.
    ///
    /// # Scratch
    ///
    /// Must hold every *live* entry, not the whole log: distinct keys times
    /// `MAX_ENTRY`. Returns [`Error::Full`] without touching the store if it
    /// does not fit, so a caller that guessed too small loses nothing.
    ///
    /// Returns the number of bytes reclaimed.
    pub fn compact(&mut self, scratch: &mut [u8]) -> Result<u32, Error> {
        let before = self.tail;
        let mut live: usize = 0;

        // Forward, replacing in place: a later entry for a key supersedes an
        // earlier one, so the scratch always holds the newest of each.
        let mut buf = [0u8; MAX_ENTRY];
        let mut at = 0;
        while let Some(e) = self.entry_into(at, &mut buf)? {
            let total = e.total as usize;
            let key = &buf[HEADER_LEN..HEADER_LEN + e.key_len];
            if let Some(prev) = find_key(&scratch[..live], key) {
                // Drop the older copy, then append the newer below. Removing
                // rather than overwriting in place because the two entries can
                // be different lengths.
                let plen = encoded_len(&scratch[prev..]);
                scratch.copy_within(prev + plen..live, prev);
                live -= plen;
            }
            if live + total > scratch.len() {
                return Err(Error::Full);
            }
            scratch[live..live + total].copy_from_slice(&buf[..total]);
            live += total;
            at += e.total;
        }

        // Nothing superseded: every byte in the log is live, so erasing and
        // writing it back would put the same bytes in the same places at the
        // cost of one erase cycle. Checked here rather than left to the
        // caller, because the caller cannot know without doing this scan.
        if live as u32 == before {
            return Ok(0);
        }

        // Past this point the store is gone until the write-back finishes.
        self.storage.erase_all()?;
        self.tail = 0;
        // One entry per `write`, not one write for the whole live set. A
        // `Storage` is allowed to cap how much it takes at once -- the ESP32's
        // does, at 256 bytes, because it copies through a word-aligned scratch
        // array (`kernel::nvs`, `SCRATCH_WORDS`) -- and `set` never writes more
        // than a single entry, so that is the size the implementations are
        // built for. Both references copy per entry for the same reason:
        // esp-idf's `Page::copyItems` calls `writeEntry` once per entry
        // (`nvs_page.cpp:535`), and Zephyr's `nvs_gc` pairs one
        // `nvs_flash_block_move` with one `nvs_flash_ate_wrt` per entry
        // (`subsys/fs/nvs/nvs.c`, ~line 510), chunking even within an entry.
        let mut at = 0;
        while at < live {
            let n = encoded_len(&scratch[at..]);
            self.storage.write(at as u32, &scratch[at..at + n])?;
            at += n;
            // Advanced per entry so a failure part-way leaves the cursor on
            // what actually reached the flash rather than on the whole set.
            self.tail = at as u32;
        }
        Ok(before.saturating_sub(self.tail))
    }

    /// Is `key` present?
    pub fn contains(&self, key: &[u8]) -> Result<bool, Error> {
        let mut out = [0u8; MAX_VALUE_LEN];
        match self.get(key, &mut out) {
            Ok(_) => Ok(true),
            Err(Error::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// The checksum, over the lengths as well as the bytes.
///
/// Including the lengths is what stops a torn header from being believed: a
/// key length that survived and a value length that did not still fails.
fn crc_of(key_len: usize, val_len: usize, body: &[u8]) -> u32 {
    let mut seed = [0u8; 2];
    seed[0] = key_len as u8;
    seed[1] = val_len as u8;
    let a = crc32(&seed);
    // Fold the body in after the lengths, so order matters.
    let b = crc32(body);
    a ^ b.rotate_left(1)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 4096;

    /// The most one `write` may carry.
    ///
    /// The ESP32's `Storage` copies through a 64-word scratch array and
    /// refuses anything longer (`kernel/src/nvs.rs`, `SCRATCH_WORDS`). The
    /// fake did not model that, so `compact` writing the whole live set in one
    /// call passed every host test and returned `Io` on the board the first
    /// time two keys were live. A cap here is what makes that a test failure
    /// instead of a hardware one.
    const MAX_WRITE: usize = 256;

    /// Flash that behaves like flash: erased is 0xFF, a write can only clear
    /// bits, and no single write may exceed [`MAX_WRITE`].
    pub(crate) struct Fake {
        bytes: [u8; CAP],
        /// Stop writing after this many bytes, to model losing power.
        stop_after: Option<usize>,
        /// How many times the store has been erased. Flash wears out, so a
        /// compaction that erases when it had nothing to reclaim is a real
        /// cost and worth a test.
        pub(crate) erases: u32,
    }

    impl Fake {
        pub(crate) fn new() -> Self {
            Self { bytes: [0xFF; CAP], stop_after: None, erases: 0 }
        }
    }

    impl Storage for Fake {
        const SECTOR_SIZE: u32 = 4096;
        fn capacity(&self) -> u32 {
            CAP as u32
        }
        fn read(&self, offset: u32, buf: &mut [u8]) -> Result<(), Error> {
            let o = offset as usize;
            if o + buf.len() > CAP {
                return Err(Error::Io);
            }
            buf.copy_from_slice(&self.bytes[o..o + buf.len()]);
            Ok(())
        }
        fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Error> {
            let o = offset as usize;
            if o + data.len() > CAP || data.len() > MAX_WRITE {
                return Err(Error::Io);
            }
            let limit = self.stop_after.unwrap_or(data.len()).min(data.len());
            for (i, &b) in data.iter().take(limit).enumerate() {
                // Flash ANDs; it cannot set a bit back to 1.
                self.bytes[o + i] &= b;
            }
            Ok(())
        }
        fn erase_all(&mut self) -> Result<(), Error> {
            self.bytes = [0xFF; CAP];
            self.erases += 1;
            Ok(())
        }
    }

    fn store() -> Store<Fake> {
        Store::open(Fake::new()).unwrap()
    }

    #[test]
    fn a_value_comes_back() {
        let mut s = store();
        s.set(b"wifi.ssid", b"kitchen").unwrap();
        let mut out = [0u8; 64];
        let n = s.get(b"wifi.ssid", &mut out).unwrap();
        assert_eq!(&out[..n], b"kitchen");
    }

    #[test]
    fn an_absent_key_is_not_found() {
        let s = store();
        let mut out = [0u8; 8];
        assert_eq!(s.get(b"nope", &mut out).unwrap_err(), Error::NotFound);
        assert!(!s.contains(b"nope").unwrap());
    }

    #[test]
    fn the_newest_value_wins() {
        let mut s = store();
        s.set(b"k", b"one").unwrap();
        s.set(b"k", b"two").unwrap();
        s.set(b"k", b"three").unwrap();
        let mut out = [0u8; 16];
        let n = s.get(b"k", &mut out).unwrap();
        assert_eq!(&out[..n], b"three", "an earlier value shadowed the newest");
    }

    #[test]
    fn keys_do_not_collide_by_prefix() {
        // A length-blind comparison would make "ab" find "abc".
        let mut s = store();
        s.set(b"ab", b"short").unwrap();
        s.set(b"abc", b"long").unwrap();
        let mut out = [0u8; 16];
        let n = s.get(b"ab", &mut out).unwrap();
        assert_eq!(&out[..n], b"short");
        let n = s.get(b"abc", &mut out).unwrap();
        assert_eq!(&out[..n], b"long");
    }

    #[test]
    fn a_reopened_store_finds_what_was_written() {
        let mut s = store();
        s.set(b"a", b"1").unwrap();
        s.set(b"b", b"22").unwrap();
        let used = s.used();

        let reopened = Store::open(s.storage).unwrap();
        assert_eq!(reopened.used(), used, "the scan found a different end");
        let mut out = [0u8; 8];
        assert_eq!(reopened.get(b"b", &mut out).unwrap(), 2);
        assert_eq!(&out[..2], b"22");
    }

    #[test]
    fn an_empty_value_is_allowed_and_distinct_from_absent() {
        let mut s = store();
        s.set(b"flag", b"").unwrap();
        let mut out = [0u8; 4];
        assert_eq!(s.get(b"flag", &mut out).unwrap(), 0);
        assert!(s.contains(b"flag").unwrap());
    }

    #[test]
    fn oversized_keys_and_values_are_refused() {
        let mut s = store();
        assert_eq!(s.set(b"", b"v").unwrap_err(), Error::BadKey);
        assert_eq!(s.set(&[b'k'; MAX_KEY_LEN + 1], b"v").unwrap_err(), Error::BadKey);
        assert_eq!(
            s.set(b"k", &[0u8; MAX_VALUE_LEN + 1]).unwrap_err(),
            Error::BadValue
        );
        // The boundaries themselves must be accepted.
        assert!(s.set(&[b'k'; MAX_KEY_LEN], &[0u8; MAX_VALUE_LEN]).is_ok());
    }

    #[test]
    fn a_full_store_says_so_rather_than_wrapping() {
        let mut s = store();
        let mut n = 0;
        loop {
            match s.set(b"key", &[0u8; MAX_VALUE_LEN]) {
                Ok(()) => n += 1,
                Err(Error::Full) => break,
                Err(e) => panic!("unexpected {e:?}"),
            }
            assert!(n < 1000, "the store never filled");
        }
        assert!(n > 0, "nothing fitted at all");
        // And it stays refused, rather than the tail wrapping to the start
        // and eating the entries already there.
        assert_eq!(s.set(b"key", &[0u8; MAX_VALUE_LEN]).unwrap_err(), Error::Full);
        let mut out = [0u8; MAX_VALUE_LEN];
        assert!(s.get(b"key", &mut out).is_ok(), "a full store lost its data");
    }

    #[test]
    fn a_buffer_too_small_is_refused_rather_than_truncating() {
        let mut s = store();
        s.set(b"k", b"0123456789").unwrap();
        let mut out = [0u8; 4];
        assert_eq!(s.get(b"k", &mut out).unwrap_err(), Error::BufferTooSmall);
    }

    /// The property the whole design exists for: losing power partway through
    /// a write must not damage anything already stored.
    ///
    /// Tears at *every* byte offset of the final write, not one chosen offset.
    /// A format that survives being cut in the payload but not in the header
    /// passes a single-offset test and loses data in the field.
    #[test]
    fn a_torn_write_never_damages_an_earlier_key() {
        let doomed = entry_len(5, 9) as usize;
        for cut in 0..doomed {
            let mut s = store();
            s.set(b"first", b"keep-me").unwrap();
            s.set(b"second", b"also-keep").unwrap();
            let before = s.used();

            // Power goes away `cut` bytes into the third write.
            s.storage.stop_after = Some(cut);
            let _ = s.set(b"third", b"truncated");
            s.storage.stop_after = None;

            // Reboot.
            let reopened = Store::open(s.storage).unwrap();
            let mut out = [0u8; 32];
            let n = reopened.get(b"first", &mut out).unwrap();
            assert_eq!(&out[..n], b"keep-me", "cut at {cut} damaged the first key");
            let n = reopened.get(b"second", &mut out).unwrap();
            assert_eq!(&out[..n], b"also-keep", "cut at {cut} damaged the second key");

            // The torn entry is either wholly there or wholly ignored; a
            // half-entry must never be readable.
            if let Ok(n) = reopened.get(b"third", &mut out) {
                assert_eq!(&out[..n], b"truncated", "cut at {cut} gave a partial value");
                assert_eq!(reopened.used(), before + doomed as u32);
            } else {
                assert_eq!(reopened.used(), before, "cut at {cut} left the tail adrift");
            }
        }
    }

    /// A torn write must not strand the space after it either — the next boot
    /// has to be able to keep writing.
    #[test]
    fn the_store_is_still_writable_after_a_torn_write() {
        let mut s = store();
        s.set(b"a", b"1").unwrap();
        s.storage.stop_after = Some(3);
        let _ = s.set(b"b", b"2");
        s.storage.stop_after = None;

        let mut reopened = Store::open(s.storage).unwrap();
        reopened.set(b"c", b"3").unwrap();
        let mut out = [0u8; 4];
        assert_eq!(reopened.get(b"c", &mut out).unwrap(), 1);
        assert_eq!(&out[..1], b"3");
        assert_eq!(reopened.get(b"a", &mut out).unwrap(), 1);
        assert_eq!(&out[..1], b"1");
    }

    #[test]
    fn a_corrupt_header_does_not_send_the_scan_wandering() {
        // A believable magic with a nonsense length. Trusting the length would
        // step the scan into the middle of the next entry and read rubbish as
        // a key.
        let mut s = store();
        s.set(b"good", b"value").unwrap();
        let at = s.used();
        s.storage.bytes[at as usize] = MAGIC.to_le_bytes()[0];
        s.storage.bytes[at as usize + 1] = MAGIC.to_le_bytes()[1];
        s.storage.bytes[at as usize + 2] = 200; // key_len, far over the limit

        let reopened = Store::open(s.storage).unwrap();
        assert_eq!(reopened.used(), at, "the scan believed a corrupt length");
        let mut out = [0u8; 8];
        assert_eq!(reopened.get(b"good", &mut out).unwrap(), 5);
    }

    #[test]
    fn a_flipped_payload_bit_is_caught() {
        let mut s = store();
        s.set(b"k", b"value").unwrap();
        let at = HEADER_LEN + 1; // inside the value
        s.storage.bytes[at] ^= 0x01;

        let reopened = Store::open(s.storage).unwrap();
        // The entry is rejected wholesale rather than returning a wrong value.
        assert_eq!(reopened.used(), 0);
        let mut out = [0u8; 8];
        assert_eq!(reopened.get(b"k", &mut out).unwrap_err(), Error::NotFound);
    }

    #[test]
    fn the_magic_is_not_what_erased_flash_reads_as() {
        // 0xFFFF would make every erased byte look like the start of an entry.
        assert_ne!(MAGIC, 0xFFFF);
        assert_ne!(MAGIC, 0x0000);
    }

    #[test]
    fn entries_stay_word_aligned() {
        for (k, v) in [(1, 0), (1, 1), (3, 2), (32, 128), (5, 9)] {
            let len = entry_len(k, v);
            assert_eq!(len % ALIGN, 0, "key {k} value {v} left the tail misaligned");
            assert!(len >= (HEADER_LEN + k + v) as u32);
        }
    }

    #[test]
    fn the_checksum_notices_lengths_swapping() {
        // key_len and val_len both survive a tear that swaps them only if the
        // checksum covers them. Same bytes, different framing.
        let body = [1u8, 2, 3, 4];
        assert_ne!(crc_of(1, 3, &body), crc_of(3, 1, &body));
    }

    /// Flash that counts how many times it is asked to read.
    ///
    /// The bytes moved are not what a `get` costs on this hardware — the *call*
    /// is, because each one runs with the instruction cache off and the other
    /// core parked. So the number worth asserting is the call count.
    struct Counting {
        inner: Fake,
        reads: core::cell::Cell<u32>,
    }

    impl Storage for Counting {
        const SECTOR_SIZE: u32 = 4096;
        fn capacity(&self) -> u32 {
            self.inner.capacity()
        }
        fn read(&self, offset: u32, buf: &mut [u8]) -> Result<(), Error> {
            self.reads.set(self.reads.get() + 1);
            self.inner.read(offset, buf)
        }
        fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Error> {
            self.inner.write(offset, data)
        }
        fn erase_all(&mut self) -> Result<(), Error> {
            self.inner.erase_all()
        }
    }

    #[test]
    fn a_lookup_reads_each_entry_once() {
        // The fix this test exists for: `get` used to read the header inside
        // `entry_at`, read it again itself, read the key a third time and the
        // value a fourth. Four reads per entry, every entry, every lookup.
        //
        // One read per entry is the floor for a log with no index — you cannot
        // skip an entry without knowing its length, and its length is in it.
        let mut store = Store::open(Counting {
            inner: Fake::new(),
            reads: core::cell::Cell::new(0),
        })
        .expect("opens");

        const N: usize = 20;
        for i in 0..N {
            let key = [b'k', b'0' + (i / 10) as u8, b'0' + (i % 10) as u8];
            store.set(&key, b"value").expect("set");
        }

        // The last key, so the scan runs the full length of the log.
        let mut out = [0u8; 16];
        store.storage.reads.set(0);
        let n = store.get(b"k19", &mut out).expect("get");
        assert_eq!(&out[..n], b"value");

        let reads = store.storage.reads.get();
        // N entries plus the one that finds the end of the log.
        assert_eq!(reads, (N + 1) as u32, "expected one read per entry");
    }

    #[test]
    fn a_miss_costs_no_more_than_a_hit() {
        // A `NotFound` still has to walk the whole log — there is nowhere else
        // the entry could be — but it must not walk it twice.
        let mut store = Store::open(Counting {
            inner: Fake::new(),
            reads: core::cell::Cell::new(0),
        })
        .expect("opens");
        for i in 0..8u8 {
            store.set(&[b'k', b'0' + i], b"v").expect("set");
        }
        let mut out = [0u8; 8];
        store.storage.reads.set(0);
        assert_eq!(store.get(b"nope", &mut out), Err(Error::NotFound));
        assert_eq!(store.storage.reads.get(), 9);
    }

    #[test]
    fn a_value_too_big_for_the_caller_is_still_refused() {
        // `get` now carries the value out of the scan rather than re-reading
        // it at the end, so the size check moved. It must still fire, and it
        // must still be the *last* entry for the key that decides.
        let mut store = Store::open(Fake::new()).expect("opens");
        store.set(b"k", &[7u8; 64]).expect("set");
        let mut small = [0u8; 8];
        assert_eq!(store.get(b"k", &mut small), Err(Error::BufferTooSmall));

        // Superseded by something that does fit: the short one wins, because
        // it is the newer.
        store.set(b"k", b"ok").expect("set");
        let n = store.get(b"k", &mut small).expect("get");
        assert_eq!(&small[..n], b"ok");
    }
}

#[cfg(test)]
mod compaction_tests {
    use super::*;
    use crate::tests::Fake;

    fn store() -> Store<Fake> {
        Store::open(Fake::new()).expect("open")
    }

    #[test]
    fn compaction_keeps_the_newest_value_for_every_key() {
        let mut s = store();
        s.set(b"a", b"1").unwrap();
        s.set(b"b", b"one").unwrap();
        s.set(b"a", b"2").unwrap();
        s.set(b"a", b"3").unwrap();
        s.set(b"c", b"x").unwrap();
        let before = s.used();

        let mut scratch = [0u8; 1024];
        let reclaimed = s.compact(&mut scratch).expect("compact");
        assert!(reclaimed > 0, "two superseded copies of `a` should be gone");
        assert_eq!(s.used(), before - reclaimed);

        let mut out = [0u8; MAX_VALUE_LEN];
        assert_eq!(s.get(b"a", &mut out).unwrap(), 1);
        assert_eq!(&out[..1], b"3", "the newest value, not the first");
        assert_eq!(s.get(b"b", &mut out).unwrap(), 3);
        assert_eq!(&out[..3], b"one");
        assert_eq!(s.get(b"c", &mut out).unwrap(), 1);
        assert_eq!(&out[..1], b"x");
    }

    #[test]
    fn compaction_is_idempotent() {
        // A second pass over an already-compact log must reclaim nothing and
        // change nothing -- otherwise a caller that compacts on every boot
        // rewrites flash for no reason, which is wear for free.
        let mut s = store();
        s.set(b"k", b"v").unwrap();
        s.set(b"j", b"w").unwrap();
        let mut scratch = [0u8; 1024];
        s.compact(&mut scratch).unwrap();
        let after_first = s.used();
        assert_eq!(s.compact(&mut scratch).unwrap(), 0);
        assert_eq!(s.used(), after_first);
    }

    #[test]
    fn compaction_makes_a_full_store_writable_again() {
        // The failure this exists for: `set` returning `Full` on a store whose
        // live set is tiny, because every rewrite of one key appended.
        let mut s = store();
        let big = [b'x'; MAX_VALUE_LEN];
        let mut writes = 0;
        while s.set(b"same", &big).is_ok() {
            writes += 1;
            assert!(writes < 10_000, "the store should fill");
        }
        assert_eq!(s.set(b"same", &big), Err(Error::Full));

        let mut scratch = [0u8; 1024];
        assert!(s.compact(&mut scratch).unwrap() > 0);
        s.set(b"same", &big).expect("room again after compaction");

        let mut out = [0u8; MAX_VALUE_LEN];
        assert_eq!(s.get(b"same", &mut out).unwrap(), MAX_VALUE_LEN);
    }

    #[test]
    fn a_scratch_too_small_refuses_and_leaves_the_store_alone() {
        // Checked before the erase, so a caller that guessed wrong loses
        // nothing. This is the test that would fail if `erase_all` ever moved
        // above the scratch bounds check.
        let mut s = store();
        s.set(b"a", &[b'x'; MAX_VALUE_LEN]).unwrap();
        s.set(b"b", &[b'y'; MAX_VALUE_LEN]).unwrap();
        let before = s.used();

        let mut tiny = [0u8; 64];
        assert_eq!(s.compact(&mut tiny), Err(Error::Full));
        assert_eq!(s.used(), before, "the store must be untouched");
        let mut out = [0u8; MAX_VALUE_LEN];
        assert_eq!(s.get(b"a", &mut out).unwrap(), MAX_VALUE_LEN);
    }

    #[test]
    fn a_live_set_wider_than_one_write_still_goes_back() {
        // The one that was missing. Five full-length values are ~700 bytes
        // live, well past what a single `Storage::write` may carry, so a
        // write-back that hands over the whole set at once fails with `Io`
        // *after* the erase -- which is exactly what the board did.
        let mut s = store();
        let keys: [&[u8]; 5] = [b"k0", b"k1", b"k2", b"k3", b"k4"];
        for (i, k) in keys.iter().enumerate() {
            s.set(k, &[b'0' + i as u8; MAX_VALUE_LEN]).unwrap();
            s.set(k, &[b'a' + i as u8; MAX_VALUE_LEN]).unwrap();
        }
        let before = s.used();

        let mut scratch = [0u8; 2048];
        let reclaimed = s.compact(&mut scratch).unwrap();
        assert!(reclaimed > 0);
        assert_eq!(s.used(), before - reclaimed);

        let mut out = [0u8; MAX_VALUE_LEN];
        for (i, k) in keys.iter().enumerate() {
            assert_eq!(s.get(k, &mut out).unwrap(), MAX_VALUE_LEN, "{:?} survived", k);
            assert_eq!(out[0], b'a' + i as u8, "and kept its newest value");
        }
    }

    #[test]
    fn nothing_to_reclaim_costs_no_erase() {
        // Zephyr's `nvs_gc` is driven by the write pointer crossing a sector,
        // not by anything knowing in advance whether there is garbage to
        // collect, so the no-garbage case has to be cheap. Erasing to write
        // the same bytes back would spend a flash cycle for nothing.
        let mut s = store();
        s.set(b"a", b"1").unwrap();
        s.set(b"b", b"2").unwrap();
        let before = s.used();
        let erases = s.storage.erases;

        let mut scratch = [0u8; 512];
        assert_eq!(s.compact(&mut scratch).unwrap(), 0);
        assert_eq!(s.used(), before);
        assert_eq!(s.storage.erases, erases, "no erase when nothing is superseded");
    }

    #[test]
    fn compacting_an_empty_store_does_nothing() {
        let mut s = store();
        let mut scratch = [0u8; 64];
        assert_eq!(s.compact(&mut scratch).unwrap(), 0);
        assert_eq!(s.used(), 0);
    }
}
