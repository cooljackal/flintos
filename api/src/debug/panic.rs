// SPDX-License-Identifier: Apache-2.0

/// Trigger a panic from user code.
/// In release builds, this performs a minimal reset.
/// In debug builds, it captures a full postmortem snapshot.
#[macro_export]
macro_rules! flint_panic {
    ($($arg:tt)*) => {
        $crate::debug::panic::__flint_panic(format_args!($($arg)*))
    };
}

/// Internal panic trigger — calls the kernel's registered panic handler.
pub fn __flint_panic(args: core::fmt::Arguments<'_>) -> ! {
    extern "Rust" {
        fn _flint_sys_panic(args: &core::fmt::Arguments<'_>) -> !;
    }
    unsafe { _flint_sys_panic(&args) }
}
