// SPDX-License-Identifier: Apache-2.0

//! `_task_create`, `_task_create_pinned_to_core` and `_task_delete` — the
//! blob's own threads.
//!
//! Three entries, and the reason `esp_wifi_init_internal` could not return.
//! The Wi-Fi driver creates `wifiT` before it does anything else, so with
//! these null the first call into the blob jumped to address zero — which is
//! what `EXCCAUSE=0x14 epc=0x00000000` was saying all along.
//!
//! # The mismatch, and the trampolines
//!
//! Same shape as [`crate::interrupts`], for the same reason. FreeRTOS's task
//! entry is `void (*)(void *param)`; `kernel::dynobj::spawn_task` takes a
//! plain `fn()`, which carries no environment, and a closure cannot bridge
//! that. What can is a *distinct function per slot*, each knowing its own slot
//! number at compile time — [`TRAMPOLINES`] is [`SLOTS`] different `fn()`
//! values, each of which looks up its slot and calls what the blob asked for
//! with the argument the blob supplied.
//!
//! [`SLOTS`] is the ceiling on how many blob tasks may exist at once. It is a
//! fixed array rather than a growable one because the trampolines have to be
//! written out by hand — there is no way to generate `fn()` items at runtime —
//! and because the number of tasks the driver creates is a property of the
//! driver, not of the workload.
//!
//! # Handles are ids plus one
//!
//! FreeRTOS's `TaskHandle_t` is a TCB pointer, and a **null handle means "the
//! calling task"** — `vTaskDelete(NULL)` deletes yourself. FlintOS's task ids
//! start at zero, so handing an id out raw would make task 0 indistinguishable
//! from "myself", and the blob would delete the wrong thread exactly once, at
//! a point far from here.
//!
//! So a handle is `id + 1`. [`handle_from_id`] and [`id_from_handle`] are the
//! only two places that know it, and `_task_get_current_task` uses them too —
//! the blob compares the handle it was given against the one it stored, and
//! two encodings would never match.
//!
//! # Stack sizes are bytes
//!
//! Vanilla FreeRTOS counts `usStackDepth` in *words*. The ESP-IDF port counts
//! it in bytes, and the blob is built against the ESP-IDF port: NuttX passes
//! `stack_depth` straight to `kthread_create`, which is bytes. Treating it as
//! words would give the Wi-Fi task four times the stack it asked for, and
//! `kernel::spawn` would reject it against `MAX_STACK_SIZE` — a failure that
//! reads like "out of memory" rather than like a units bug.

use core::cell::UnsafeCell;
use core::ffi::{c_char, c_void};

use kernel::dynobj;
use kernel::scheduler::Affinity;

use crate::adapter::{priority_from_freertos, PD_FALSE, PD_TRUE};

/// How many blob tasks may exist at once.
///
/// The Wi-Fi driver creates one (`wifiT`); the timer and event machinery adds
/// a couple more, and BLE will want its own when #66 lands. Eight is room for
/// all of that with the array still small enough to write out below.
pub const SLOTS: usize = 8;

/// Room for the blob's task name, without its NUL.
///
/// FreeRTOS's own limit is `configMAX_TASK_NAME_LEN`, 16 including the
/// terminator, and Espressif's names are all shorter than that.
const NAME_CAP: usize = 15;

/// No task has claimed this slot yet.
///
/// Not zero: zero is a real task id, and the slot's id is written by whichever
/// of [`create`] and the trampoline gets there first — see the note in
/// [`create`].
const NO_TASK: u32 = u32::MAX;

/// What the blob asked to run, and what became of it.
#[derive(Clone, Copy)]
struct Slot {
    /// The blob's entry point. `*mut c_void` is not `Send`; the argument is
    /// held as a `usize` and cast at the point of call, as in
    /// [`crate::interrupts`].
    entry: Option<unsafe extern "C" fn(*mut c_void)>,
    arg: usize,
    /// The FlintOS task id, once there is one.
    task: u32,
    used: bool,
}

impl Slot {
    const FREE: Self = Slot { entry: None, arg: 0, task: NO_TASK, used: false };
}

/// The slots.
///
/// Behind the kernel's `Spinlock` for the reason [`crate::interrupts`] gives:
/// written by whichever core calls `_task_create` and read by the task itself,
/// which may be running on the other one. A critical section masks the calling
/// core alone and would not cover that.
static SLOTS_TABLE: kernel::smp::Spinlock<[Slot; SLOTS]> =
    kernel::smp::Spinlock::new([Slot::FREE; SLOTS]);

