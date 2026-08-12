// SPDX-License-Identifier: Apache-2.0

//! The driver's events: a bounded queue and a task that drains it.
//!
//! # Why a queue, when a direct call is shorter
//!
//! `_event_post` is called from inside the blob, on `wifiT`, part-way through
//! whatever the driver was doing. The first implementation called the
//! application's handler right there. That is one function call instead of a
//! copy, an allocation-free ring push and a context switch — and it is wrong,
//! for a reason both references state rather than imply.
//!
//! Zephyr's event thread carries the comment:
//!
//! > Dispatch library-posted events on this dedicated task so a handler runs
//! > on its own stack and cannot stall or overrun the Wi-Fi library task. A
//! > single queue drained by one task preserves event order.
//!
//! esp-idf does the same through `esp_event`'s loop task. Both then do the
//! thing a synchronous handler cannot: **they call back into `esp_wifi_*` from
//! the handler.** Zephyr drains `esp_wifi_scan_get_ap_record` inside its
//! `WIFI_EVENT_SCAN_DONE` case. A handler invoked on `wifiT` cannot do that —
//! it would re-enter the driver from the driver's own task, behind the API
//! lock that task already holds.
//!
//! That is the whole reason this file exists, and it was found by comparing
//! against the references rather than by debugging: `esp_wifi_scan_get_ap_num`
//! did not return, and the shape of the call was a pattern neither reference
//! uses.
//!
//! # What is copied, and why it has to be
//!
//! `event_data` points at the driver's own storage and is **valid only for the
//! duration of the call**. A queue that stored the pointer would hand the
//! handler freed memory some microseconds later — which is exactly the class
//! of bug the OSI table pointer already produced once (see
//! [`crate::wifi::init`]). So the payload is copied into the entry.
//!
//! `event_base` is *not* copied: it is a `const char *` to a string literal in
//! the blob's `.rodata`, or to [`crate::wifi::WIFI_EVENT`], both of which
//! outlive everything here. It is compared by pointer, which is what makes it
//! an identity rather than a string.

use core::ffi::{c_char, c_void};

use kernel::smp::Spinlock;

/// The largest event payload kept.
///
/// The biggest a station build posts is `wifi_event_sta_disconnected_t`: a
/// 32-byte SSID, its length, a 6-byte BSSID, a reason and an RSSI — 45 bytes.
/// 64 leaves room for the ones that grow without making the queue expensive:
/// this is [`QUEUE_LEN`] times this many bytes of `.bss`, and nothing else.
///
/// A payload larger than this is truncated rather than dropped, and says so.
/// The alternative — dropping — loses the event *and* the fact that it
/// happened, and every event carries its id in the entry regardless.
pub const MAX_PAYLOAD: usize = 64;

/// How many events may be waiting.
///
/// Eight. The driver posts one per state change, and the dispatch task runs
/// above the application, so the queue is normally empty or holds one. Depth
/// is here for the burst when a scan finishes and something else changes at
/// the same time.
pub const QUEUE_LEN: usize = 8;

/// What a handler is given.
///
/// `base` is an `esp_event_base_t` — compare it against
/// [`crate::wifi::WIFI_EVENT`] by pointer. `data` points into the dispatch
/// task's own copy and is valid for the duration of the call only.
pub type EventHandler = fn(base: *const c_char, id: i32, data: *mut c_void, len: usize);

/// One queued event.
#[derive(Clone, Copy)]
struct Event {
    /// The blob's `esp_event_base_t`, held as a `usize` because a raw pointer
    /// is not `Send` and this crosses tasks. Never dereferenced here.
    base: usize,
    id: i32,
    len: usize,
    data: [u8; MAX_PAYLOAD],
}

impl Event {
    const EMPTY: Self = Event { base: 0, id: 0, len: 0, data: [0; MAX_PAYLOAD] };
}

/// The ring. Head is where the next event comes out, `len` how many are in.
struct Ring {
    slots: [Event; QUEUE_LEN],
    head: usize,
    len: usize,
    /// Events refused because the queue was full, since boot.
    dropped: u32,
}

impl Ring {
    const fn new() -> Self {
        Ring { slots: [Event::EMPTY; QUEUE_LEN], head: 0, len: 0, dropped: 0 }
    }

    fn push(&mut self, e: Event) -> bool {
        if self.len == QUEUE_LEN {
            self.dropped = self.dropped.saturating_add(1);
            return false;
        }
        let at = (self.head + self.len) % QUEUE_LEN;
        self.slots[at] = e;
        self.len += 1;
        true
    }

    fn pop(&mut self) -> Option<Event> {
        if self.len == 0 {
            return None;
        }
        let e = self.slots[self.head];
        self.head = (self.head + 1) % QUEUE_LEN;
        self.len -= 1;
        Some(e)
    }
}

static QUEUE: Spinlock<Ring> = Spinlock::new(Ring::new());

