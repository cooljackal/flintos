// SPDX-License-Identifier: Apache-2.0

//! `nvs_*` — the blob's key/value store, over `lib/kvstore`.
//!
//! Twelve table entries. The Wi-Fi driver keeps small things here: the country
//! code, the stored AP record, a handful of flags. It is not where the
//! calibration blob goes — that has its own module, because it is 1904 bytes
//! and needs a commit point (see [`crate::calibration`]).
//!
//! # Namespaces, which kvstore has not got
//!
//! esp-idf's NVS is namespaced: `nvs_open("wifi", ..)` gives a handle, and
//! every key is scoped to it. `kvstore` is one flat keyspace, so the namespace
//! is folded into the key as `<namespace>:<key>`.
//!
//! That is not free, and the arithmetic is the reason the limits below exist:
//! `kvstore`'s [`MAX_KEY_LEN`] is 32, and IDF allows 15 characters each of
//! namespace and key, which is 15 + 1 + 15 = 31. It fits, with a byte spare,
//! and [`MAX_NAME_LEN`] enforces the halves rather than letting a long name
//! silently collide with a different one after truncation.
//!
//! # Types are checked, because IDF checks them
//!
//! Every value carries a one-byte tag. `nvs_get_u8` on something stored by
//! `nvs_set_u16` returns `ESP_ERR_NVS_TYPE_MISMATCH`, as it does in IDF —
//! without the tag it would return the first byte of the `u16` and look like
//! a working read. The tag costs one byte per record and removes a whole
//! class of silent wrong answers.
//!
//! # What is deliberately not implemented
//!
//! `nvs_commit` is a no-op. `kvstore` writes through — a `set` has reached
//! flash by the time it returns — so there is nothing to flush, and returning
//! `ESP_OK` is the truthful answer rather than a stub's shrug. `nvs_close`
//! releases the handle and nothing else, since there is no cached state to
//! write back.
//!
//! Error values are esp-idf v4.4's, read from `components/nvs_flash/include/
//! nvs.h` at the tag `tools/fetch-blobs.sh` pins.

#[cfg(target_os = "none")]
use core::ffi::c_void;
use core::ffi::c_char;

use kvstore::{Error as KvError, Storage, Store, MAX_KEY_LEN, MAX_VALUE_LEN};

// ── esp_err_t values, from nvs.h at v4.4 ────────────────────────────────────

pub const ESP_OK: i32 = 0;
pub const ESP_ERR_NVS_NOT_FOUND: i32 = 0x1102;
pub const ESP_ERR_NVS_TYPE_MISMATCH: i32 = 0x1103;
pub const ESP_ERR_NVS_READ_ONLY: i32 = 0x1104;
pub const ESP_ERR_NVS_NOT_ENOUGH_SPACE: i32 = 0x1105;
pub const ESP_ERR_NVS_INVALID_NAME: i32 = 0x1106;
pub const ESP_ERR_NVS_INVALID_HANDLE: i32 = 0x1107;
pub const ESP_ERR_NVS_INVALID_LENGTH: i32 = 0x110c;
pub const ESP_ERR_NVS_VALUE_TOO_LONG: i32 = 0x110e;

/// Longest namespace or key, each. IDF's own limit, and what makes the
/// composite key fit `kvstore`'s.
pub const MAX_NAME_LEN: usize = 15;

const _: () = assert!(
    MAX_NAME_LEN * 2 < MAX_KEY_LEN,
    "namespace + ':' + key must fit one kvstore key"
);

/// Type tags. Values, not an ordering we may renumber: they are written to
/// flash and read back by a later boot.
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
const TAG_I8: u8 = 1;
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
const TAG_U8: u8 = 2;
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
const TAG_U16: u8 = 3;
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
const TAG_BLOB: u8 = 4;

/// Largest blob, after the tag byte.
pub const MAX_BLOB_LEN: usize = MAX_VALUE_LEN - 1;

// ── Handles ─────────────────────────────────────────────────────────────────

