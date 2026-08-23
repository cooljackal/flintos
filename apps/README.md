<!-- SPDX-License-Identifier: Apache-2.0 -->

# Applications

The FlintOS kernel is a library. The thing you flash is an **application**: a
small `no_std` binary crate that links the kernel, names an entry point, and
spawns its own tasks.

Two directories. [`examples/`](examples/) is what a newcomer reads or copies.
[`tests/`](tests/) is the on-target verification workloads: each prints
`PASS`/`FAIL` and stands in for one issue or plan step. `make apps` prints
both lists from each crate's `description`, and is the copy that cannot go
stale. The tables below add the board each one needs.

## Examples, in reading order

Read them top to bottom; each one's header ends with a `Next:` line pointing
at the one after it. The order only really holds for the first two — `blink`
and `pwm` need an Atom, and the last three are peers, not steps.

| Step | App | What it does | Board |
|---|---|---|---|
| 1 | [`hello`](examples/hello/) | One task, logging once a second. The minimal template. | any |
| 2 | [`demo`](examples/demo/) | Three tasks at three priorities on three periods. Also the on-target verification workload — `make test-target` flashes this. | any |
| 3 | [`blink`](examples/blink/) | First peripheral: drives the onboard addressable LED over RMT. | Atom Lite or Matrix |
| 4 | [`pwm`](examples/pwm/) | Drives LEDC and measures its own duty cycle by reading the pin back. | Atom Lite or Matrix |

## Porting templates

One per bus. Start from the one your device speaks; they have the same shape.

| App | What it does | Board |
|---|---|---|
| [`imu`](examples/imu/) | Reads the onboard IMU — the first Layer 1-2-3 assembly. The I²C porting template. | Atom Matrix |
| [`spitxrx`](examples/spitxrx/) | SPI looped MOSI→MISO on one pad, driven through the Layer-2 `Bus`. The SPI porting template. | DevKitC |
| [`uartecho`](examples/uartecho/) | UART2 internal TX→RX loopback, echoed through the UART's `ByteStream` (a UART is a stream, not a `Bus`). The UART template. | DevKitC |

These three refuse to build for a board whose manifest does not declare the
hardware they drive, and say which board to use instead.

## Verification apps

Not templates. Each exercises one subsystem on hardware and reports
`PASS`/`FAIL`. The last column is what it is the evidence for.

| App | What it verifies | Board | Covers |
|---|---|---|---|
| [`smp`](tests/smp/) | Starts the APP CPU and joins it to the scheduler, proving both cores run tasks. | any | #20 |
| [`spidma`](tests/spidma/) | Moves bytes through the DMA engine, over SPI looped back through the GPIO matrix. | any | #80 |
| [`flashprobe`](tests/flashprobe/) | Erases, programs and reads back the `nvs` partition — with core 1 running throughout, which is the only cover the cross-core flash path has. | any | [nvs handover](../doc/nvs-flash-handover.md) |
| [`radioprobe`](tests/radioprobe/) | Calls into the PHY blob on real silicon and reports what it did. Needs `EXTRA_FEATURES=blobs`. | any | [plan-radio](../doc/plan-radio.md) 3.6 |
| [`wifiscan`](tests/wifiscan/) | Scans for access points. Needs `EXTRA_FEATURES=blobs`. | any | [plan-radio](../doc/plan-radio.md) 5.2 |
| [`wificonnect`](tests/wificonnect/) | Joins a WPA2 network. Needs `EXTRA_FEATURES=blobs` and credentials in the environment. | any | [plan-radio](../doc/plan-radio.md) phase 3 acceptance |
| [`arm-selftest`](tests/arm-selftest/) | The machine-judged RP2040 suite, run by `make test-arm-target`. | Wio RP2040 Mini | the ARM port |

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

Copy `examples/hello/` to `examples/<name>/` and rename the package in its
`Cargo.toml`. That is the whole setup: the workspace picks up every directory
under `apps/examples/` and `apps/tests/`, so there is nothing to register, and
`make flash APP=<name>` works from there.

An application is three files, and `hello` is nothing but those three.

**`src/main.rs`** — the entry point and your tasks:

```rust
#![no_std]
#![no_main]

use api::prelude::*;

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

**`Cargo.toml`** — dependencies, and nothing to wire up for the board:

```toml
[dependencies]
kernel = { path = "../../../kernel", default-features = false }
api = { path = "../../../api" }

[build-dependencies]
build = { path = "../../../tools/build" }

[features]
default = ["kernel/debug-level-1"]   # bare `cargo build` still needs a board
```

The board and the debug level are the kernel's features, and `make` passes them
straight through on the command line — `--features kernel/board-esp32-devkitc,kernel/debug-level-1`
— because cargo accepts `pkg/feature` for a workspace member. So a new app's
manifest declares **no** `board-*` or `debug-level-*` features at all: the block
of two dozen forwarding lines that used to sit here is gone, and there is
nothing to copy or keep in step (see #120). Exactly one board must still reach
`board` — none stops the build with the list of choices, two is a compile error
— but that is now enforced once, in the kernel, not re-stated in every app.

Declare a feature here only for something the *application itself* owns and its
build script or code reads: `self-test`, `blobs`, a `watchdog-test-*` switch, or
`radio-bt` (which `build::link()` reads to size the DRAM map). `make` passes
`--no-default-features`, so a feature added to some crate's defaults later cannot
quietly change what you built; a further kernel feature goes through
`EXTRA_FEATURES`, e.g. `make flash ... EXTRA_FEATURES=kernel/radio-ble`.

---

## Stack sizes

A task gets 4096 bytes of stack unless `Task::new(..).stack(bytes)` says
otherwise, and 4096 is a reasonable default. Traps run
on the interrupted task's own stack, so the budget has to cover your deepest
call chain *plus* a trap frame and the kernel's trap handler on top — and with
logging on, `log_info!` → formatting → the UART driver is already several frames
deep before a tick can land on it.

Every stack is painted at spawn and its lowest word is a guard, so an overflow
reports itself by name (`FATAL: stack overflow in task ...`) rather than
corrupting whatever lies beneath.
