// SPDX-License-Identifier: Apache-2.0

//! Filling in [`crate::osi::WifiOsiFuncs`].
//!
//! Each function here is a translation between FreeRTOS's conventions and
//! FlintOS's, and the translations are where the bugs are. The three that
//! matter most, written down once rather than at every call site:
//!
//! **Return codes are inverted.** FreeRTOS returns `pdTRUE` (1) for success
//! and `pdFALSE` (0) for failure. FlintOS returns `bool`. Getting this
//! backwards makes every blocking call appear to time out, which looks like a
//! dead radio rather than a wrong constant.
//!
//! **Priorities are inverted.** FreeRTOS counts *up* to more urgent; FlintOS
//! counts *down*. A blob task created at the number it asks for would run at
//! the opposite end of the system. See [`priority_from_freertos`].
//!
//! **Timeouts are in ticks, not milliseconds.** The blob speaks ticks and
//! `dynobj` speaks milliseconds. `OSI_FUNCS_TIME_BLOCKING` maps to
//! `dynobj::FOREVER`; anything else is converted.
//!
//! # Two kinds of symbol, one file
//!
//! The table is what Espressif made replaceable. Further down, under
//! `by_name`, are the symbols the archives call directly instead — `malloc`,
//! the logging hooks, the mesh stubs. That distinction is Espressif's rather
//! than a real one, and keeping the two in separate files is what let `malloc`
//! end up implemented twice.
//!
//! # Handles
//!
//! The blob holds `void*` handles it never dereferences. Each object is
//! allocated on the radio heap and the pointer handed back, so the handle *is*
//! the object. That keeps `_delete` honest — it frees what it was given —
//! and means a stale handle is a use-after-free rather than an index into a
//! table that has been reused, which is the easier of the two to catch under
//! a debugger.

use core::ffi::c_void;

use kernel::dynobj::{self, DynQueue, EventGroup, RecursiveMutex, Semaphore};
use kernel::heap::{self, Caps};

use crate::osi::{WifiOsiFuncs, TIME_BLOCKING};

/// What the table still leaves null, and why.
///
/// Not a list of things forgotten — a list of things that cannot be written
/// yet or that need hardware to mean anything. Kept here so the gap is
/// countable rather than discovered one crash at a time.
pub const UNIMPLEMENTED: &[(&str, &str)] = &[
    ("_set_intr / _clear_intr / _set_isr", "interrupt routing onto the ESP32 interrupt matrix; the IRAM placement it waited on is done"),
    ("_phy_* ", "PHY enable, init data and RF calibration (step 3.6)"),
    ("_coex_*", "coexistence; only meaningful once both radios run (#66, #67)"),
    ("_nvs_*", "maps onto kvstore, but the blob's key namespace needs deciding"),
    ("_timer_*", "esp_timer equivalents; TIMG exists, the shim does not"),
    ("_log_write / _log_writev", "variadic C logging into api::log"),
    ("_event_post", "the esp_event loop, which FlintOS has no equivalent of"),
];

// ── Conversions ─────────────────────────────────────────────────────────────

/// FreeRTOS `pdTRUE`.
pub const PD_TRUE: i32 = 1;
/// FreeRTOS `pdFALSE`.
pub const PD_FALSE: i32 = 0;

/// A `bool` as FreeRTOS would report it.
#[inline]
fn pd(ok: bool) -> i32 {
    if ok {
        PD_TRUE
    } else {
        PD_FALSE
    }
}

/// Blob ticks to FlintOS milliseconds.
///
/// The blob's tick is esp-idf's `portTICK_PERIOD_MS`, which is 1 ms in every
/// configuration Espressif ships for the ESP32 — `CONFIG_FREERTOS_HZ` is 100
/// by default but the Wi-Fi driver requires 1000. Treating a tick as a
/// millisecond is therefore exact rather than approximate, and this function
/// exists so that assumption has one home if it ever stops being true.
#[inline]
pub fn ms_from_ticks(ticks: u32) -> u32 {
    if ticks == TIME_BLOCKING {
        dynobj::FOREVER
    } else {
        ticks
    }
}

/// FreeRTOS priority to FlintOS priority.
///
/// FreeRTOS counts up: 0 is idle and `configMAX_PRIORITIES - 1` is the most
/// urgent. FlintOS counts down: 0 is the most urgent. So the two are mirror
/// images and a value passed straight through lands at the opposite end of the
/// system — a Wi-Fi task that must pre-empt everything would instead run below
/// the idle-adjacent background work, and the symptom is dropped frames under
/// load rather than anything that points here.
///
/// Espressif creates its tasks between 18 and 23 out of 25. Those map to the
/// urgent end here, which is what they are asking for.
#[inline]
pub fn priority_from_freertos(prio: u32) -> hal::types::Priority {
    const FREERTOS_MAX: u32 = 25;
    let clamped = if prio >= FREERTOS_MAX { FREERTOS_MAX - 1 } else { prio };
    // Mirror, then place in the Critical band's numeric range. Critical(0) is
    // the most urgent thing FlintOS has, and a radio that misses its window
    // corrupts a frame rather than merely running late.
    let inverted = (FREERTOS_MAX - 1 - clamped) as u8;
    // 0..=15 is the width of one band.
    hal::types::Priority::Critical(inverted.min(15))
}