/// Namespaces open at once. The Wi-Fi driver opens one ("nvs.net80211") and
/// the PHY another; four is slack.
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
const MAX_HANDLES: usize = 4;

#[derive(Clone, Copy)]
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
struct Namespace {
    /// The name, padded. Empty length means the slot is free.
    name: [u8; MAX_NAME_LEN],
    len: usize,
    /// False for `NVS_READONLY`, so a write can be refused as IDF would.
    writable: bool,
}

impl Namespace {
    #[cfg_attr(not(target_os = "none"), allow(dead_code))]
    const FREE: Self = Namespace { name: [0; MAX_NAME_LEN], len: 0, writable: false };
}

// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
static HANDLES: kernel::smp::Spinlock<[Namespace; MAX_HANDLES]> =
    kernel::smp::Spinlock::new([Namespace::FREE; MAX_HANDLES]);

/// `NVS_READWRITE`, from `nvs_open_mode_t`. `NVS_READONLY` is 0.
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
const NVS_READWRITE: u32 = 1;

// ── Key composition and encoding: the testable half ─────────────────────────

/// Build `<namespace>:<key>` into `out`, or `None` if either half is too long
/// or empty.
///
/// Refusing rather than truncating: two namespaces sharing the first fifteen
/// characters would otherwise become one, and the symptom is one driver
/// reading another's settings.
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
fn compose_key<'a>(ns: &[u8], key: &[u8], out: &'a mut [u8; MAX_KEY_LEN]) -> Option<&'a [u8]> {
    if ns.is_empty() || key.is_empty() || ns.len() > MAX_NAME_LEN || key.len() > MAX_NAME_LEN {
        return None;
    }
    out[..ns.len()].copy_from_slice(ns);
    out[ns.len()] = b':';
    let end = ns.len() + 1 + key.len();
    out[ns.len() + 1..end].copy_from_slice(key);
    Some(&out[..end])
}

/// A C string as bytes, up to `max`, or `None` if null or unterminated within
/// the limit.
///
/// # Safety
/// `p` must be a valid C string or null.
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
unsafe fn c_str<'a>(p: *const c_char, max: usize) -> Option<&'a [u8]> {
    if p.is_null() {
        return None;
    }
    let bytes = p as *const u8;
    for i in 0..=max {
        if unsafe { *bytes.add(i) } == 0 {
            return Some(unsafe { core::slice::from_raw_parts(bytes, i) });
        }
    }
    None
}

/// Map a `kvstore` failure onto the `esp_err_t` IDF would return.
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
fn map_err(e: KvError) -> i32 {
    match e {
        KvError::NotFound => ESP_ERR_NVS_NOT_FOUND,
        KvError::Full => ESP_ERR_NVS_NOT_ENOUGH_SPACE,
        KvError::BadKey => ESP_ERR_NVS_INVALID_NAME,
        KvError::BadValue => ESP_ERR_NVS_VALUE_TOO_LONG,
        // The caller's buffer being too small is IDF's INVALID_LENGTH, and a
        // storage refusal has no closer equivalent -- IDF would surface a
        // flash error, which the blob treats as "unusable" either way.
        KvError::BufferTooSmall | KvError::Io => ESP_ERR_NVS_INVALID_LENGTH,
    }
}

/// Store `bytes` under `ns:key` with `tag`.
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
fn put<S: Storage>(store: &mut Store<S>, ns: &[u8], key: &[u8], tag: u8, bytes: &[u8]) -> i32 {
    if bytes.len() > MAX_BLOB_LEN {
        return ESP_ERR_NVS_VALUE_TOO_LONG;
    }
    let mut kbuf = [0u8; MAX_KEY_LEN];
    let Some(k) = compose_key(ns, key, &mut kbuf) else {
        return ESP_ERR_NVS_INVALID_NAME;
    };
    let mut vbuf = [0u8; MAX_VALUE_LEN];
    vbuf[0] = tag;
    vbuf[1..1 + bytes.len()].copy_from_slice(bytes);
    match store.set(k, &vbuf[..1 + bytes.len()]) {
        Ok(()) => ESP_OK,
        Err(KvError::Full) => {
            // Compact and retry once. The log is append-only, so a store the
            // driver rewrites on every boot fills with superseded copies of a
            // handful of keys -- and `Full` reaching the driver as
            // `ESP_ERR_NVS_NOT_ENOUGH_SPACE` is what hung init.
            if !compact(store) {
                return map_err(KvError::Full);
            }
            match store.set(k, &vbuf[..1 + bytes.len()]) {
                Ok(()) => ESP_OK,
                Err(e) => map_err(e),
            }
        }
        Err(e) => map_err(e),
    }
}

