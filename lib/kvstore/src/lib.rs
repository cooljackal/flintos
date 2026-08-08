// SPDX-License-Identifier: Apache-2.0

//! A key/value store that survives being interrupted.
//!
//! Nothing in Flint outlives a reboot today. Calibration constants, a device
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
        let mut at = 0;
        loop {
            match self.entry_at(at)? {
                Some(len) => at += len,
                None => return Ok(at),
            }
        }
    }

    /// Length of the valid entry at `at`, or `None` if there is not one.
    fn entry_at(&self, at: u32) -> Result<Option<u32>, Error> {
        if at + HEADER_LEN as u32 > self.storage.capacity() {
            return Ok(None);
        }
        let mut header = [0u8; HEADER_LEN];
        self.storage.read(at, &mut header)?;

        let magic = u16::from_le_bytes([header[0], header[1]]);
        if magic != MAGIC {
            return Ok(None);
        }
        let key_len = header[2] as usize;
        let val_len = header[3] as usize;
        if key_len == 0 || key_len > MAX_KEY_LEN || val_len > MAX_VALUE_LEN {
            // A corrupt header with a believable magic. Treat it as the end
            // rather than trusting the lengths and walking off into whatever
            // follows.
            return Ok(None);
        }
        let total = entry_len(key_len, val_len);
        if at + total > self.storage.capacity() {
            return Ok(None);
        }

        let stored_crc = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
        let mut body = [0u8; MAX_KEY_LEN + MAX_VALUE_LEN];
        let n = key_len + val_len;
        self.storage.read(at + HEADER_LEN as u32, &mut body[..n])?;
        if crc_of(key_len, val_len, &body[..n]) != stored_crc {
            return Ok(None);
        }
        Ok(Some(total))
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
        let mut found: Option<(u32, usize, usize)> = None;
        let mut at = 0;
        // Forward, keeping the last match: newer entries are later, and the
        // log has no back-pointers to walk the other way.
        while let Some(len) = self.entry_at(at)? {
            let mut header = [0u8; HEADER_LEN];
            self.storage.read(at, &mut header)?;
            let key_len = header[2] as usize;
            let val_len = header[3] as usize;
            if key_len == key.len() {
                let mut k = [0u8; MAX_KEY_LEN];
                self.storage.read(at + HEADER_LEN as u32, &mut k[..key_len])?;
                if &k[..key_len] == key {
                    found = Some((at, key_len, val_len));
                }
            }
            at += len;
        }
        let (at, key_len, val_len) = found.ok_or(Error::NotFound)?;
        if out.len() < val_len {
            return Err(Error::BufferTooSmall);
        }
        self.storage
            .read(at + HEADER_LEN as u32 + key_len as u32, &mut out[..val_len])?;
        Ok(val_len)
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

    /// Flash that behaves like flash: erased is 0xFF, and a write can only
    /// clear bits.
    struct Fake {
        bytes: [u8; CAP],
        /// Stop writing after this many bytes, to model losing power.
        stop_after: Option<usize>,
    }

    impl Fake {
        fn new() -> Self {
            Self { bytes: [0xFF; CAP], stop_after: None }
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
            if o + data.len() > CAP {
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
}
