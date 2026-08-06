# Writing an Application

The kernel is a library. The thing you flash is an application in `apps/`.

Copy `apps/hello/`, rename it, add it to `members` in the workspace
`Cargo.toml`. That's the setup — `make flash APP=<name>` works from there.

An application is three files.

## `src/main.rs`

```rust
#![no_std]
#![no_main]

use flint_api::task;
use flint_hal::types::Priority;

flint_kernel::flint_app!(main);

fn main() {
    task::spawn("worker", worker, Priority::Normal(1), 4096);
}

fn worker() {
    loop {
        flint_api::log_info!("tick");
        task::sleep_ms(1000);
    }
}
```

`main` runs once, after the console, tick timer and idle task are up but
**before** interrupts are unmasked. Spawn tasks and return. Don't loop in it.

## `build.rs`

```rust
fn main() {
    flint_build::link();
}
```

Supplies the linker script. Cargo won't inherit it from a dependency.

## `Cargo.toml`

```toml
[dependencies]
flint-kernel = { path = "../../kernel", default-features = false }
flint-api = { path = "../../api" }
flint-hal = { path = "../../hal" }

[build-dependencies]
flint-build = { path = "../../tools/build" }

[features]
default = ["board-esp32-wrover", "debug-level-1"]
board-esp32-wrover = ["flint-kernel/board-esp32-wrover"]
board-esp32-devkitc = ["flint-kernel/board-esp32-devkitc"]
board-m5-atom = ["flint-kernel/board-m5-atom"]
debug-level-0 = ["flint-kernel/debug-level-0"]
debug-level-1 = ["flint-kernel/debug-level-1"]
debug-level-2 = ["flint-kernel/debug-level-2"]
debug-level-3 = ["flint-kernel/debug-level-3"]
phase0-tests = ["flint-kernel/phase0-tests"]
```

Copy the feature block verbatim. Exactly one `board-*` must reach
`flint-board`, which is why the app sets `default-features = false` and `make`
passes `--no-default-features`. Cargo unions features; without that the default
board stays on alongside the one you asked for.

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

```rust
task::spawn(name, entry, priority, stack_bytes)  // -> Option<TaskId>
task::sleep_ms(ms)
task::yield_now()

timer::now_ms()

log_error!() log_warn!() log_info!()      // always
log_debug!() log_trace!()                 // feature-gated
```

Queues and mutexes: see `api/src/queue.rs` and `api/src/mutex.rs`.
`Queue::send_isr` is the interrupt-to-task path; it wakes a blocked receiver.