/// Bytes of scratch for a compaction.
///
/// Must hold the live set -- distinct keys times `kvstore::MAX_ENTRY`, not the
/// whole log. The driver keeps a handful of keys and the RF calibration is
/// fifteen chunks plus three, so 4 KiB is roughly twice what has ever been
/// live.
const COMPACT_SCRATCH: usize = 4096;

/// Reclaim superseded entries. `false` if it could not be attempted.
///
/// The scratch comes from the heap rather than `.bss`: this runs at most once
/// per boot, and 4 KiB of permanently reserved DRAM to serve it would cost
/// more than the radio heap does.
///
/// **A reset during the rewrite loses the store** -- see
/// `kvstore::Store::compact`, which explains why and what it would take to fix
/// properly. Everything here is regenerable.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
fn compact<S: Storage>(store: &mut Store<S>) -> bool {
    let scratch = unsafe { kernel::heap::alloc(COMPACT_SCRATCH, 4) };
    if scratch.is_null() {
        api::log_error!("radio: nvs is full and there is no heap to compact with");
        return false;
    }
    let buf = unsafe { core::slice::from_raw_parts_mut(scratch, COMPACT_SCRATCH) };
    let result = store.compact(buf);
    unsafe { kernel::heap::free(scratch, kernel::heap::Caps::Internal) };
    match result {
        Ok(freed) => {
            api::log_info!("radio: nvs compacted, {} bytes reclaimed", freed);
            true
        }
        Err(e) => {
            api::log_error!("radio: nvs compaction failed: {:?}", e);
            false
        }
    }
}

/// Fetch `ns:key`, checking the tag. Returns the payload length in `out`.
// Used by the C shims, which are target-only; the tests exercise them on a
// host. Neither `cfg(test)` nor `cfg(target_os)` alone describes that.
#[cfg_attr(not(target_os = "none"), allow(dead_code))]
fn fetch<S: Storage>(
    store: &Store<S>,
    ns: &[u8],
    key: &[u8],
    want: u8,
    out: &mut [u8; MAX_VALUE_LEN],
) -> Result<usize, i32> {
    let mut kbuf = [0u8; MAX_KEY_LEN];
    let Some(k) = compose_key(ns, key, &mut kbuf) else {
        return Err(ESP_ERR_NVS_INVALID_NAME);
    };
    let mut raw = [0u8; MAX_VALUE_LEN];
    let n = store.get(k, &mut raw).map_err(map_err)?;
    if n == 0 {
        return Err(ESP_ERR_NVS_INVALID_LENGTH);
    }
    if raw[0] != want {
        return Err(ESP_ERR_NVS_TYPE_MISMATCH);
    }
    out[..n - 1].copy_from_slice(&raw[1..n]);
    Ok(n - 1)
}

// ── The one store ───────────────────────────────────────────────────────────
//
// `kvstore` is an append-only log with a cursor, so **two `Store` values over
// the same region is corruption**, not merely waste: each believes the free
// space starts where its own last write ended. The radio has two users -- this
// module and `crate::calibration` -- so there is one store here and both go
// through it.

#[cfg(target_os = "none")]
static STORE: kernel::smp::Spinlock<Option<Store<kernel::nvs::FlashStorage>>> =
    kernel::smp::Spinlock::new(None);