/// The task names, in storage that outlives the tasks.
///
/// `kernel::spawn` stores the name as a `&'static str` in the TCB rather than
/// copying it, so the bytes have to live somewhere permanent. The blob's own
/// pointer very probably qualifies — its names are string literals in the
/// archive's `.rodata` — but "very probably" is the wrong standard for
/// something the kernel will dereference whenever a task list is printed, so
/// the name is copied here instead.
///
/// The slot's buffer is written only while the slot is being claimed, and the
/// slot is not released until the task that owns it is deleted, so the
/// `&'static str` handed to the kernel never sees the bytes change under it.
struct Names(UnsafeCell<[[u8; NAME_CAP]; SLOTS]>);

// SAFETY: see above -- one writer, at claim time, for a slot no other core can
// be holding a name from.
unsafe impl Sync for Names {}

static NAMES: Names = Names(UnsafeCell::new([[0; NAME_CAP]; SLOTS]));

/// The name given to a task whose own name did not survive the copy.
const FALLBACK_NAME: &str = "wifi-blob";

/// A FlintOS task id as the blob's `TaskHandle_t`.
pub fn handle_from_id(id: u32) -> *mut c_void {
    (id as usize + 1) as *mut c_void
}

/// The blob's `TaskHandle_t` as a FlintOS task id, or `None` for null — which
/// FreeRTOS defines as "the calling task".
pub fn id_from_handle(handle: *mut c_void) -> Option<u32> {
    let raw = handle as usize;
    if raw == 0 {
        None
    } else {
        u32::try_from(raw - 1).ok()
    }
}

/// Copy the blob's task name into slot `n`'s buffer and borrow it back.
///
/// # Safety
/// `name` is the blob's, and is read up to its NUL or [`NAME_CAP`] bytes,
/// whichever comes first. Slot `n` must be claimed by the caller.
unsafe fn store_name(n: usize, name: *const c_char) -> &'static str {
    let base = unsafe { (*NAMES.0.get()).as_mut_ptr().add(n) as *mut u8 };
    let mut len = 0;
    if !name.is_null() {
        while len < NAME_CAP {
            let b = unsafe { name.add(len).read() } as u8;
            if b == 0 {
                break;
            }
            unsafe { base.add(len).write(b) };
            len += 1;
        }
    }
    if len == 0 {
        // A null or empty name is valid UTF-8, so this has to be checked
        // separately from the conversion below. An unnamed task in a task list
        // is as unhelpful as a corrupted one.
        return FALLBACK_NAME;
    }
    let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(base, len) };
    // A truncated multi-byte character would leave invalid UTF-8 behind, and a
    // name is not worth an unchecked conversion the kernel then prints.
    core::str::from_utf8(bytes).unwrap_or(FALLBACK_NAME)
}

/// Which core to put a task on, given what the driver asked for.
///
/// The case this exists for: **a request for a core that is not running.**
/// This chip has two, and applications that use one leave the second held in
/// reset. Pinning a task there is not an error anyone reports — the task is
/// created, success goes back to the driver, and it then waits forever for a
/// thread that can never be scheduled. Sending it wherever it *can* run is the
/// only useful answer, and it is what esp-idf arrives at from the other
/// direction: its "which core" setting is offered only when the second core
/// is enabled (`components/esp_wifi/Kconfig:184-190`), so a single-core build
/// cannot ask for the second core in the first place.
///
/// Not currently reached — measured, the driver asks for core 0, which is
/// also what this tree tells it to use. It is here because the cost of being
/// wrong is a hang with nothing pointing at it.
fn affinity_for(core_id: u32, second_core_running: bool) -> Affinity {
    match u8::try_from(core_id) {
        Ok(0) => Affinity::Core(hal::smp::CoreId(0)),
        Ok(c) if (c as usize) < hal::smp::MAX_CORES && second_core_running => {
            Affinity::Core(hal::smp::CoreId(c))
        }
        // Either no preference (`tskNO_AFFINITY` is too large for a `u8`), or
        // a core that cannot run it.
        _ => Affinity::Any,
    }
}

/// Take a free slot, or `None` if all [`SLOTS`] are in use.
fn claim() -> Option<usize> {
    SLOTS_TABLE.with(|t| {
        let n = t.iter().position(|s| !s.used)?;
        t[n] = Slot { entry: None, arg: 0, task: NO_TASK, used: true };
        Some(n)
    })
}

// ── What the blob asked for, and what happened ──────────────────────────────

