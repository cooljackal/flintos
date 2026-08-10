// SPDX-License-Identifier: Apache-2.0

//! C symbols the blobs reference directly, rather than through the OSI table.
//!
//! [`crate::osi::WifiOsiFuncs`] covers what Espressif chose to make
//! replaceable. This covers what they did not: symbols the archives call by
//! name and expect the surrounding system to define. `make blob-symbols` lists
//! them; this file is where they get answered.
//!
//! # Everything here is target-only
//!
//! `#[cfg(target_os = "none")]` on the lot. Defining `malloc` in a host test
//! binary would collide with the system libc, and the collision is a link
//! error in the middle of an unrelated test run.
//!
//! # The printf problem
//!
//! Four of these — `phy_printf`, `rtc_printf`, `net80211_printf`,
//! `coexist_printf` — are variadic C functions, and **Rust cannot define a
//! variadic function**. It can only declare one.
//!
//! They are defined here taking the format string alone. On the Xtensa
//! windowed ABI the caller passes arguments in `a2`–`a7` and on the stack, and
//! cleans up after itself, so a callee that reads fewer than it was given is
//! well defined — it simply ignores the rest. What is lost is the *formatting*:
//! the format string is logged verbatim, so `"rate %d"` appears as `rate %d`
//! rather than `rate 6`.
//!
//! That is a real limitation and worth stating plainly rather than discovering
//! from confusing output. These are diagnostic paths the blob takes when
//! something has already gone wrong; the format string alone still says which
//! one, which is most of the value. Doing better means implementing a C
//! `vsnprintf` against `va_list`, which is a great deal of work for a message
//! nobody reads in normal operation.

#![cfg(target_os = "none")]

use core::ffi::{c_char, c_int, c_void};

// ── Allocation ──────────────────────────────────────────────────────────────
//
// Called by name rather than through the table, so the OSI entries are not
// enough on their own -- but they are the *same* functions, delegated to
// rather than reimplemented. Two copies of "allocate from the radio heap"
// would be two places to change the alignment, and the second one would be
// missed.

/// # Safety
/// C calling convention; `size` is the caller's.
#[no_mangle]
pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
    unsafe { crate::adapter::osi_malloc(size) }
}

/// # Safety
/// `p` must have come from [`malloc`] here, or be null.
#[no_mangle]
pub unsafe extern "C" fn free(p: *mut c_void) {
    unsafe { crate::adapter::osi_free(p) }
}

// ── DPORT ───────────────────────────────────────────────────────────────────

/// The erratum-safe DPORT read.
///
/// Not a convenience: a plain load from DPORT can return the value of an
/// unrelated APB read performed by the *other* core. `soc_esp32::dport::read`
/// is the workaround — an APB pre-read with the two loads adjacent and
/// interrupts masked — and the blobs must go through it for the same reason
/// everything else does. Wiring this to a bare `read_volatile` would work
/// almost always, which is the worst way for it to be wrong.
///
/// # Safety
/// `reg` must be a DPORT register address.
#[no_mangle]
pub unsafe extern "C" fn esp_dport_access_reg_read(reg: u32) -> u32 {
    unsafe { soc_esp32::dport::read(reg) }
}

// ── Logging ─────────────────────────────────────────────────────────────────

/// Log a blob's diagnostic line.
///
/// The format string only — see the module docs on why the arguments cannot be
/// interpolated from Rust.
///
/// # Safety
/// `fmt` must be a nul-terminated C string, which every caller here is.
unsafe fn log_c_str(tag: &str, fmt: *const c_char) -> c_int {
    if fmt.is_null() {
        return 0;
    }
    // Bounded: a blob passing a string with no terminator would otherwise walk
    // memory until it faulted, and this is a diagnostic path that must not be
    // able to make things worse.
    const MAX: usize = 256;
    let mut len = 0;
    while len < MAX && unsafe { *fmt.add(len) } != 0 {
        len += 1;
    }
    let bytes = unsafe { core::slice::from_raw_parts(fmt as *const u8, len) };
    match core::str::from_utf8(bytes) {
        Ok(s) => api::log_info!("[{}] {}", tag, s),
        // Not an error worth reporting: a corrupt format string in a
        // diagnostic path says the same thing either way.
        Err(_) => api::log_info!("[{}] <non-utf8 message>", tag),
    }
    len as c_int
}

macro_rules! blob_printf {
    ($name:ident, $tag:literal) => {
        /// # Safety
        /// Variadic in C; see the module docs. `fmt` must be nul-terminated.
        #[no_mangle]
        pub unsafe extern "C" fn $name(fmt: *const c_char) -> c_int {
            unsafe { log_c_str($tag, fmt) }
        }
    };
}

blob_printf!(phy_printf, "phy");
blob_printf!(rtc_printf, "rtc");
blob_printf!(net80211_printf, "net80211");
blob_printf!(coexist_printf, "coex");

// ── Mesh, stubbed ───────────────────────────────────────────────────────────
//
// `libnet80211.a` references these thirteen unconditionally, so leaving
// `libmesh.a` out of the link is not free — the symbols still have to exist.
// Linking mesh instead resolves them and pulls in seven more, including
// `esp_event_handler_register`, which wants an event loop FlintOS does not
// have. Thirteen stubs are the smaller surface.
//
// They are unreachable in a station-only build: nothing ever starts mesh, and
// these are only called from code paths mesh enables. Reaching one means that
// assumption has broken, so they say so loudly rather than returning a
// plausible zero and letting the radio continue into undefined behaviour.

macro_rules! mesh_stub {
    ($($name:ident),* $(,)?) => {
        $(
            /// Unreachable in a station-only build. See the note above.
            ///
            /// # Safety
            /// Never called. The signature is deliberately argument-free: the
            /// real ones differ, and since reaching this is already a fault
            /// the only thing that matters is the symbol and the panic.
            #[no_mangle]
            pub unsafe extern "C" fn $name() -> ! {
                panic!(
                    concat!(
                        "radio: mesh entry point ", stringify!($name),
                        " was called. Mesh is not supported and libmesh.a is ",
                        "not linked, so reaching this means the radio was ",
                        "configured for mesh somewhere it should not have been."
                    )
                )
            }
        )*
    };
}

mesh_stub!(
    ieee80211_init_mesh_assoc_ie,
    ieee80211_vnd_mesh_quick_get,
    ieee80211_vnd_mesh_quick_set,
    ieee80211_vnd_mesh_roots_get,
    ieee80211_vnd_mesh_roots_set,
    mesh_clear_parent_candidate,
    mesh_get_parent_candidate,
    mesh_get_parent_monitor_config,
    mesh_get_rssi_threshold,
    mesh_set_ie_crypto_config,
    mesh_set_parent_candidate,
    mesh_set_parent_monitor_config,
    mesh_set_rssi_threshold,
);
