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
    ("_set_intr / _clear_intr / _set_isr", "interrupt routing; needs the blob's ISRs in IRAM (step 3.5)"),
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

unsafe extern "C" fn osi_task_yield_from_isr() {
    dynobj::yield_from_isr();
}

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
