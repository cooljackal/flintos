// SPDX-License-Identifier: Apache-2.0

//! Opt-in isolated compute tasks. Ordinary tasks remain trusted. Untrusted
//! code must be linked into `.user.text` (and its constants `.user.rodata`),
//! cannot use the direct-call API, and receives no MMIO or DMA authority.
//! Only bounded, pointer-free SVC operations are admitted. The MPU does not
//! constrain DMA/debug masters or privileged code; those remain trusted.

#[cfg(target_os = "none")]
use hal::isolation::{Access, Region};
pub use hal::isolation::{Error, TaskMemory};

/// Size-preserving bump reservation. Never round a grant up to include a
/// neighbour. Both allocations succeed together, or neither consumes space.
#[cfg(any(target_os = "none", test))]
pub(crate) fn reserve(cursor: u32, end: u32, stack: u32, data: u32) -> Option<(u32, u32, u32)> {
    if !(1024..=16384).contains(&stack)
        || !stack.is_power_of_two()
        || (data != 0 && (!(256..=4096).contains(&data) || !data.is_power_of_two()))
    {
        return None;
    }
    let stack_base = cursor.checked_add(stack - 1)? & !(stack - 1);
    let stack_end = stack_base.checked_add(stack)?;
    let (data_base, next) = if data == 0 {
        (0, stack_end)
    } else {
        let base = stack_end.checked_add(data - 1)? & !(data - 1);
        (base, base.checked_add(data)?)
    };
    (next <= end).then_some((stack_base, data_base, next))
}

#[cfg(target_os = "none")]
unsafe extern "C" {
    static _user_text_start: u8;
    static _user_text_end: u8;
}

#[cfg(target_os = "none")]
fn code_region() -> Region {
    let start = core::ptr::addr_of!(_user_text_start) as u32;
    let end = core::ptr::addr_of!(_user_text_end) as u32;
    Region::new(start, end - start, Access::ReadExecute).expect("invalid isolated linker region")
}

/// Create a private-stack/private-data task; the MPU is active before the task
/// becomes runnable. Data and stacks live until reboot (no reuse or aliases).
/// `data_bytes == 0` denies all data outside the stack. The lower stack eighth
/// is a hardware guard, so usable stack is 7/8 of `stack_bytes`.
#[cfg(target_os = "none")]
pub fn spawn(
    name: &'static str,
    entry: fn(),
    priority: hal::Priority,
    stack_bytes: u32,
    data_bytes: u32,
    affinity: crate::scheduler::Affinity,
) -> Result<hal::TaskId, Error> {
    use crate::scheduler::{self, TaskState};
    use hal::arch::Architecture;
    if !arch_armv6m::mpu::available() {
        return Err(Error::Unsupported);
    }
    if affinity
        .pinned_to()
        .is_some_and(|c| !crate::smp::is_pinnable(c.0))
    {
        return Err(Error::Unsupported);
    }
    if priority.numeric() > scheduler::MAX_PUBLIC_PRIORITY {
        return Err(Error::Entry);
    }
    let code = code_region();
    if !code.usable().contains(entry as usize as u32 & !1, 2) {
        return Err(Error::Entry);
    }
    if reserve(0, u32::MAX, stack_bytes, data_bytes).is_none() {
        return Err(Error::Size);
    }
    scheduler::with(|sched| {
        let id = sched.alloc_id().ok_or(Error::Capacity)?;
        let Some((stack_base, data_base)) = crate::spawn::allocate_private(stack_bytes, data_bytes)
        else {
            sched.tasks[id as usize] = None;
            return Err(Error::Capacity);
        };
        let stack = Region::stack(stack_base, stack_bytes).expect("validated private stack");
        let data = if data_bytes == 0 {
            None
        } else {
            Some(
                Region::new(data_base, data_bytes, Access::ReadWrite)
                    .expect("validated private data"),
            )
        };
        let memory = TaskMemory::new(code, stack, data, entry as usize as u32)
            .expect("exclusive allocated regions");
        let usable = stack.usable();
        crate::spawn::paint_stack(usable.start, usable.end - usable.start);
        if let Some(data) = data {
            unsafe { core::ptr::write_bytes(data.base() as *mut u8, 0, data.size() as usize) };
        }
        let tcb = sched.tasks[id as usize].as_mut().expect("allocated TCB");
        tcb.init_common(name, entry, priority.numeric(), TaskState::Ready);
        tcb.affinity = affinity;
        tcb.stack_base = usable.start;
        tcb.stack_size = usable.end - usable.start;
        tcb.isolation = Some(memory);
        unsafe {
            crate::arch::SelectedArch::init_context(&mut tcb.context, entry as usize, usable.end);
            arch_armv6m::init_isolated_return(&tcb.context);
        }
        sched.make_ready(id);
        Ok(hal::TaskId(id))
    })
}