/// The registered handler.
static HANDLER: Spinlock<Option<EventHandler>> = Spinlock::new(None);

/// Install what the dispatch task calls. Returns the previous handler.
///
/// The handler runs on the dispatch task, on its own stack, with nothing of
/// the driver's held. It **may** call back into `esp_wifi_*`, which is the
/// point — see the module docs.
pub fn set_handler(f: Option<EventHandler>) -> Option<EventHandler> {
    HANDLER.with(|h| core::mem::replace(h, f))
}

/// How many events have been refused because the queue was full.
///
/// Reported rather than silent: a dropped event is a scan result nobody
/// collects or a disconnect nobody notices, and the only symptom is absence.
pub fn dropped() -> u32 {
    QUEUE.with(|q| q.dropped)
}

/// `_event_post(base, id, data, len, ticks)`, and `wifi_init_config_t`'s
/// `event_handler`.
///
/// Both routes land here. esp-idf's `esp_event_send_internal` — the one the
/// config field points at — calls `esp_event_post`, the OSI entry, and then
/// forwards to the legacy loop; with no legacy loop to forward to, the two
/// converge on one function.
///
/// **Always returns `ESP_OK`**, including when the queue is full. The driver
/// treats a failed post as a fault worth retrying, and a full queue is a
/// consumer that is behind, not an error the driver can do anything about.
/// The drop is counted instead — see [`dropped`].
///
/// `ticks` is ignored. It is how long esp-idf would block waiting for room on
/// the event queue, and blocking here would be blocking `wifiT`, which is the
/// thing this file exists to stop.
///
/// # Safety
/// `base` is a C string the blob owns. `data` is readable for `len` bytes.
/// Called by the blob.
pub unsafe extern "C" fn post(
    base: *const c_char,
    id: i32,
    data: *mut c_void,
    len: usize,
    _ticks: u32,
) -> i32 {
    let mut e = Event { base: base as usize, id, len: 0, data: [0; MAX_PAYLOAD] };
    if !data.is_null() && len > 0 {
        let n = len.min(MAX_PAYLOAD);
        unsafe { core::ptr::copy_nonoverlapping(data as *const u8, e.data.as_mut_ptr(), n) };
        e.len = n;
        if len > MAX_PAYLOAD {
            api::log_warn!(
                "radio: event {} payload is {} bytes; kept {}",
                id,
                len,
                MAX_PAYLOAD
            );
        }
    }

    let queued = QUEUE.with(|q| q.push(e));
    if queued {
        // Outside the queue lock would be tidier, but `with` has already
        // returned by here and the wake takes the scheduler's lock, not this
        // one. Nesting those two in this order is what `Spinlock` forbids.
        wake();
    } else {
        api::log_error!("radio: event {} dropped; the dispatch task is behind", id);
    }
    0
}

/// The address the dispatch task blocks on.
///
/// The same trick `kernel::alarm` uses: `kernel::queue`'s waiter lists are
/// keyed by address and do not care what is at it.
static WAIT_TOKEN: u8 = 0;

#[inline]
fn wait_key() -> usize {
    core::ptr::addr_of!(WAIT_TOKEN) as usize
}

fn wake() {
    kernel::queue::wake_one_receiver(wait_key());
}

/// How long the dispatch task waits before looking again.
///
/// A backstop, not the mechanism — [`post`] wakes it. It exists so a wake that
/// races the task going to sleep costs a delay rather than an event that sits
/// in the queue forever.
const IDLE_WAKE_MS: u32 = 100;

/// The dispatch task. One queue, one drainer, so order is preserved.
fn dispatch_task() {
    loop {
        let next = QUEUE.with(|q| q.pop());
        let Some(e) = next else {
            kernel::queue::block_recv(wait_key(), IDLE_WAKE_MS);
            continue;
        };
        // Read the handler per event rather than once: it may be installed or
        // replaced while this task is running, and a stale copy would call
        // something the application has already taken back.
        let handler = HANDLER.with(|h| *h);
        if let Some(f) = handler {
            let mut data = e.data;
            let ptr = if e.len == 0 {
                core::ptr::null_mut()
            } else {
                data.as_mut_ptr() as *mut c_void
            };
            f(e.base as *const c_char, e.id, ptr, e.len);
        }
    }
}

/// `ESP_TASKD_EVENT_PRIO` — esp-idf's own priority for the event task.
///
/// `ESP_TASK_PRIO_MAX - 5`, which is 20: below the Wi-Fi task at 23 and below
/// the timer service at 22, above anything an application would run at. Taken
/// from `esp_task.h` rather than chosen.
const ESP_TASKD_EVENT_PRIO: u32 = 20;

const EVENT_PRIORITY: hal::types::Priority =
    crate::adapter::priority_from_freertos(ESP_TASKD_EVENT_PRIO);