// ── Allocation ──────────────────────────────────────────────────────────────

/// `_malloc`, `_malloc_internal`, `_wifi_malloc`.
///
/// All three are the same here: everything the radio heap holds is internal
/// RAM and DMA-capable, so there is nothing for the variants to distinguish.
/// They stay separate entries because the blob calls them by name.
pub(crate) unsafe extern "C" fn osi_malloc(size: usize) -> *mut c_void {
    unsafe { heap::alloc(size, 8) as *mut c_void }
}

/// `_malloc` alone takes `unsigned int` rather than `size_t` in the header.
/// The two are the same width on this target, but the signatures are not
/// interchangeable to the compiler and — more to the point — matching the
/// header exactly is the whole discipline of this file.
unsafe extern "C" fn osi_malloc_uint(size: u32) -> *mut c_void {
    unsafe { osi_malloc(size as usize) }
}

unsafe extern "C" fn osi_calloc(n: usize, size: usize) -> *mut c_void {
    let bytes = match n.checked_mul(size) {
        Some(b) => b,
        None => return core::ptr::null_mut(),
    };
    let p = unsafe { heap::alloc(bytes, 8) };
    if !p.is_null() {
        unsafe { core::ptr::write_bytes(p, 0, bytes) };
    }
    p as *mut c_void
}

unsafe extern "C" fn osi_zalloc(size: usize) -> *mut c_void {
    unsafe { osi_calloc(1, size) }
}

pub(crate) unsafe extern "C" fn osi_free(p: *mut c_void) {
    if !p.is_null() {
        unsafe { heap::free(p as *mut u8, Caps::Internal) };
    }
}

/// `_get_free_heap_size`.
///
/// The sum of the free blocks. The blob uses it to decide whether to attempt
/// an allocation at all, so reporting the *largest* block would be the more
/// useful number — but this entry has a defined meaning in esp-idf and
/// changing it silently would be worse than matching it.
unsafe extern "C" fn osi_get_free_heap_size() -> u32 {
    heap::free_bytes(Caps::Internal) as u32
}

// ── Queues ──────────────────────────────────────────────────────────────────

/// Put an object on the heap and hand back the pointer as the blob's handle.
unsafe fn box_up<T>(value: T) -> *mut c_void {
    let p = unsafe { heap::alloc(core::mem::size_of::<T>(), core::mem::align_of::<T>()) };
    if p.is_null() {
        return core::ptr::null_mut();
    }
    unsafe { core::ptr::write(p as *mut T, value) };
    p as *mut c_void
}

/// Take an object back off the heap.
unsafe fn unbox<T>(handle: *mut c_void) -> Option<T> {
    if handle.is_null() {
        return None;
    }
    let value = unsafe { core::ptr::read(handle as *mut T) };
    unsafe { heap::free(handle as *mut u8, Caps::Internal) };
    Some(value)
}

unsafe extern "C" fn osi_queue_create(len: u32, item_size: u32) -> *mut c_void {
    match DynQueue::create(len as usize, item_size as usize) {
        Some(q) => unsafe { box_up(q) },
        None => core::ptr::null_mut(),
    }
}

unsafe extern "C" fn osi_queue_delete(handle: *mut c_void) {
    if let Some(q) = unsafe { unbox::<DynQueue>(handle) } {
        q.delete();
    }
}

unsafe extern "C" fn osi_queue_send(handle: *mut c_void, item: *mut c_void, ticks: u32) -> i32 {
    if handle.is_null() {
        return PD_FALSE;
    }
    let q = unsafe { &mut *(handle as *mut DynQueue) };
    pd(unsafe { q.send(item as *const u8, ms_from_ticks(ticks)) })
}

/// `_queue_send_from_isr`.
///
/// `hptw` is FreeRTOS's `pxHigherPriorityTaskWoken`: an out-parameter the
/// handler checks on the way out to decide whether to yield. It is written
/// through only when non-null, which the blob does rely on.
#[cfg_attr(target_os = "none", link_section = ".iram1.radio")]
#[inline(never)]
unsafe extern "C" fn osi_queue_send_from_isr(
    handle: *mut c_void,
    item: *mut c_void,
    hptw: *mut c_void,
) -> i32 {
    if handle.is_null() {
        return PD_FALSE;
    }
    let q = unsafe { &mut *(handle as *mut DynQueue) };
    let (sent, woke) = unsafe { q.send_from_isr(item as *const u8) };
    if !hptw.is_null() {
        unsafe { *(hptw as *mut i32) = pd(woke) };
    }
    pd(sent)
}