/// Open the `nvs` partition. Call once, before the blob starts.
///
/// # Safety
/// Takes ownership of the `nvs` partition's flash region.
#[cfg(target_os = "none")]
pub unsafe fn init() -> bool {
    let storage = unsafe { kernel::nvs::FlashStorage::nvs() };
    match Store::open(storage) {
        Ok(s) => {
            STORE.with(|slot| *slot = Some(s));
            true
        }
        Err(e) => {
            api::log_error!("radio: could not open the nvs partition: {:?}", e);
            false
        }
    }
}

/// Run `f` against the shared store, or return `absent` if [`init`] has not
/// run.
///
/// `crate::calibration` is the other caller: its `load` and `save` take a
/// `&Store`, so they compose through here rather than opening a second one.
#[cfg(target_os = "none")]
pub fn with_store<R>(f: impl FnOnce(&mut Store<kernel::nvs::FlashStorage>) -> R, absent: R) -> R {
    STORE.with(|slot| match slot {
        Some(s) => f(s),
        None => absent,
    })
}

/// The namespace behind a handle, if it is open.
#[cfg(target_os = "none")]
fn namespace_of(handle: u32) -> Option<([u8; MAX_NAME_LEN], usize, bool)> {
    let i = handle.checked_sub(1)? as usize;
    if i >= MAX_HANDLES {
        return None;
    }
    HANDLES.with(|t| {
        let n = t[i];
        if n.len == 0 {
            None
        } else {
            Some((n.name, n.len, n.writable))
        }
    })
}

/// `nvs_open(name, mode, out_handle)`.
///
/// Handles are 1-based so that 0 is never valid: the blob zeroes its handle
/// variable, and a 0-based scheme would make an uninitialised handle address
/// the first namespace.
///
/// # Safety
/// `name` is a C string; `out` receives a `nvs_handle_t`. Called by the blob.
#[no_mangle]
#[cfg(target_os = "none")]
pub unsafe extern "C" fn nvs_open(name: *const c_char, mode: u32, out: *mut c_void) -> i32 {
    let Some(ns) = (unsafe { c_str(name, MAX_NAME_LEN) }) else {
        return ESP_ERR_NVS_INVALID_NAME;
    };
    if ns.is_empty() {
        return ESP_ERR_NVS_INVALID_NAME;
    }
    let handle = HANDLES.with(|t| {
        // A second open of the same name returns the existing handle, as
        // IDF's does, rather than consuming another slot.
        for (i, slot) in t.iter().enumerate() {
            if slot.len == ns.len() && slot.name[..slot.len] == *ns {
                return Some(i + 1);
            }
        }
        let i = t.iter().position(|s| s.len == 0)?;
        let mut name = [0u8; MAX_NAME_LEN];
        name[..ns.len()].copy_from_slice(ns);
        t[i] = Namespace { name, len: ns.len(), writable: mode == NVS_READWRITE };
        Some(i + 1)
    });
    match handle {
        Some(h) => {
            if !out.is_null() {
                unsafe { (out as *mut u32).write(h as u32) };
            }
            ESP_OK
        }
        None => {
            api::log_error!("radio: no free NVS handle; MAX_HANDLES is {}", MAX_HANDLES);
            ESP_ERR_NVS_NOT_ENOUGH_SPACE
        }
    }
}

/// `nvs_close(handle)`. Nothing is cached, so there is nothing to write back.
///
/// # Safety
/// Called by the blob.
#[no_mangle]
#[cfg(target_os = "none")]
pub unsafe extern "C" fn nvs_close(handle: u32) {
    if let Some(i) = handle.checked_sub(1) {
        let i = i as usize;
        if i < MAX_HANDLES {
            HANDLES.with(|t| t[i] = Namespace::FREE);
        }
    }
}

/// `nvs_commit(handle)`.
///
/// A no-op returning `ESP_OK`, and truthfully so: `kvstore` writes through, so
/// a `set` has reached flash before it returns and nothing is buffered.
///
/// # Safety
/// Called by the blob.
#[no_mangle]
#[cfg(target_os = "none")]
pub unsafe extern "C" fn nvs_commit(handle: u32) -> i32 {
    match namespace_of(handle) {
        Some(_) => ESP_OK,
        None => ESP_ERR_NVS_INVALID_HANDLE,
    }
}

