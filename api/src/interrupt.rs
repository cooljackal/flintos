// SPDX-License-Identifier: Apache-2.0

//! Wiring a peripheral's interrupt to a handler.
//!
//! [`connect`] is the ergonomic form: name the peripheral source and the
//! top-half, and the kernel picks a free CPU input, routes it, registers the
//! handler and unmasks it — returning the [`CpuInt`] it landed on. A driver
//! that does not care which input it uses (almost all of them) no longer has
//! to invent a number and hope nothing else picked the same one.

pub use hal::{ConnectError, CpuInt};

/// Route a peripheral `source` to the first free CPU input, register `handler`
/// as its top-half, and unmask it. Returns which input it landed on.
///
/// The handler runs in trap context: it must be short, must not block, and
/// must acknowledge its peripheral — enabling the interrupt *at the peripheral*
/// is still the driver's job, and deliberately separate. Returns
/// [`ConnectError::NoneFree`] when every input the kernel may hand out already
/// has a handler, and [`ConnectError::Route`] when the source will not route.
///
/// # Safety
/// `handler` runs in trap context under the contract above; the kernel writes
/// the interrupt crossbar and unmasks a CPU input on your behalf. A handler
/// that blocks, is slow, or fails to acknowledge its peripheral is a wedged or
/// re-entering core, which the type system cannot catch — hence `unsafe`.
pub unsafe fn connect(source: u8, handler: fn()) -> Result<CpuInt, ConnectError> {
    extern "Rust" {
        fn _flint_sys_interrupt_connect(
            source: u8,
            handler: fn(),
        ) -> Result<CpuInt, ConnectError>;
    }
    unsafe { _flint_sys_interrupt_connect(source, handler) }
}