unsafe extern "C" fn osi_queue_recv(handle: *mut c_void, item: *mut c_void, ticks: u32) -> i32 {
    if handle.is_null() {
        return PD_FALSE;
    }
    let q = unsafe { &mut *(handle as *mut DynQueue) };
    pd(unsafe { q.recv(item as *mut u8, ms_from_ticks(ticks)) })
}

unsafe extern "C" fn osi_queue_msg_waiting(handle: *mut c_void) -> u32 {
    if handle.is_null() {
        return 0;
    }
    unsafe { (*(handle as *mut DynQueue)).len() as u32 }
}

// ── Semaphores and mutexes ──────────────────────────────────────────────────

unsafe extern "C" fn osi_semphr_create(max: u32, init: u32) -> *mut c_void {
    match Semaphore::create(max, init) {
        Some(s) => unsafe { box_up(s) },
        None => core::ptr::null_mut(),
    }
}

unsafe extern "C" fn osi_semphr_delete(handle: *mut c_void) {
    let _ = unsafe { unbox::<Semaphore>(handle) };
}

unsafe extern "C" fn osi_semphr_take(handle: *mut c_void, ticks: u32) -> i32 {
    if handle.is_null() {
        return PD_FALSE;
    }
    let s = unsafe { &mut *(handle as *mut Semaphore) };
    pd(s.take(ms_from_ticks(ticks)))
}

#[cfg_attr(target_os = "none", link_section = ".iram1.radio")]
#[inline(never)]
unsafe extern "C" fn osi_semphr_give(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return PD_FALSE;
    }
    pd(unsafe { (*(handle as *mut Semaphore)).give() })
}

/// `_mutex_create` and `_recursive_mutex_create`.
///
/// Both give a recursive mutex. FlintOS's own mutex refuses re-entry by
/// design, and handing the blob a type that panics where FreeRTOS would have
/// succeeded is not a trade worth making for the non-recursive entry — the
/// blob's own code decides which it asked for.
unsafe extern "C" fn osi_mutex_create() -> *mut c_void {
    unsafe { box_up(RecursiveMutex::new()) }
}

unsafe extern "C" fn osi_mutex_delete(handle: *mut c_void) {
    let _ = unsafe { unbox::<RecursiveMutex>(handle) };
}

unsafe extern "C" fn osi_mutex_lock(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return PD_FALSE;
    }
    let m = unsafe { &mut *(handle as *mut RecursiveMutex) };
    pd(m.lock(dynobj::current_task(), dynobj::FOREVER))
}

unsafe extern "C" fn osi_mutex_unlock(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        return PD_FALSE;
    }
    let m = unsafe { &mut *(handle as *mut RecursiveMutex) };
    pd(m.unlock(dynobj::current_task()))
}

// ── Event groups ────────────────────────────────────────────────────────────

unsafe extern "C" fn osi_event_group_create() -> *mut c_void {
    unsafe { box_up(EventGroup::new()) }
}

unsafe extern "C" fn osi_event_group_delete(handle: *mut c_void) {
    let _ = unsafe { unbox::<EventGroup>(handle) };
}

unsafe extern "C" fn osi_event_group_set_bits(handle: *mut c_void, bits: u32) -> u32 {
    if handle.is_null() {
        return 0;
    }
    unsafe { (*(handle as *mut EventGroup)).set(bits) }
}

unsafe extern "C" fn osi_event_group_clear_bits(handle: *mut c_void, bits: u32) -> u32 {
    if handle.is_null() {
        return 0;
    }
    unsafe { (*(handle as *mut EventGroup)).clear(bits) }
}

/// Note the argument order: the header is
/// `(event, bits_to_wait_for, clear_on_exit, wait_for_all_bits, block_time)`,
/// while `EventGroup::wait` takes `wait_for_all` before `clear_on_exit`. They
/// are both `bool`-ish and swapping them compiles cleanly, so the order is
/// spelled out rather than trusted.
unsafe extern "C" fn osi_event_group_wait_bits(
    handle: *mut c_void,
    bits: u32,
    clear_on_exit: i32,
    wait_for_all: i32,
    ticks: u32,
) -> u32 {
    if handle.is_null() {
        return 0;
    }
    let g = unsafe { &*(handle as *mut EventGroup) };
    g.wait(
        bits,
        wait_for_all != 0,
        clear_on_exit != 0,
        ms_from_ticks(ticks),
    )
    .unwrap_or(0)
}

// ── Tasks ───────────────────────────────────────────────────────────────────

unsafe extern "C" fn osi_task_get_current_task() -> *mut c_void {
    dynobj::current_task() as usize as *mut c_void
}

unsafe extern "C" fn osi_task_get_max_priority() -> i32 {
    // What FreeRTOS would report, not what FlintOS uses internally: the blob
    // compares its own priorities against this before inverting.
    25
}

unsafe extern "C" fn osi_task_ms_to_tick(ms: u32) -> i32 {
    // One tick is one millisecond; see `ms_from_ticks`.
    ms as i32
}

#[cfg_attr(target_os = "none", link_section = ".iram1.radio")]
#[inline(never)]
unsafe extern "C" fn osi_task_yield_from_isr() {
    dynobj::yield_from_isr();
}