/// The body every setter shares.
#[cfg(target_os = "none")]
fn set_tagged(handle: u32, key: *const c_char, tag: u8, bytes: &[u8]) -> i32 {
    let Some((ns, len, writable)) = namespace_of(handle) else {
        return ESP_ERR_NVS_INVALID_HANDLE;
    };
    if !writable {
        return ESP_ERR_NVS_READ_ONLY;
    }
    let Some(k) = (unsafe { c_str(key, MAX_NAME_LEN) }) else {
        return ESP_ERR_NVS_INVALID_NAME;
    };
    with_store(|s| put(s, &ns[..len], k, tag, bytes), ESP_ERR_NVS_NOT_FOUND)
}

/// The body every getter shares. Returns the payload length.
#[cfg(target_os = "none")]
fn get_tagged(
    handle: u32,
    key: *const c_char,
    tag: u8,
    out: &mut [u8; MAX_VALUE_LEN],
) -> Result<usize, i32> {
    let Some((ns, len, _)) = namespace_of(handle) else {
        return Err(ESP_ERR_NVS_INVALID_HANDLE);
    };
    let Some(k) = (unsafe { c_str(key, MAX_NAME_LEN) }) else {
        return Err(ESP_ERR_NVS_INVALID_NAME);
    };
    with_store(|s| fetch(s, &ns[..len], k, tag, out), Err(ESP_ERR_NVS_NOT_FOUND))
}

macro_rules! scalar {
    ($set:ident, $get:ident, $ty:ty, $tag:expr, $n:expr) => {
        /// # Safety
        /// Called by the blob.
        #[no_mangle]
        #[cfg(target_os = "none")]
        pub unsafe extern "C" fn $set(handle: u32, key: *const c_char, value: $ty) -> i32 {
            set_tagged(handle, key, $tag, &value.to_le_bytes())
        }

        /// # Safety
        /// `out` receives the value. Called by the blob.
        #[no_mangle]
        #[cfg(target_os = "none")]
        pub unsafe extern "C" fn $get(handle: u32, key: *const c_char, out: *mut $ty) -> i32 {
            if out.is_null() {
                return ESP_ERR_NVS_INVALID_LENGTH;
            }
            let mut buf = [0u8; MAX_VALUE_LEN];
            match get_tagged(handle, key, $tag, &mut buf) {
                Ok(n) if n == $n => {
                    let mut le = [0u8; $n];
                    le.copy_from_slice(&buf[..n]);
                    unsafe { out.write(<$ty>::from_le_bytes(le)) };
                    ESP_OK
                }
                Ok(_) => ESP_ERR_NVS_INVALID_LENGTH,
                Err(e) => e,
            }
        }
    };
}

scalar!(nvs_set_i8, nvs_get_i8, i8, TAG_I8, 1);
scalar!(nvs_set_u8, nvs_get_u8, u8, TAG_U8, 1);
scalar!(nvs_set_u16, nvs_get_u16, u16, TAG_U16, 2);

/// `nvs_set_blob(handle, key, value, length)`.
///
/// # Safety
/// `value` must be readable for `length` bytes. Called by the blob.
#[no_mangle]
#[cfg(target_os = "none")]
pub unsafe extern "C" fn nvs_set_blob(
    handle: u32,
    key: *const c_char,
    value: *const c_void,
    length: usize,
) -> i32 {
    if value.is_null() {
        return ESP_ERR_NVS_INVALID_LENGTH;
    }
    if length > MAX_BLOB_LEN {
        return ESP_ERR_NVS_VALUE_TOO_LONG;
    }
    let bytes = unsafe { core::slice::from_raw_parts(value as *const u8, length) };
    set_tagged(handle, key, TAG_BLOB, bytes)
}

