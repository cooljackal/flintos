//! Console output bridge — wires the debug/log system to the UART.
//!
//! Provides `Console` (a `core::fmt::Write` implementation) and the
//! `print!`/`println!` macros for kernel-internal use.

use core::fmt::{self, Write};
use crate::startup;

/// A fmt::Write implementation that outputs to the console UART.
pub struct Console;

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        startup::console_write(s.as_bytes());
        Ok(())
    }
}

/// Print a formatted string to the console.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        let _ = write!($crate::debug::console::Console, $($arg)*);
    };
}

/// Print a formatted string with a newline.
#[macro_export]
macro_rules! println {
    () => {
        let _ = writeln!($crate::debug::console::Console);
    };
    ($($arg:tt)*) => {
        let _ = writeln!($crate::debug::console::Console, $($arg)*);
    };
}