#[cfg_attr(target_os = "none", link_section = ".iram1.radio")]
#[inline(never)]
unsafe extern "C" fn osi_is_from_isr() -> bool {
    kernel::interrupt::in_interrupt()
}

// ── Spinlocks ───────────────────────────────────────────────────────────────

unsafe extern "C" fn osi_spin_lock_create() -> *mut c_void {
    unsafe { box_up(dynobj::SpinlockHandle::new()) }
}

unsafe extern "C" fn osi_spin_lock_delete(handle: *mut c_void) {
    let _ = unsafe { unbox::<dynobj::SpinlockHandle>(handle) };
}

// ── The table ───────────────────────────────────────────────────────────────

/// Build the table the blob is given.
///
/// Everything not set here is null, and [`UNIMPLEMENTED`] says why. A null is
/// a diagnosable crash at a known address; a wrong function is not.
pub fn table() -> WifiOsiFuncs {
    let mut t = WifiOsiFuncs::empty();

    t._malloc = Some(osi_malloc_uint);
    t._malloc_internal = Some(osi_malloc);
    t._wifi_malloc = Some(osi_malloc);
    t._calloc_internal = Some(osi_calloc);
    t._wifi_calloc = Some(osi_calloc);
    t._zalloc_internal = Some(osi_zalloc);
    t._wifi_zalloc = Some(osi_zalloc);
    t._free = Some(osi_free);
    t._get_free_heap_size = Some(osi_get_free_heap_size);

    t._queue_create = Some(osi_queue_create);
    t._wifi_create_queue = None; // takes ints and wraps a queue; see below
    t._queue_delete = Some(osi_queue_delete);
    t._queue_send = Some(osi_queue_send);
    t._queue_send_to_back = Some(osi_queue_send);
    t._queue_send_from_isr = Some(osi_queue_send_from_isr);
    t._queue_recv = Some(osi_queue_recv);
    t._queue_msg_waiting = Some(osi_queue_msg_waiting);

    t._semphr_create = Some(osi_semphr_create);
    t._semphr_delete = Some(osi_semphr_delete);
    t._semphr_take = Some(osi_semphr_take);
    t._semphr_give = Some(osi_semphr_give);

    t._mutex_create = Some(osi_mutex_create);
    t._recursive_mutex_create = Some(osi_mutex_create);
    t._mutex_delete = Some(osi_mutex_delete);
    t._mutex_lock = Some(osi_mutex_lock);
    t._mutex_unlock = Some(osi_mutex_unlock);

    t._event_group_create = Some(osi_event_group_create);
    t._event_group_delete = Some(osi_event_group_delete);
    t._event_group_set_bits = Some(osi_event_group_set_bits);
    t._event_group_clear_bits = Some(osi_event_group_clear_bits);
    t._event_group_wait_bits = Some(osi_event_group_wait_bits);

    t._task_get_current_task = Some(osi_task_get_current_task);
    t._task_get_max_priority = Some(osi_task_get_max_priority);
    t._task_ms_to_tick = Some(osi_task_ms_to_tick);
    t._task_yield_from_isr = Some(osi_task_yield_from_isr);
    t._is_from_isr = Some(osi_is_from_isr);

    t._spin_lock_create = Some(osi_spin_lock_create);
    t._spin_lock_delete = Some(osi_spin_lock_delete);

    t
}

// ── Symbols the blobs call by name ──────────────────────────────────────────
//
// Everything above answers `wifi_osi_funcs_t`, which is what Espressif chose
// to make replaceable. What follows answers what they did not: symbols the
// archives call directly and expect the surrounding system to define.
//
// These lived in a separate file for a while. That split followed Espressif's
// distinction rather than a real one, and left `malloc` implemented twice --
// once in each file, three commits apart. One file, one implementation.
//
// A module rather than a run of `#[cfg]` attributes, because the gating is the
// point: defining `malloc` in a host test binary collides with the system
// libc, and the collision surfaces as a link error in the middle of an
// unrelated test run.
#[cfg(target_os = "none")]
mod by_name {
    use core::ffi::{c_char, c_int, c_void};

    // Allocation delegates rather than reimplementing. Two copies of "take it
    // from the radio heap" would be two places to change the alignment, and
    // the second would be missed -- which is exactly what happened.

    /// # Safety
    /// C calling convention; `size` is the caller's.
    #[no_mangle]
    pub unsafe extern "C" fn malloc(size: usize) -> *mut c_void {
        unsafe { super::osi_malloc(size) }
    }

    /// # Safety
    /// `p` must have come from [`malloc`], or be null.
    #[no_mangle]
    pub unsafe extern "C" fn free(p: *mut c_void) {
        unsafe { super::osi_free(p) }
    }