/// One `_task_create` request, as it arrived and as it was resolved.
///
/// Recorded rather than logged: this runs on the blob's init path, and
/// logging from inside the blob has hung the board before. The application
/// reads it back afterwards.
///
/// The question it exists to answer: **does the driver ever ask for the
/// second core?** That core is not started in this application
/// (`soc_esp32::appcpu::is_running()` is false, measured), and
/// [`create`] maps a request for it onto `Affinity::Core(1)` because
/// `MAX_CORES` is 2 — a task pinned to a core that never runs is created
/// successfully, reports success to the blob, and then sits there. That would
/// explain both the init hang and the receiver never being switched on, and
/// it is entirely unconfirmed until one of these records shows `core_id` of 1.
#[derive(Clone, Copy)]
pub struct Created {
    pub core_id: u32,
    pub prio: u32,
    pub stack: u32,
    /// `Some(n)` for a specific core, `None` for no preference.
    pub pinned_to: Option<u8>,
    pub slot: u8,
    pub spawned: bool,
}

/// More than [`SLOTS`], because a refused request is worth seeing too and
/// those never occupy a slot.
const MAX_CREATES: usize = 16;

static CREATES: kernel::smp::Spinlock<([Option<Created>; MAX_CREATES], usize)> =
    kernel::smp::Spinlock::new(([None; MAX_CREATES], 0));

fn note_create(c: Created) {
    CREATES.with(|(table, n)| {
        if *n < MAX_CREATES {
            table[*n] = Some(c);
            *n += 1;
        }
    });
}

/// Every task the blob has asked for, in order.
pub fn for_each_create(mut f: impl FnMut(&Created)) {
    CREATES.with(|(table, n)| {
        for c in table[..*n].iter().flatten() {
            f(c);
        }
    });
}

#[allow(clippy::declare_interior_mutable_const)]
const ZERO: core::sync::atomic::AtomicU32 = core::sync::atomic::AtomicU32::new(0);

/// Times each slot's trampoline has been entered, and returned from.
///
/// A slot created but never entered is a task that was never scheduled, which
/// is the whole point of the exercise.
static ENTERED: [core::sync::atomic::AtomicU32; SLOTS] = [ZERO; SLOTS];
static EXITED: [core::sync::atomic::AtomicU32; SLOTS] = [ZERO; SLOTS];

/// Entered and exited counts for slot `n`.
pub fn slot_counts(n: usize) -> (u32, u32) {
    use core::sync::atomic::Ordering;
    if n >= SLOTS {
        return (0, 0);
    }
    (ENTERED[n].load(Ordering::Relaxed), EXITED[n].load(Ordering::Relaxed))
}

/// The trap-free half for slot `N`: what the kernel actually starts.
fn trampoline<const N: usize>() {
    ENTERED[N].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    // Record the id here as well as in `create`, because on two cores the task
    // can be picked up and run before `spawn_task_on` has even returned to the
    // caller. Whoever arrives first wins and both write the same value.
    let slot = SLOTS_TABLE.with(|t| {
        if t[N].task == NO_TASK {
            t[N].task = dynobj::current_task();
        }
        t[N]
    });
    if let Some(f) = slot.entry {
        unsafe { f(slot.arg as *mut c_void) };
    }
    // A FreeRTOS task entry never returns; the port faults if one does. Reach
    // this and something in the blob is wrong, so say which task and take the
    // same exit `_task_delete` would rather than run off the end of the stack.
    EXITED[N].fetch_add(1, core::sync::atomic::Ordering::Relaxed);
    api::log_error!("radio: blob task '{}' returned from its entry point", slot.task);
    release_slot_of(slot.task);
    dynobj::delete_self();
}

/// Free whichever slot belongs to task `id`, and the scratch semaphore the
/// blob may have taken out in its name. A no-op if it has neither.
///
/// The semaphore matters as much as the slot: task ids are recycled, and one
/// left behind would be handed to whatever task next lands on this id — see
/// [`crate::adapter::forget_thread_semaphore`].
fn release_slot_of(id: u32) {
    crate::adapter::forget_thread_semaphore(id);
    SLOTS_TABLE.with(|t| {
        for s in t.iter_mut() {
            if s.used && s.task == id {
                *s = Slot::FREE;
            }
        }
    });
}

/// One distinct `fn()` per slot, which is the whole trick.
static TRAMPOLINES: [fn(); SLOTS] = [
    trampoline::<0>,
    trampoline::<1>,
    trampoline::<2>,
    trampoline::<3>,
    trampoline::<4>,
    trampoline::<5>,
    trampoline::<6>,
    trampoline::<7>,
];

