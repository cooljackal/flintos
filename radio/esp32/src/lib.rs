// SPDX-License-Identifier: Apache-2.0

//! The adapter between Espressif's radio blobs and FlintOS.
//!
//! Phase 3 of `doc/plan-radio.md`, issue #65.
//!
//! # What this is
//!
//! Espressif's Wi-Fi and BLE support ships as precompiled `.a` files built
//! against FreeRTOS. They do not link against an RTOS directly — they call out
//! through [`osi::WifiOsiFuncs`], a table of 115 function pointers covering
//! tasks, queues, semaphores, event groups, allocation, interrupts and the
//! PHY. This crate fills that table in.
//!
//! Everything the object-model half needs already exists: `kernel::dynobj`
//! (#58) and `kernel::heap` (#57) were built for exactly this and are host- and
//! hardware-tested on their own. Most of what remains here is translation —
//! FreeRTOS's conventions into FlintOS's — and the translation is where the
//! bugs live, so each one is written down at the function that performs it.
//!
//! # The layer concession
//!
//! This is the only crate in the tree permitted to name `kernel`. That is a
//! deliberate, single exception recorded in `tools/check-layers.sh`: the blob
//! demands a kernel's object model, and the alternative to answering here is
//! answering everywhere. If a second crate ever wants `kernel`, that is a
//! signal to ask why rather than to widen the rule.
//!
//! # Status
//!
//! The table is generated and the object-model entries are implemented. The
//! blobs are **fetched, not vendored** — `make blobs` pulls them at pinned
//! revisions with checksums, and `build.rs` links whatever is in
//! `.blobs/esp32`, or tells you to run `make blobs` if it is empty. Without
//! that step this crate still builds and tests on its own.
//!
//! What it cannot do yet is prove the table is right; only the blob can say
//! that, and it says it by running. That is step 3.6.
//!
//! [`adapter::UNIMPLEMENTED`] lists what is still null and why.

#![no_std]

pub mod adapter;
pub mod osi;

pub use osi::{WifiOsiFuncs, IDF_VERSION, MAGIC, VERSION};