/// Initialize each core independently; absence is fatal before user launch.
#[cfg(target_os = "none")]
pub(crate) fn init_core() {
    assert!(
        arch_armv6m::mpu::available(),
        "task-isolation requires the eight-region MPU"
    );
    unsafe { arch_armv6m::mpu::activate(None) };
}

#[cfg(target_os = "none")]
#[derive(Clone, Copy)]
struct ActiveDomain {
    id: u32,
    memory: Option<TaskMemory>,
}

// Exactly one writer per entry: the owning core's interrupt-masked context
// switch. HardFault reads only its own core after checking EXC_RETURN. A
// user-mode fault cannot interrupt publication in handler mode. No scheduler
// lock is needed (or safe) in HardFault; the other core may be holding it.
#[cfg(target_os = "none")]
static mut ACTIVE: [ActiveDomain; 2] = [ActiveDomain {
    id: u32::MAX,
    memory: None,
}; 2];

#[cfg(all(target_os = "none", feature = "isolation-test"))]
pub static ISOLATION_ACTIVATIONS: [portable_atomic::AtomicU32; 2] =
    [const { portable_atomic::AtomicU32::new(0) }; 2];

#[cfg(target_os = "none")]
pub(crate) fn activate(id: u32, memory: Option<TaskMemory>) {
    unsafe {
        let slot = (&raw mut ACTIVE)
            .cast::<ActiveDomain>()
            .add(crate::smp::current_core().index());
        let previous = slot.read_volatile();
        slot.write_volatile(ActiveDomain { id, memory });
        #[cfg(feature = "isolation-test")]
        if memory.is_some() {
            use portable_atomic::Ordering;
            let count = &ISOLATION_ACTIVATIONS[crate::smp::current_core().index()];
            // One writer on this core with interrupts masked; no CAS emulation.
            count.store(
                count.load(Ordering::Relaxed).wrapping_add(1),
                Ordering::Relaxed,
            );
        }
        // Trusted-to-trusted switches have identical denied user maps. Avoid
        // reprogramming all eight slots when no grant changes. Every change
        // of user domain still replaces the entire bank, including empty slots.
        if previous.memory != memory {
            arch_armv6m::mpu::activate(memory);
        }
        arch_armv6m::set_thread_unprivileged(memory.is_some());
    }
}

#[cfg(target_os = "none")]
#[no_mangle]
extern "C" fn _flint_armv6m_validate_psp(psp: u32) -> u32 {
    let active = unsafe {
        (&raw const ACTIVE)
            .cast::<ActiveDomain>()
            .add(crate::smp::current_core().index())
            .read_volatile()
    };
    let valid = active.memory.is_none_or(|m| m.exception_frame(psp));
    assert!(valid, "isolated task supplied invalid exception stack");
    psp
}

/// Supervisor ABI: r0 is the operation; results replace r0. No pointers,
/// callbacks, DMA handles, IRQ masks or privilege transitions cross this gate.
#[cfg(target_os = "none")]
#[no_mangle]
unsafe extern "C" fn _flint_armv6m_user_svc(frame: *mut u32, exc_return: u32) {
    assert_eq!(
        exc_return, 0xffff_fffd,
        "user SVC must return to thread PSP"
    );
    let mut switch = false;
    crate::scheduler::with(|s| {
        let current = s.current();
        let memory = s.tasks[current as usize]
            .as_ref()
            .and_then(|t| t.isolation)
            .expect("user SVC from trusted task");
        assert!(
            memory.exception_frame(frame as u32),
            "invalid user SVC stack"
        );
        let operation = unsafe { frame.read() };
        let result = match operation {
            0 => {
                switch = s.yield_current();
                0
            }
            1 => {
                s.block_current(crate::scheduler::TaskState::Suspended);
                switch = true;
                0
            }
            2 => current,
            3 => u32::from(crate::smp::current_core().0),
            4 => s.ticks() as u32,
            5 => memory.data().map_or(0, |r| r.base()),
            6 => memory.data().map_or(0, |r| r.size()),
            _ => u32::MAX,
        };
        unsafe { frame.write(result) };
    });
    if switch {
        crate::scheduler::request_switch();
    }
}