/// Stack for the dispatch task.
///
/// esp-idf's `CONFIG_ESP_SYSTEM_EVENT_TASK_STACK_SIZE` is 2304, but its
/// handlers are small. Handlers here call back into `esp_wifi_*` — that is the
/// point of the queue — and a scan handler that copies out AP records has an
/// array on its stack. 8 KiB, because a silent overflow on a kernel with no
/// MPU costs more to find than the memory costs to reserve.
const EVENT_STACK: usize = 8192;

/// Whether [`start`] has already spawned the task.
static STARTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Start the dispatch task. Idempotent.
///
/// Called from [`crate::wifi::init`], for the reason `ets_timer::start` is: a
/// service the adapter depends on should not be each application's job to
/// remember, and init is the only place that can guarantee "before the blob
/// posts anything".
pub fn start() {
    use core::sync::atomic::Ordering;
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    api::task::spawn("radio-event", dispatch_task, EVENT_PRIORITY, EVENT_STACK);
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(r: &mut Ring) -> usize {
        let mut n = 0;
        while r.pop().is_some() {
            n += 1;
        }
        n
    }

    fn event(id: i32) -> Event {
        Event { base: 1, id, len: 0, data: [0; MAX_PAYLOAD] }
    }

    #[test]
    fn the_ring_is_first_in_first_out_and_wraps() {
        // Order is the property Zephyr's comment calls out -- "a single queue
        // drained by one task preserves event order" -- and the wrap is where
        // a hand-written ring gets it wrong.
        let mut r = Ring::new();
        for round in 0..3i32 {
            for i in 0..QUEUE_LEN as i32 {
                assert!(r.push(event(round * 100 + i)), "round {round} item {i}");
            }
            for i in 0..QUEUE_LEN as i32 {
                assert_eq!(r.pop().map(|e| e.id), Some(round * 100 + i));
            }
            assert!(r.pop().is_none());
        }
    }

    #[test]
    fn a_full_queue_refuses_and_counts_rather_than_overwriting() {
        // Overwriting the oldest would lose the event the application is
        // waiting for and keep one it has already seen. Refusing loses the
        // newest, which is at least the one the log names.
        let mut r = Ring::new();
        for i in 0..QUEUE_LEN as i32 {
            assert!(r.push(event(i)));
        }
        assert!(!r.push(event(99)));
        assert!(!r.push(event(100)));
        assert_eq!(r.dropped, 2);
        // The queued ones are untouched and still in order.
        for i in 0..QUEUE_LEN as i32 {
            assert_eq!(r.pop().map(|e| e.id), Some(i));
        }
    }

    #[test]
    fn a_payload_is_copied_and_a_long_one_is_truncated() {
        // The driver's buffer is valid for the call only. Storing the pointer
        // is the bug the OSI table pointer already caused once.
        let payload: [u8; 8] = [1, 2, 3, 4, 5, 6, 7, 8];
        let rc = unsafe {
            post(
                core::ptr::null(),
                7,
                payload.as_ptr() as *mut c_void,
                payload.len(),
                0,
            )
        };
        assert_eq!(rc, 0);
        let got = QUEUE.with(|q| q.pop()).expect("queued");
        assert_eq!(got.id, 7);
        assert_eq!(got.len, 8);
        assert_eq!(&got.data[..8], &payload);

        let long = [0xABu8; MAX_PAYLOAD + 16];
        unsafe {
            post(core::ptr::null(), 8, long.as_ptr() as *mut c_void, long.len(), 0);
        }
        let got = QUEUE.with(|q| q.pop()).expect("queued");
        assert_eq!(got.len, MAX_PAYLOAD, "truncated, not dropped");
        assert_eq!(got.data[MAX_PAYLOAD - 1], 0xAB);
        QUEUE.with(|q| {
            drain(q);
            q.dropped = 0;
        });
    }

    #[test]
    fn a_full_queue_still_reports_success_to_the_driver() {
        // The driver treats a failed post as a fault worth retrying, and a
        // consumer that is behind is not something it can act on.
        QUEUE.with(|q| {
            drain(q);
            q.dropped = 0;
            for i in 0..QUEUE_LEN as i32 {
                q.push(event(i));
            }
        });
        let rc = unsafe { post(core::ptr::null(), 1, core::ptr::null_mut(), 0, 0) };
        assert_eq!(rc, 0, "a full queue must not look like a failure");
        assert_eq!(dropped(), 1);
        QUEUE.with(|q| {
            drain(q);
            q.dropped = 0;
        });
    }

    #[test]
    fn a_handler_replaces_rather_than_stacks() {
        fn a(_: *const c_char, _: i32, _: *mut c_void, _: usize) {}
        fn b(_: *const c_char, _: i32, _: *mut c_void, _: usize) {}
        assert!(set_handler(Some(a)).is_none());
        assert!(set_handler(Some(b)).is_some());
        assert!(set_handler(None).is_some());
        assert!(set_handler(None).is_none());
    }
}
