// SPDX-License-Identifier: Apache-2.0

//! Restricted API for opt-in unprivileged compute tasks. All task code and
//! constants must be in the user linker sections; these wrappers must inline.
//! The ordinary task/IPC/driver APIs are privileged and are NOT callable here.
//! No user pointers or callbacks are passed to the kernel.

#[inline(always)]
#[cfg(all(target_arch = "arm", target_os = "none"))]
fn call(operation: u32) -> u32 {
    let mut result = operation;
    // SVC preserves the stacked registers other than its r0 result. Deliberate
    // memory clobber: compiler must not move private-memory accesses across it.
    unsafe { core::arch::asm!("svc 1", inout("r0") result) };
    result
}

#[cfg(all(target_arch = "arm", target_os = "none"))]
#[inline(always)]
pub fn yield_now() {
    let _ = call(0);
}
#[cfg(all(target_arch = "arm", target_os = "none"))]
#[inline(always)]
pub fn exit() -> ! {
    let _ = call(1);
    loop {
        core::hint::spin_loop();
    }
}
#[cfg(all(target_arch = "arm", target_os = "none"))]
#[inline(always)]
pub fn current_id() -> u32 {
    call(2)
}
#[cfg(all(target_arch = "arm", target_os = "none"))]
#[inline(always)]
pub fn current_core() -> u32 {
    call(3)
}
#[cfg(all(target_arch = "arm", target_os = "none"))]
#[inline(always)]
pub fn ticks() -> u32 {
    call(4)
}
#[cfg(all(target_arch = "arm", target_os = "none"))]
#[inline(always)]
pub fn data_base() -> *mut u8 {
    call(5) as *mut u8
}
#[cfg(all(target_arch = "arm", target_os = "none"))]
#[inline(always)]
pub fn data_size() -> u32 {
    call(6)
}