/// `_task_create_pinned_to_core(entry, name, stack_depth, param, prio,
/// task_handle, core_id)`.
///
/// `task_handle` is an out-parameter, and may be null when the caller does not
/// want one back. `core_id` is FreeRTOS's `tskNO_AFFINITY` (`0xFFFFFFFF`) or
/// any out-of-range value when the task may float, which is how
/// [`create_anywhere`] is written in terms of this.
///
/// Returns `pdTRUE`/`pdFALSE`, not an id — the id goes through `task_handle`.
///
/// # Safety
/// `entry` is called on a new task with `param`. `name` is a C string.
/// `task_handle`, if non-null, is written with one `TaskHandle_t`.
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn create(
    entry: *mut c_void,
    name: *const c_char,
    stack_depth: u32,
    param: *mut c_void,
    prio: u32,
    task_handle: *mut c_void,
    core_id: u32,
) -> i32 {
    crate::adapter::calls::bump(crate::adapter::calls::TASK_CREATE);
    if entry.is_null() {
        api::log_error!("radio: _task_create with a null entry point");
        return PD_FALSE;
    }
    let Some(n) = claim() else {
        api::log_error!("radio: _task_create with all {} blob task slots in use", SLOTS);
        return PD_FALSE;
    };

    // Everything the trampoline reads has to be in place before the task can
    // be scheduled, so the entry and argument are written first and the spawn
    // comes after.
    let task_name = unsafe { store_name(n, name) };
    let f = unsafe {
        core::mem::transmute::<*mut c_void, unsafe extern "C" fn(*mut c_void)>(entry)
    };
    SLOTS_TABLE.with(|t| {
        t[n].entry = Some(f);
        t[n].arg = param as usize;
    });

    // SAFETY: a register read, and the answer is only used to choose an
    // affinity — a core that starts between here and the spawn costs nothing.
    #[cfg(target_os = "none")]
    let second_core = unsafe { soc_esp32::appcpu::is_running() };
    #[cfg(not(target_os = "none"))]
    let second_core = false;
    let affinity = affinity_for(core_id, second_core);
    let spawned = dynobj::spawn_task_on(
        task_name,
        TRAMPOLINES[n],
        priority_from_freertos(prio),
        stack_depth as usize,
        affinity,
    );

    note_create(Created {
        core_id,
        prio,
        stack: stack_depth,
        pinned_to: match affinity {
            Affinity::Core(c) => Some(c.0),
            _ => None,
        },
        slot: n as u8,
        spawned: spawned.is_some(),
    });

    match spawned {
        Some(id) => {
            SLOTS_TABLE.with(|t| {
                if t[n].task == NO_TASK {
                    t[n].task = id;
                }
            });
            if !task_handle.is_null() {
                unsafe { (task_handle as *mut *mut c_void).write(handle_from_id(id)) };
            }
            PD_TRUE
        }
        None => {
            // No TCB slot, no heap, or a stack size outside the kernel's
            // bounds. Say which numbers were asked for: the usual cause is the
            // last of those, and the size is the only clue to it.
            api::log_error!(
                "radio: could not create blob task '{}' with a {}-byte stack",
                task_name,
                stack_depth
            );
            SLOTS_TABLE.with(|t| t[n] = Slot::FREE);
            PD_FALSE
        }
    }
}

/// `_task_create(entry, name, stack_depth, param, prio, task_handle)`.
///
/// esp-idf's is `xTaskCreate`, which is `xTaskCreatePinnedToCore` with
/// `tskNO_AFFINITY`. Written the same way here rather than duplicated.
///
/// # Safety
/// As [`create`].
pub unsafe extern "C" fn create_anywhere(
    entry: *mut c_void,
    name: *const c_char,
    stack_depth: u32,
    param: *mut c_void,
    prio: u32,
    task_handle: *mut c_void,
) -> i32 {
    unsafe { create(entry, name, stack_depth, param, prio, task_handle, u32::MAX) }
}