/// `nvs_get_blob(handle, key, out_value, length)`.
///
/// Implements IDF's two-call idiom: a null `out_value` asks for the size,
/// which is written to `*length` with `ESP_OK`. A buffer too small gets the
/// required size back and `ESP_ERR_NVS_INVALID_LENGTH`, not a partial copy.
///
/// # Safety
/// `length` must be readable and writable, and `out_value` writable for
/// `*length` bytes when not null. Called by the blob.
#[no_mangle]
#[cfg(target_os = "none")]
pub unsafe extern "C" fn nvs_get_blob(
    handle: u32,
    key: *const c_char,
    out_value: *mut c_void,
    length: *mut usize,
) -> i32 {
    if length.is_null() {
        return ESP_ERR_NVS_INVALID_LENGTH;
    }
    let mut buf = [0u8; MAX_VALUE_LEN];
    let n = match get_tagged(handle, key, TAG_BLOB, &mut buf) {
        Ok(n) => n,
        Err(e) => return e,
    };
    if out_value.is_null() {
        unsafe { length.write(n) };
        return ESP_OK;
    }
    if unsafe { length.read() } < n {
        unsafe { length.write(n) };
        return ESP_ERR_NVS_INVALID_LENGTH;
    }
    unsafe { core::ptr::copy_nonoverlapping(buf.as_ptr(), out_value as *mut u8, n) };
    unsafe { length.write(n) };
    ESP_OK
}