/// Retained per-core exception evidence. ARMv6-M has no CFSR/MMFAR: a random
/// HardFault is NOT labelled an MPU violation. Tests may identify a known
/// denied instruction; production records only the observed unprivileged
/// HardFault, task, PC, stack and active region bases.
#[cfg(target_os = "none")]
#[no_mangle]
#[link_section = ".uninit.isolation_fault"]
pub static mut FLINT_ISOLATION_FAULT: [[u32; 12]; 2] = [[0; 12]; 2];

#[cfg(target_os = "none")]
#[no_mangle]
unsafe extern "C" fn _flint_armv6m_isolation_fault(frame: *const u32, exc_return: u32) -> bool {
    if exc_return != 0xffff_fffd {
        return false;
    }
    let core = crate::smp::current_core().index();
    let task = unsafe {
        (&raw const ACTIVE)
            .cast::<ActiveDomain>()
            .add(core)
            .read_volatile()
    };
    let id = task.id;
    let Some(memory) = task.memory else {
        return false;
    };
    // A nested exception/invalid stack is never an expected user fault. Do
    // not read a frame merely because a CPU register points at it.
    if exc_return != 0xffff_fffd || !memory.exception_frame(frame as u32) {
        crate::debug::panic::handle(&format_args!("isolated task {id}: invalid fault stack"));
    }
    let pc = unsafe { frame.add(6).read() };
    unsafe {
        core::ptr::write_volatile(
            (&raw mut FLINT_ISOLATION_FAULT)
                .cast::<[u32; 12]>()
                .add(core),
            [
                0x139f_a017,
                id,
                core as u32,
                frame as u32,
                pc,
                frame.add(7).read(),
                exc_return,
                memory.code().base(),
                memory.stack().base(),
                memory.data().map_or(0, |r| r.base()),
                frame.read(),
                frame.add(1).read(),
            ],
        );
    }
    #[cfg(feature = "isolation-test")]
    {
        unsafe extern "C" {
            fn _flint_isolation_test_fault(id: u32, frame: *mut u32) -> bool;
        }
        if unsafe { _flint_isolation_test_fault(id, frame.cast_mut()) } {
            return true;
        }
    }
    crate::debug::panic::handle_trap(
        &format_args!("unprivileged HardFault task={id} pc={pc:08x}"),
        hal::arch::ContextDiagnostics {
            pc,
            architecture_state: frame as u32,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reservation_is_exclusive_aligned_and_exact() {
        let (stack, data, next) = reserve(0x2000_0410, 0x2001_0000, 4096, 1024).unwrap();
        assert_eq!((stack, data, next), (0x2000_1000, 0x2000_2000, 0x2000_2400));
        let second = reserve(next, 0x2001_0000, 2048, 256).unwrap();
        assert!(second.0 >= next);
    }
    #[test]
    fn reservations_fail_without_rounding_or_wrapping() {
        for size in [0, 512, 1025, 32768, u32::MAX] {
            assert!(reserve(0, u32::MAX, size, 0).is_none());
        }
        for size in [1, 128, 257, 8192, u32::MAX] {
            assert!(reserve(0, u32::MAX, 1024, size).is_none());
        }
        assert!(reserve(0xffff_ffff, u32::MAX, 1024, 0).is_none());
        assert!(reserve(0x2000_0000, 0x2000_1100, 4096, 512).is_none());
        assert_eq!(
            reserve(0x2000_0000, 0x2000_1000, 4096, 0),
            Some((0x2000_0000, 0, 0x2000_1000))
        );
    }
}