/// `_task_delete(handle)`. A null handle deletes the calling task.
///
/// The slot is released before the task is, because after `delete_task` the id
/// belongs to the allocator again and a later create could be handed it — at
/// which point a slot still claiming that id would be freed out from under a
/// live task.
///
/// # Safety
/// `handle` is one this module handed out, or null. Called by the blob.
pub unsafe extern "C" fn delete(handle: *mut c_void) {
    let id = id_from_handle(handle).unwrap_or_else(dynobj::current_task);
    release_slot_of(id);
    // `delete_task` routes a self-delete to `delete_self`, which does not
    // return -- so there is nothing after this call to get wrong.
    if let Err(e) = dynobj::delete_task(id) {
        api::log_error!("radio: _task_delete({}) refused: {:?}", id, e);
    }
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_never_goes_to_a_core_that_is_not_running() {
        // The whole point. Asking for the second core while it is held in
        // reset used to pin the task there, and a task pinned to a core that
        // never runs is created, reported as a success, and then waits for
        // ever.
        assert_eq!(affinity_for(1, false), Affinity::Any);
        assert_eq!(affinity_for(1, true), Affinity::Core(hal::smp::CoreId(1)));
    }

    #[test]
    fn core_zero_is_always_core_zero() {
        assert_eq!(affinity_for(0, false), Affinity::Core(hal::smp::CoreId(0)));
        assert_eq!(affinity_for(0, true), Affinity::Core(hal::smp::CoreId(0)));
    }

    #[test]
    fn no_preference_stays_no_preference() {
        // FreeRTOS spells "anywhere" as a value far too large for a core
        // number, and `_task_create` passes u32::MAX for the same reason.
        assert_eq!(affinity_for(0x7FFF_FFFF, true), Affinity::Any);
        assert_eq!(affinity_for(u32::MAX, true), Affinity::Any);
    }

    #[test]
    fn a_core_this_chip_does_not_have_is_not_invented() {
        assert_eq!(affinity_for(2, true), Affinity::Any);
    }

    use super::*;

    #[test]
    fn there_is_one_trampoline_per_slot() {
        // Written out by hand, so the thing that goes wrong is a duplicated
        // index -- `trampoline::<3>` twice would run two blob tasks from one
        // slot, each seeing the other's argument.
        assert_eq!(TRAMPOLINES.len(), SLOTS);
        for (i, a) in TRAMPOLINES.iter().enumerate() {
            for (j, b) in TRAMPOLINES.iter().enumerate().skip(i + 1) {
                assert!(
                    !core::ptr::fn_addr_eq(*a, *b),
                    "blob task slots {i} and {j} share a trampoline"
                );
            }
        }
    }

    #[test]
    fn a_null_handle_is_the_calling_task_and_task_zero_is_not() {
        // The whole reason handles are offset by one. If these ever compare
        // equal, `vTaskDelete(NULL)` and "delete task 0" become the same call.
        assert_eq!(id_from_handle(core::ptr::null_mut()), None);
        assert_eq!(id_from_handle(handle_from_id(0)), Some(0));
        assert!(!handle_from_id(0).is_null());
    }

    #[test]
    fn a_handle_round_trips() {
        for id in [0u32, 1, 7, 63, 1000] {
            assert_eq!(id_from_handle(handle_from_id(id)), Some(id));
        }
    }

    #[test]
    fn a_name_is_copied_and_truncated_rather_than_borrowed() {
        let short = b"wifiT\0";
        let name = unsafe { store_name(0, short.as_ptr() as *const c_char) };
        assert_eq!(name, "wifiT");

        // Longer than the buffer: truncated, not overrun. The assertion that
        // matters is the length -- an overrun would land in the next slot's
        // name and only show up as a corrupted task list.
        let long = b"a-name-far-longer-than-the-buffer\0";
        let name = unsafe { store_name(1, long.as_ptr() as *const c_char) };
        assert_eq!(name.len(), NAME_CAP);
        assert_eq!(name, "a-name-far-long");

        // And slot 0 still says what it said, which is what proves the two
        // buffers are separate.
        let slot0 = unsafe { (*NAMES.0.get()).as_ptr().read() };
        assert_eq!(&slot0[..5], b"wifiT");
    }

    #[test]
    fn a_null_name_falls_back_rather_than_dereferencing() {
        let name = unsafe { store_name(2, core::ptr::null()) };
        assert_eq!(name, FALLBACK_NAME);
    }

    #[test]
    fn slots_are_handed_out_once_and_returned() {
        // Drain the table, then check it refuses rather than wrapping onto a
        // slot that is already running something.
        let mut taken = [0usize; SLOTS];
        for t in taken.iter_mut() {
            *t = claim().expect("a free slot");
        }
        assert!(claim().is_none(), "handed out more slots than exist");
        for (i, a) in taken.iter().enumerate() {
            for b in taken.iter().skip(i + 1) {
                assert_ne!(a, b, "the same slot was handed out twice");
            }
        }
        // Returning them makes them available again; `release_slot_of` is what
        // `_task_delete` calls, so this is the real path.
        SLOTS_TABLE.with(|t| {
            for (n, s) in t.iter_mut().enumerate() {
                s.task = n as u32;
            }
        });
        for n in 0..SLOTS {
            release_slot_of(n as u32);
        }
        assert!(claim().is_some(), "a released slot was not reusable");
        SLOTS_TABLE.with(|t| *t = [Slot::FREE; SLOTS]);
    }
}
