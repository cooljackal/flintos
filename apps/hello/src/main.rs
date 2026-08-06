// SPDX-License-Identifier: Apache-2.0

//! The smallest useful Flint application: one task, logging on a timer.
//!
//! Copy this directory to start your own. Everything you need to change is in
//! this file and `Cargo.toml`.

#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

// Names `main` below as this build's application entry point. The kernel calls
// it once the console, tick timer and idle task are up, and unmasks interrupts
// when it returns.
kernel::flint_app!(main);

fn main() {
    task::spawn("hello", hello, Priority::Normal(1), 4096);
}

fn hello() {
    let mut n = 0u32;
    loop {
        n += 1;
        api::log_info!("[hello] n={}", n);
        task::sleep_ms(1000);
    }
}
