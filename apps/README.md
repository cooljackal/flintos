<!-- SPDX-License-Identifier: Apache-2.0 -->

# Applications

The FlintOS kernel is a library. The thing you flash is an **application**: a
small `no_std` binary crate that links the kernel, names an entry point, and
spawns its own tasks.

`make apps` prints this list from each crate's `description`, and is the copy
that cannot go stale. This table adds the board each one needs.

| App | What it does | Board |
|---|---|---|
| [`hello`](hello/) | One task, logging once a second. The minimal template. | any |
| [`demo`](demo/) | Three tasks at three priorities on three periods. What the kernel is verified against. | any |
| [`smp`](smp/) | Starts the APP CPU and joins it to the scheduler, proving both cores run tasks. | any |
| [`spidma`](spidma/) | Moves bytes through the DMA engine, over SPI looped back through the GPIO matrix. | any |
| [`flashprobe`](flashprobe/) | Erases, programs and reads back the `nvs` partition — with core 1 running throughout, which is the only cover the cross-core flash path has. | any |
| [`blink`](blink/) | Drives the onboard addressable LED over RMT. | Atom Lite or Matrix |
| [`pwm`](pwm/) | Drives LEDC and measures its own duty cycle by reading the pin back. | Atom Lite or Matrix |
| [`imu`](imu/) | Reads the onboard IMU — the first Layer 1-2-3 assembly. | Atom Matrix |

The last three refuse to build for a board whose manifest does not declare the
hardware they drive, and say which board to use instead.

```bash
make apps                                                    # list what's here
make flash BOARD=board-esp32-devkitc                         # demo, on a WROOM-32
make flash APP=hello BOARD=board-esp32-devkitc               # hello instead
make flash APP=demo  BOARD=board-m5-atom-lite                # different board
make flash APP=hello BOARD=board-esp32-devkitc DEBUG=debug-level-0   # no logging
```

`DEBUG` defaults to `debug-level-1`. **`BOARD` has no default** — name it, or
the build stops and lists the choices.

---

## Starting your own

Copy `hello/`, rename it, and add it to `members` in the workspace
[`Cargo.toml`](../Cargo.toml). That is the whole setup — `make flash APP=<name>`
works from there.

An application is three files.

**`src/main.rs`** — the entry point and your tasks:

```rust
#![no_std]
#![no_main]

use api::task;
use hal::types::Priority;

kernel::flint_app!(main, abi = 1);

fn main() {
    task::spawn("worker", worker, Priority::Normal(1), 4096);
}

fn worker() {
    loop {
        api::log_info!("tick");
        task::sleep_ms(1000);
    }
}
```

`main` runs once, after the console, tick timer and idle task are up but
*before* interrupts are unmasked. Spawn tasks and return; nothing is scheduled
until you do. It is not a place to loop.

**`build.rs`** — supplies the linker script, which Cargo will not inherit from a
dependency:

```rust
fn main() {
    build::link();
}
```

**`Cargo.toml`** — dependencies plus the feature pass-through. The application
is the binary, so it is the only crate that can choose a board without the
choice leaking into everything else that links the kernel:

```toml
[dependencies]
kernel = { path = "../../kernel", default-features = false }
api = { path = "../../api" }
hal = { path = "../../hal" }

[build-dependencies]
build = { path = "../../tools/build" }

[features]
default = ["debug-level-1"]          # no default board, deliberately
board-esp32-wrover = ["kernel/board-esp32-wrover"]
board-esp32-devkitc = ["kernel/board-esp32-devkitc"]
board-m5-atom-matrix = ["kernel/board-m5-atom-matrix"]
debug-level-0 = ["kernel/debug-level-0"]
debug-level-1 = ["kernel/debug-level-1"]
debug-level-2 = ["kernel/debug-level-2"]
debug-level-3 = ["kernel/debug-level-3"]
```

Copy the feature block verbatim. Exactly one `board-*` feature must reach
`board`: none stops the build with the list of choices, and two is a compile
error, because a binary with two board manifests merged in is not a build for
either board. No board is a default feature anywhere in the tree, and `make`
still passes `--no-default-features` so that a feature added to some crate's
defaults later cannot quietly change what you built.

---

## Stack sizes

`spawn` takes a stack size in bytes, and 4096 is a reasonable default. Traps run
on the interrupted task's own stack, so the budget has to cover your deepest
call chain *plus* a trap frame and the kernel's trap handler on top — and with
logging on, `log_info!` → formatting → the UART driver is already several frames
deep before a tick can land on it.

Every stack is painted at spawn and its lowest word is a guard, so an overflow
reports itself by name (`FATAL: stack overflow in task ...`) rather than
corrupting whatever lies beneath.
