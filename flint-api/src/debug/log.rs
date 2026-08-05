// SPDX-License-Identifier: Apache-2.0

//! Logging macros and severity levels.
//!
//! # Macros
//!
//! | Macro           | Level   | Gate           |
//! |-----------------|---------|----------------|
//! | `log_error!`    | Error   | Always         |
//! | `log_warn!`     | Warn    | Always         |
//! | `log_info!`     | Info    | Always         |
//! | `log_debug!`    | Debug   | `flint-log`    |
//! | `log_trace!`    | Trace   | `flint-trace`  |
//!
//! # Example
//!
//! ```ignore
//! log_info!("boot complete in {} ms", 42);
//! ```

/// Log an error message.
#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {
        $crate::debug::log::__flint_log(flint_api::debug::log::Level::Error, format_args!($($arg)*))
    };
}

/// Log a warning message.
#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::debug::log::__flint_log(flint_api::debug::log::Level::Warn, format_args!($($arg)*))
    };
}

/// Log an info message.
#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::debug::log::__flint_log(flint_api::debug::log::Level::Info, format_args!($($arg)*))
    };
}

/// Log a debug message (requires feature `flint-log`).
#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        #[cfg(feature = "flint-log")]
        $crate::debug::log::__flint_log(flint_api::debug::log::Level::Debug, format_args!($($arg)*))
    };
}

/// Log a trace message (requires feature `flint-trace`).
#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {
        #[cfg(feature = "flint-trace")]
        $crate::debug::log::__flint_log(flint_api::debug::log::Level::Trace, format_args!($($arg)*))
    };
}

/// Log severity level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

/// The kernel-provided log write function.
pub fn __flint_log(_level: Level, _args: core::fmt::Arguments<'_>) {
    extern "Rust" {
        fn _flint_sys_log_write(level: Level, args: &core::fmt::Arguments<'_>);
    }
    #[cfg(feature = "flint-log")]
    unsafe {
        _flint_sys_log_write(_level, &_args);
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn level_values() {
        assert_eq!(Level::Error as isize, 0);
        assert_eq!(Level::Warn as isize, 1);
        assert_eq!(Level::Info as isize, 2);
        assert_eq!(Level::Debug as isize, 3);
        assert_eq!(Level::Trace as isize, 4);
    }

    #[test]
    fn level_clone_eq() {
        assert_eq!(Level::Info, Level::Info);
        assert_ne!(Level::Info, Level::Warn);
        let cloned = Level::Error;
        assert_eq!(cloned, Level::Error);
    }

    #[test]
    fn level_debug() {
        let s = std::format!("{:?}", Level::Warn);
        assert_eq!(s, "Warn");
    }
}