    /// The erratum-safe DPORT read.
    ///
    /// Not a convenience: a plain load from DPORT can return the value of an
    /// unrelated APB read performed by the *other* core. `dport::read` is the
    /// workaround — an APB pre-read with the two loads adjacent and interrupts
    /// masked — and the blobs go through it for the same reason everything
    /// else does. A bare `read_volatile` here would work almost always, which
    /// is the worst way for it to be wrong.
    ///
    /// # Safety
    /// `reg` must be a DPORT register address.
    #[no_mangle]
    #[cfg_attr(target_os = "none", link_section = ".iram1.radio")]
    #[inline(never)]
    pub unsafe extern "C" fn esp_dport_access_reg_read(reg: u32) -> u32 {
        unsafe { soc_esp32::dport::read(reg) }
    }

    // ── Logging ─────────────────────────────────────────────────────────────
    //
    // `phy_printf`, `rtc_printf`, `net80211_printf` and `coexist_printf` are
    // variadic C functions, and Rust cannot *define* a variadic function --
    // only declare one.
    //
    // So these take the format string alone. On the windowed ABI the caller
    // passes arguments in a2-a7 and on the stack and cleans up after itself,
    // so a callee reading fewer than it was given is well defined: it ignores
    // the rest. What is lost is the formatting -- "rate %d" logs as `rate %d`
    // rather than `rate 6`.
    //
    // Worth stating plainly rather than leaving to be discovered from puzzling
    // output. These are paths the blob takes when something has already gone
    // wrong, and the format string still says which one. Doing better means a
    // C `vsnprintf` against `va_list`: a great deal of work for a message
    // nobody reads in normal operation.

    /// # Safety
    /// `fmt` must be a nul-terminated C string, which every caller here is.
    unsafe fn log_c_str(tag: &str, fmt: *const c_char) -> c_int {
        if fmt.is_null() {
            return 0;
        }
        // Bounded: a string with no terminator would otherwise walk memory
        // until it faulted, and a diagnostic path must not be able to make
        // things worse.
        const MAX: usize = 256;
        let mut len = 0;
        while len < MAX && unsafe { *fmt.add(len) } != 0 {
            len += 1;
        }
        let bytes = unsafe { core::slice::from_raw_parts(fmt as *const u8, len) };
        match core::str::from_utf8(bytes) {
            Ok(s) => api::log_info!("[{}] {}", tag, s),
            // Not worth reporting as an error: a corrupt format string in a
            // diagnostic path says the same thing either way.
            Err(_) => api::log_info!("[{}] <non-utf8 message>", tag),
        }
        len as c_int
    }

