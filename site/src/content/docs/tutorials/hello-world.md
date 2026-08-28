---
title: Hello, world
---


The kernel is a library. The thing you flash is an application in `apps/`. This
walks through the smallest one, `apps/examples/hello` — one task that logs a tick.

Copy `apps/examples/hello/`, rename it, add it to `members` in the workspace
`Cargo.toml`. That's the setup — `make flash APP=<name>` works from there.

An application is three files.

## `src/main.rs`

```rust
// No standard library: this is bare-metal firmware, no OS beneath.
#![no_std]
// Don't generate a normal `main`; the kernel owns the startup path
// and calls into the app itself.
#![no_main]

// One line brings in the whole everyday API: Task, log_info!,
// sleep_ms, Priority — nothing below needs its own `use`.
use api::prelude::*;

// Register the `main` below as this app's entry point. `abi = 2`
// is the app-to-kernel contract version, checked at link time so a
// stale app cannot boot against a newer kernel.
kernel::flint_app!(main, abi = 2);

// Runs once at startup: after the console, tick and idle task are
// up, but *before* interrupts unmask. Spawn tasks here, then return.
fn main() {
    // Create a task named "worker" that runs the `worker` function,
    // then start it. `.spawn()` returns `None` if the task pool is
    // full; `.expect(...)` turns that into a clear panic instead of
    // a task that silently never runs.
    Task::new("worker", worker).spawn().expect("spawn");
}

// The task body — each spawned task is its own thread of execution.
fn worker() {
    // Loop forever.
    loop {
        // Print a line over the console UART.
        log_info!("tick");
        // Sleep one second, letting other tasks run meanwhile.
        sleep_ms(1000);
    }
}
```

`main` runs once, after the console, tick timer and idle task are up but
**before** interrupts are unmasked. Spawn tasks and return. Don't loop in it.

## `build.rs`

```rust
fn main() {
    build::link();
}
```

Supplies the linker script. Cargo won't inherit it from a dependency.

## `Cargo.toml`

```toml
[dependencies]
kernel = { path = "../../../kernel", default-features = false }
api = { path = "../../../api" }

[build-dependencies]
build = { path = "../../../tools/build" }

[features]
default = ["kernel/debug-level-1"]
```

An application depends on `api` and `kernel` only — `api` re-exports everything
it used to reach into `hal` for, so the manifest no longer lists it (#105).
Paths are three `..` deep because examples live under `apps/examples/`.

Board and debug level are the **kernel's** features, not the application's, and
`make` forwards them on the command line rather than the app declaring one
feature per choice (#120):

```
cargo build -p hello --no-default-features --features kernel/board-esp32-devkitc,kernel/debug-level-1
```

Exactly one `board-*` must reach `board`, which is why the app sets
`default-features = false` and `make` passes `--no-default-features`. Cargo
unions features; without that a default board would stay on alongside the one
you asked for.

## Priorities

Banded, so you can slot a task in without renumbering:

| Band | Levels | Numeric |
|---|---|---|
| `Critical(0..15)` | real-time | 0x00–0x0F |
| `Normal(0..15)` | application | 0x10–0x1F |
| `Background(0..15)` | idle work | 0x20–0x2F |

Lower number wins. Same priority round-robins. Higher priority preempts
immediately.

## Stack sizes

4096 is a reasonable default.

Traps run on the interrupted task's **own** stack, so your budget has to cover
your deepest call chain *plus* a 112-byte trap frame *plus* the kernel's trap
handler. With logging on, `log_info!` → formatting → UART driver is already
several frames deep before a tick can land on it.

Every stack is painted at spawn and its lowest word is a guard, so an overflow
reports itself by name:

```
[FLINT] FATAL: stack overflow in task worker
```

## API

`use api::prelude::*;` brings all of this in:

```rust
Task::new(name, entry)          // builder; nothing runs until .spawn()
    .priority(Priority::Normal(2))   // optional, defaults to Normal(1)
    .stack(4096)                     // optional, defaults to 4096
    .on_core(1)                      // optional, defaults to "either core"
    .spawn();                   // -> Option<TaskId>; None if the pool is full

sleep_ms(ms)
task::yield_now()

timer::now_ms()

log_error!() log_warn!() log_info!()      // always
log_debug!() log_trace!()                 // feature-gated
```

Queues and mutexes: see `api/src/queue.rs` and `api/src/mutex.rs`.
`Queue::send_isr` is the interrupt-to-task path; it wakes a blocked receiver.

`Task::new` leaves the task free to run on any core, which is usually what you
want. Reach for `.on_core(n)` when the core matters for correctness — see
[Multicore](/developers/multicore/#when-to-pin). The second core has to be started first,
or a task pinned to it waits forever.
