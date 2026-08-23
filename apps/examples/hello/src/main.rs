// SPDX-License-Identifier: Apache-2.0

//! The smallest useful FlintOS application: one task, logging on a timer.
//!
//! Copy this directory to start your own. Everything you need to change is in
//! this file and `Cargo.toml`.
//!
//! Next: `demo` — three tasks at three priorities, which is also the workload
//! the kernel is verified against.

#![no_std]
#![no_main]

use api::prelude::*;

// Names `main` below as this build's application entry point. The kernel calls
// it once the console, tick timer and idle task are up, and unmasks interrupts
// when it returns.
kernel::flint_app!(main, abi = 1);

fn main() {
    Task::new("hello", hello).spawn().expect("spawn");
}

fn hello() {
    let mut n = 0u32;
    loop {
        n += 1;
        log_info!("n={n}");
        sleep_ms(1000);
    }
}