    macro_rules! blob_printf {
        ($name:ident, $tag:literal) => {
            /// # Safety
            /// Variadic in C; see the note above. `fmt` must be nul-terminated.
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

    // ── PHY critical section ────────────────────────────────────────────────
    //
    // esp-idf's pair, which has a trap in it:
    //
    //     uint32_t IRAM_ATTR phy_enter_critical(void) {
    //         ...
    //         // Interrupt level will be stored in current tcb, so always
    //         // return zero.
    //         return 0;
    //     }
    //     void IRAM_ATTR phy_exit_critical(uint32_t level) {
    //         // Param level don't need any more, ignore it.
    //
    // The level does **not** round-trip. `enter` returns zero and `exit`
    // discards what it is given, because FreeRTOS keeps the saved state in the
    // task control block. So we cannot hand the saved `PS` out through the
    // return value and expect it back -- the blob is entitled to pass zero,
    // and restoring zero to `PS` would unmask everything including levels the
    // kernel never unmasks.
    //
    // It is kept here instead, per core, with a nesting count so only the
    // outermost exit restores. Same shape as the FreeRTOS original, minus the
    // task control block.

    /// Saved `PS` and nesting depth, one slot per core.
    ///
    /// Two cores never share a slot, and a core cannot preempt itself between
    /// the store and the mask, so no lock is needed around it — the mask is
    /// what makes the region exclusive in the first place.
    static PHY_CS: [core::sync::atomic::AtomicU32; 2] = [
        core::sync::atomic::AtomicU32::new(0),
        core::sync::atomic::AtomicU32::new(0),
    ];
    static PHY_CS_DEPTH: [core::sync::atomic::AtomicU32; 2] = [
        core::sync::atomic::AtomicU32::new(0),
        core::sync::atomic::AtomicU32::new(0),
    ];

    /// # Safety
    /// Must be matched by exactly one [`phy_exit_critical`] on the same core.
    #[no_mangle]
    #[cfg_attr(target_os = "none", link_section = ".iram1.radio")]
    #[inline(never)]
    pub unsafe extern "C" fn phy_enter_critical() -> u32 {
        let core = kernel::smp::current_core().index();
        let saved = unsafe { kernel::arch::cs_enter() };
        // Only the outermost entry's `PS` is the one worth restoring; an inner
        // one was taken with interrupts already masked.
        if PHY_CS_DEPTH[core].fetch_add(1, core::sync::atomic::Ordering::Acquire) == 0 {
            PHY_CS[core].store(saved, core::sync::atomic::Ordering::Relaxed);
        }
        // Zero, as esp-idf does. The blob is documented not to use it.
        0
    }

    /// # Safety
    /// Must match a [`phy_enter_critical`] on this core. The argument is
    /// ignored, as it is in esp-idf.
    #[no_mangle]
    #[cfg_attr(target_os = "none", link_section = ".iram1.radio")]
    #[inline(never)]
    pub unsafe extern "C" fn phy_exit_critical(_level: u32) {
        let core = kernel::smp::current_core().index();
        let depth = PHY_CS_DEPTH[core].load(core::sync::atomic::Ordering::Relaxed);
        if depth == 0 {
            // Unbalanced. Restoring a `PS` we never saved would be worse than
            // doing nothing, and saying so is better than either.
            api::log_error!("radio: phy_exit_critical without a matching enter");
            return;
        }
        PHY_CS_DEPTH[core].store(depth - 1, core::sync::atomic::Ordering::Release);
        if depth == 1 {
            let saved = PHY_CS[core].load(core::sync::atomic::Ordering::Relaxed);
            unsafe { kernel::arch::cs_exit(saved) };
        }
    }

    // ── RTC ─────────────────────────────────────────────────────────────────

    /// The 48-bit RTC counter, in RTC slow-clock ticks.
    ///
    /// The registers, the latch-and-read sequence and the bounded wait are all
    /// `soc_esp32::rtc`'s -- this file used to carry its own copy of every one
    /// of them, beside a second copy in `arch-xtensa`. Three implementations of
    /// one 48-bit read.
    ///
    /// The bound matters on this path in particular: it runs inside PHY
    /// initialisation, and a stopped slow clock would otherwise hang the radio
    /// with nothing pointing at the cause. 100,000 polls is orders of
    /// magnitude more than the few microseconds a sample takes.
    ///
    /// # Safety
    /// Reads RTC_CNTL. Called by the blob.
    #[no_mangle]
    pub unsafe extern "C" fn rtc_time_get() -> u64 {
        match unsafe { soc_esp32::rtc::counter(100_000) } {
            Some(t) => t,
            None => {
                api::log_error!("radio: RTC counter never reported valid");
                0
            }
        }
    }

    /// The crystal frequency in MHz, as the bootloader recorded it.
    ///
    /// The decode -- duplicated 16-bit halves, esp-idf's ROM-log flag in the
    /// low bit -- is `soc_esp32::rtc::xtal_freq_mhz`, which returns `None`
    /// rather than a default when the register holds nothing credible.
    ///
    /// **The fallback is this crate's decision, not the SoC crate's**, which
    /// is why the split is here. A wrong crystal mis-calibrates the radio and
    /// the symptom is poor range, nothing pointing at this function -- so the
    /// answer is the one both supported modules use, said loudly, rather than
    /// esp-idf's `RTC_XTAL_FREQ_AUTO` (0) and an estimator FlintOS has not
    /// got.
    ///
    /// # Safety
    /// Reads one RTC_CNTL register. Called by the blob.
    #[no_mangle]
    pub unsafe extern "C" fn rtc_get_xtal() -> u32 {
        if let Some(mhz) = unsafe { soc_esp32::rtc::xtal_freq_mhz() } {
            return mhz;
        }
        api::log_error!(
            "radio: RTC_XTAL_FREQ_REG holds no crystal this chip has;              falling back to 40 MHz, which a 26 MHz board needs checked"
        );
        soc_esp32::rtc::XTAL_40_MHZ
    }

    // The five below are RTC clock and sleep entry points that esp-idf v4.4
    // defines *nowhere* -- not in its source, and not in any of its twelve
    // ROM linker scripts -- yet esp-idf links. They are referenced by members
    // of librtc.a that its build never pulls in, because a linker only takes
    // the archive members it needs.
    //
    // So these most likely never link either, and defining them costs nothing
    // if so. If one ever does get pulled in, a panic naming it is far better
    // than a plausible zero: `rtc_get_xtal` returning the wrong crystal
    // frequency would mis-calibrate the radio, and the symptom would be poor
    // range rather than anything pointing here.
    //
    // `rtc_get_xtal` in particular is deliberately not guessed. esp-idf reads
    // it from a retention register, and which of the STORE registers holds it
    // is something this has not confirmed from the source. Guessing a register
    // address is what cost several sessions on the flash driver.

    macro_rules! rtc_unimplemented {
        ($($name:ident),* $(,)?) => {
            $(
                /// Not implemented; see the note above. Part of step 3.6.
                ///
                /// # Safety
                /// Expected never to be linked, let alone called.
                #[no_mangle]
                pub unsafe extern "C" fn $name() -> ! {
                    panic!(concat!(
                        "radio: ", stringify!($name), " was called. It is an RTC ",
                        "clock/sleep entry point that esp-idf does not define ",
                        "either, so this build did not expect the archive member ",
                        "referencing it to be linked. See doc/plan-radio.md 3.6."
                    ))
                }
            )*
        };
    }

    rtc_unimplemented!(
        rtc_init_clk,
        rtc_dbias_cfg,
        rtc_slowck_cali,
        rtc_slp_prep,
        rtc_sleep_set_wakeup_time,
    );

    // ── C library ───────────────────────────────────────────────────────────
    //
    // Only what `compiler_builtins` does not already provide. It supplies
    // `memcpy`, `memset`, `memmove`, `memcmp` and `strlen`, and defining those
    // again here would be a duplicate symbol at the final link rather than a
    // helpful redundancy. Checked against the built rlib rather than assumed;
    // the list below is the difference.

    /// # Safety
    /// `dst` must have room for `src` including its terminator.
    #[no_mangle]
    pub unsafe extern "C" fn strcpy(dst: *mut c_char, src: *const c_char) -> *mut c_char {
        let mut i = 0;
        loop {
            let c = unsafe { *src.add(i) };
            unsafe { *dst.add(i) = c };
            if c == 0 {
                return dst;
            }
            i += 1;
        }
    }

    /// # Safety
    /// Both must be readable for `n` bytes or until a terminator.
    #[no_mangle]
    pub unsafe extern "C" fn strncmp(a: *const c_char, b: *const c_char, n: usize) -> c_int {
        for i in 0..n {
            let (x, y) = unsafe { (*a.add(i) as u8, *b.add(i) as u8) };
            if x != y {
                return x as c_int - y as c_int;
            }
            if x == 0 {
                break;
            }
        }
        0
    }

    /// C's `strncpy`, terminator quirk included.
    ///
    /// If `src` is shorter than `n` the remainder of `dst` is zero-filled, and
    /// if it is longer the result is **not** terminated. Both are surprising
    /// and both are what the standard says; a caller written against C expects
    /// exactly this, so the quirk is reproduced rather than improved on.
    ///
    /// # Safety
    /// `dst` must have room for `n` bytes.
    #[no_mangle]
    pub unsafe extern "C" fn strncpy(dst: *mut c_char, src: *const c_char, n: usize) -> *mut c_char {
        let mut i = 0;
        while i < n {
            let c = unsafe { *src.add(i) };
            unsafe { *dst.add(i) = c };
            i += 1;
            if c == 0 {
                break;
            }
        }
        while i < n {
            unsafe { *dst.add(i) = 0 };
            i += 1;
        }
        dst
    }

    /// # Safety
    /// `s` must be readable for up to `maxlen` bytes.
    #[no_mangle]
    pub unsafe extern "C" fn strnlen(s: *const c_char, maxlen: usize) -> usize {
        let mut n = 0;
        while n < maxlen && unsafe { *s.add(n) } != 0 {
            n += 1;
        }
        n
    }

    /// # Safety
    /// Never returns.
    #[no_mangle]
    pub unsafe extern "C" fn abort() -> ! {
        panic!("radio: a blob called abort()")
    }

    /// # Safety
    /// `s` must be nul-terminated.
    #[no_mangle]
    pub unsafe extern "C" fn puts(s: *const c_char) -> c_int {
        unsafe { log_c_str("blob", s) }
    }

    /// `sprintf`, which cannot do what its name says.
    ///
    /// Variadic, so Rust cannot define it — the same wall as the `*_printf`
    /// hooks above. Unlike those, this one writes into a caller's buffer whose
    /// size it is never told, so the tempting fallback of copying the format
    /// string in is a buffer overflow waiting for a format string longer than
    /// the buffer someone sized for the *formatted* result.
    ///
    /// So it writes an empty string and reports zero. That is wrong but
    /// bounded, and bounded is the property worth keeping when the alternative
    /// is corrupting the caller's stack. If a blob path ever turns out to
    /// depend on the contents, the fix is a small C shim calling the real
    /// `vsnprintf` — not a cleverer guess here.
    ///
    /// # Safety
    /// `buf` must be writable for at least one byte.
    #[no_mangle]
    pub unsafe extern "C" fn sprintf(buf: *mut c_char, _fmt: *const c_char) -> c_int {
        if !buf.is_null() {
            unsafe { *buf = 0 };
        }
        0
    }

    /// Population count. `compiler_builtins` has the other nine libgcc
    /// routines the blobs want but not this one.
    #[no_mangle]
    pub extern "C" fn __popcountsi2(a: i32) -> i32 {
        (a as u32).count_ones() as i32
    }

    // ── Odds and ends ───────────────────────────────────────────────────────

    /// esp-idf's event base for Wi-Fi, which is a `const char *` compared by
    /// pointer rather than by value. What it points at does not matter; that
    /// every reference sees the *same* address does.
    #[no_mangle]
    pub static WIFI_EVENT: &[u8; 11] = b"WIFI_EVENT\0";

    /// Espressif's hex-string decoder: `hex` into `buf`, `len` bytes.
    ///
    /// Returns 0 on success and -1 on a non-hex digit, which is the contract
    /// its callers check. Implemented rather than stubbed because it is six
    /// lines and a stub returning success would hand the radio a buffer of
    /// zeroes where it expected a key.
    ///
    /// # Safety
    /// `hex` must be readable for `2 * len` bytes; `buf` writable for `len`.
    #[no_mangle]
    pub unsafe extern "C" fn hexstr2bin(hex: *const c_char, buf: *mut u8, len: usize) -> c_int {
        fn nibble(c: u8) -> Option<u8> {
            match c {
                b'0'..=b'9' => Some(c - b'0'),
                b'a'..=b'f' => Some(c - b'a' + 10),
                b'A'..=b'F' => Some(c - b'A' + 10),
                _ => None,
            }
        }
        for i in 0..len {
            let hi = unsafe { *hex.add(i * 2) } as u8;
            let lo = unsafe { *hex.add(i * 2 + 1) } as u8;
            match (nibble(hi), nibble(lo)) {
                (Some(h), Some(l)) => unsafe { *buf.add(i) = (h << 4) | l },
                _ => return -1,
            }
        }
        0
    }

    // ── Mesh, stubbed ───────────────────────────────────────────────────────
    //
    // `libnet80211.a` references these thirteen unconditionally, so leaving
    // `libmesh.a` out of the link is not free -- the symbols still have to
    // exist. Linking mesh instead resolves them and pulls in seven more,
    // including `esp_event_handler_register`, which wants an event loop
    // FlintOS does not have. Thirteen stubs are the smaller surface.
    //
    // Unreachable in a station-only build: nothing starts mesh, and these are
    // reached only from code paths mesh enables. Reaching one means that
    // assumption has broken, so they say so loudly rather than returning a
    // plausible zero and letting the radio continue into undefined behaviour.

    macro_rules! mesh_stub {
        ($($name:ident),* $(,)?) => {
            $(
                /// Unreachable in a station-only build. See the note above.
                ///
                /// # Safety
                /// Never called. Deliberately argument-free: the real
                /// signatures differ, and since reaching this is already a
                /// fault, only the symbol and the panic matter.
                #[no_mangle]
                pub unsafe extern "C" fn $name() -> ! {
                    panic!(concat!(
                        "radio: mesh entry point ", stringify!($name),
                        " was called. Mesh is not supported and libmesh.a is ",
                        "not linked, so reaching this means the radio was ",
                        "configured for mesh somewhere it should not have been."
                    ))
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
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_and_failure_use_freertos_s_codes_not_rust_s() {
        // Inverting these makes every blocking call look like a timeout, which
        // reads as a dead radio rather than a wrong constant.
        assert_eq!(pd(true), 1);
        assert_eq!(pd(false), 0);
        assert_eq!(PD_TRUE, 1);
        assert_eq!(PD_FALSE, 0);
    }

    #[test]
    fn blocking_forever_survives_the_tick_conversion() {
        assert_eq!(ms_from_ticks(TIME_BLOCKING), dynobj::FOREVER);
        // And an ordinary timeout passes through, one tick being one ms.
        assert_eq!(ms_from_ticks(0), 0);
        assert_eq!(ms_from_ticks(250), 250);
    }

    #[test]
    fn priorities_are_mirrored_not_passed_through() {
        use hal::types::Priority;
        // FreeRTOS's most urgent is its largest; FlintOS's is its smallest.
        let urgent = priority_from_freertos(24);
        let idle = priority_from_freertos(0);
        assert!(
            urgent.numeric() < idle.numeric(),
            "a high FreeRTOS priority must become an urgent FlintOS one"
        );
        assert_eq!(urgent, Priority::Critical(0));
        // Espressif's own Wi-Fi tasks sit at 18-23; they must all land urgent.
        for p in 18..=23 {
            assert!(priority_from_freertos(p).numeric() <= Priority::Critical(6).numeric());
        }
    }

    #[test]
    fn an_out_of_range_priority_is_clamped_rather_than_wrapping() {
        // Subtracting from FREERTOS_MAX would underflow, and an underflow here
        // produces a priority number far outside the band -- which is not a
        // panic, just a task that never runs.
        let p = priority_from_freertos(999);
        assert!(p.numeric() <= hal::types::Priority::Background(15).numeric());
    }

    #[test]
    fn the_table_fills_in_what_the_object_model_can_answer() {
        let t = table();
        assert!(t._queue_create.is_some());
        assert!(t._semphr_take.is_some());
        assert!(t._event_group_wait_bits.is_some());
        assert!(t._malloc.is_some() && t._free.is_some());
        assert!(t._spin_lock_create.is_some());
        // And leaves the rest null on purpose.
        assert!(t._phy_enable.is_none());
        assert!(t._coex_init.is_none());
        assert_eq!(t._version, crate::osi::VERSION);
        assert_eq!(t._magic, crate::osi::MAGIC);
    }

    #[test]
    fn every_unimplemented_entry_carries_a_reason() {
        assert!(!UNIMPLEMENTED.is_empty());
        for (name, why) in UNIMPLEMENTED {
            assert!(!name.is_empty());
            assert!(why.len() > 10, "{name} needs a real reason, not a shrug");
        }
    }
}