/// `nvs_erase_key(handle, key)`.
///
/// `kvstore` has no delete, so this writes an empty value. That reads back as
/// a zero-length record, which fails the tag check, so a later `get` reports
/// a failure rather than the old value. The difference from IDF's erase is
/// space: the record stays on the log until the store is erased.
///
/// # Safety
/// Called by the blob.
#[no_mangle]
#[cfg(target_os = "none")]
pub unsafe extern "C" fn nvs_erase_key(handle: u32, key: *const c_char) -> i32 {
    let Some((ns, len, writable)) = namespace_of(handle) else {
        return ESP_ERR_NVS_INVALID_HANDLE;
    };
    if !writable {
        return ESP_ERR_NVS_READ_ONLY;
    }
    let Some(k) = (unsafe { c_str(key, MAX_NAME_LEN) }) else {
        return ESP_ERR_NVS_INVALID_NAME;
    };
    let mut kbuf = [0u8; MAX_KEY_LEN];
    let Some(full) = compose_key(&ns[..len], k, &mut kbuf) else {
        return ESP_ERR_NVS_INVALID_NAME;
    };
    with_store(
        |s| match s.set(full, &[]) {
            Ok(()) => ESP_OK,
            Err(e) => map_err(e),
        },
        ESP_ERR_NVS_NOT_FOUND,
    )
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const CAP: usize = 8 * 1024;
    struct Fake {
        bytes: [u8; CAP],
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
        let mut f = Fake { bytes: [0xFF; CAP] };
        f.erase_all().unwrap();
        Store::open(f).unwrap()
    }

    #[test]
    fn a_value_round_trips_within_its_namespace() {
        let mut s = store();
        assert_eq!(put(&mut s, b"wifi", b"country", TAG_U16, &0x1234u16.to_le_bytes()), ESP_OK);
        let mut out = [0u8; MAX_VALUE_LEN];
        assert_eq!(fetch(&s, b"wifi", b"country", TAG_U16, &mut out), Ok(2));
        assert_eq!(u16::from_le_bytes([out[0], out[1]]), 0x1234);
    }

    #[test]
    fn two_namespaces_do_not_share_a_key() {
        // The whole reason the namespace is folded into the key. One driver
        // reading another's settings is the failure this prevents.
        let mut s = store();
        put(&mut s, b"wifi", b"n", TAG_U8, &[1]);
        put(&mut s, b"phy", b"n", TAG_U8, &[2]);
        let mut out = [0u8; MAX_VALUE_LEN];
        assert_eq!(fetch(&s, b"wifi", b"n", TAG_U8, &mut out), Ok(1));
        assert_eq!(out[0], 1);
        assert_eq!(fetch(&s, b"phy", b"n", TAG_U8, &mut out), Ok(1));
        assert_eq!(out[0], 2);
    }

    #[test]
    fn the_wrong_type_is_a_mismatch_not_a_short_read() {
        // Without the tag this returns the low byte of the u16 and looks like
        // a working read, which is exactly the silent wrong answer IDF's own
        // type check exists to stop.
        let mut s = store();
        put(&mut s, b"wifi", b"v", TAG_U16, &0xBEEFu16.to_le_bytes());
        let mut out = [0u8; MAX_VALUE_LEN];
        assert_eq!(
            fetch(&s, b"wifi", b"v", TAG_U8, &mut out),
            Err(ESP_ERR_NVS_TYPE_MISMATCH)
        );
    }

    #[test]
    fn a_missing_key_is_not_found() {
        let s = store();
        let mut out = [0u8; MAX_VALUE_LEN];
        assert_eq!(fetch(&s, b"wifi", b"nope", TAG_U8, &mut out), Err(ESP_ERR_NVS_NOT_FOUND));
    }

    #[test]
    fn an_over_long_name_is_refused_rather_than_truncated() {
        // Truncation would merge two namespaces that share fifteen characters.
        let mut buf = [0u8; MAX_KEY_LEN];
        assert!(compose_key(&[b'a'; MAX_NAME_LEN], &[b'b'; MAX_NAME_LEN], &mut buf).is_some());
        assert!(compose_key(&[b'a'; MAX_NAME_LEN + 1], b"k", &mut buf).is_none());
        assert!(compose_key(b"ns", &[b'b'; MAX_NAME_LEN + 1], &mut buf).is_none());
        assert!(compose_key(b"", b"k", &mut buf).is_none());
        assert!(compose_key(b"ns", b"", &mut buf).is_none());
    }

    #[test]
    fn the_composite_key_is_what_it_says() {
        let mut buf = [0u8; MAX_KEY_LEN];
        assert_eq!(compose_key(b"wifi", b"country", &mut buf).unwrap(), b"wifi:country");
    }

    #[test]
    fn a_blob_too_big_for_a_record_is_refused() {
        // kvstore takes 128 bytes and one is the tag. Saying so beats a
        // silently truncated blob.
        let mut s = store();
        assert_eq!(put(&mut s, b"wifi", b"b", TAG_BLOB, &[0u8; MAX_BLOB_LEN]), ESP_OK);
        assert_eq!(
            put(&mut s, b"wifi", b"b", TAG_BLOB, &[0u8; MAX_BLOB_LEN + 1]),
            ESP_ERR_NVS_VALUE_TOO_LONG
        );
    }

    #[test]
    fn the_error_values_are_esp_idfs() {
        // Read from nvs.h at v4.4. The blob compares against these numbers,
        // so a wrong one is a driver that mishandles a perfectly good failure.
        assert_eq!(ESP_OK, 0);
        assert_eq!(ESP_ERR_NVS_NOT_FOUND, 0x1102);
        assert_eq!(ESP_ERR_NVS_TYPE_MISMATCH, 0x1103);
        assert_eq!(ESP_ERR_NVS_READ_ONLY, 0x1104);
        assert_eq!(ESP_ERR_NVS_NOT_ENOUGH_SPACE, 0x1105);
        assert_eq!(ESP_ERR_NVS_INVALID_NAME, 0x1106);
        assert_eq!(ESP_ERR_NVS_INVALID_HANDLE, 0x1107);
        assert_eq!(ESP_ERR_NVS_INVALID_LENGTH, 0x110c);
        assert_eq!(ESP_ERR_NVS_VALUE_TOO_LONG, 0x110e);
    }

    #[test]
    fn every_type_tag_is_distinct() {
        // These go to flash and are read back by a later boot, so they are
        // values rather than an ordering that may be renumbered.
        let tags = [TAG_I8, TAG_U8, TAG_U16, TAG_BLOB];
        for (i, a) in tags.iter().enumerate() {
            assert_ne!(*a, 0, "0 would be indistinguishable from an empty record");
            for b in &tags[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
