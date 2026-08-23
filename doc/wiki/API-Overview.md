# API Overview

**The full, always-current API reference is the generated rustdoc:**
[**flintos API docs**](https://cooljackal.github.io/flintos/api/). It is built
from the doc comments on every push to `main`, so it never drifts from the code.

This page is the map — what each part is and where to start. rustdoc answers
*what's callable*; this answers *where to look*.

## Writing an application — the system API

Everything an application calls lives in the [`api`](https://cooljackal.github.io/flintos/api/) crate.

| Module | What it gives you |
|---|---|
| [`api::prelude`](https://cooljackal.github.io/flintos/api/prelude/index.html) | `use api::prelude::*;` — one glob for the logging macros, the `Task` builder, `sleep_ms`/`exit`/`wait_until`, `Error`/`Result`, the sync cells and the bus/pin/stream surface |
| [`api::task`](https://cooljackal.github.io/flintos/api/task/index.html) | The `Task` builder (`Task::new(name, entry).priority(..).stack(..).on_core(..).spawn()`), plus `spawn`/`spawn_on` free fns, `sleep_ms`, `yield_now`, `exit`, `wait_until` |
| [`api::queue`](https://cooljackal.github.io/flintos/api/queue/index.html) | Typed, bounded queues; `send`/`recv` with timeout, `send_isr` from an interrupt |
| [`api::mutex`](https://cooljackal.github.io/flintos/api/mutex/index.html) | `Mutex` with priority inheritance; `lock` returns a guard |
| [`api::sync`](https://cooljackal.github.io/flintos/api/sync/index.html) | `Once` and `CsCell` shared-static cells (also re-exported at the crate root) |
| [`api::timer`](https://cooljackal.github.io/flintos/api/timer/index.html) | `now_ms` and the monotonic tick |
| [`api::time`](https://cooljackal.github.io/flintos/api/time/index.html) | `now_us` — microsecond monotonic clock, for timing shorter than a tick |
| [`api::interrupt`](https://cooljackal.github.io/flintos/api/interrupt/index.html) | `connect(source, handler)` — route a peripheral IRQ to a top-half |
| [`api::dma`](https://cooljackal.github.io/flintos/api/dma/index.html) | `alloc`/`begin`/`await_transfer` for drivers that move blocks |
| `api::Error` / `api::Result` | The one error type an application `?`s into (re-exported from `hal`) |
| [`api::debug`](https://cooljackal.github.io/flintos/api/debug/index.html) | `log_error!`…`log_trace!`, metrics, stack high-water marks, panic capture |

Start with [Tutorial: Hello World](Tutorial-Hello-World).

## Writing a driver — the layer contracts

A driver implements one trait for its layer and calls only the layer below. The
traits are the contract; the [rustdoc](https://cooljackal.github.io/flintos/hal/bus/index.html)
carries the exact signatures.

| You are writing | Implement | Reference |
|---|---|---|
| A sensor / display / LED (Layer 3) | uses a `BusHandle`, or a `lib/` device trait like `LedStrip` | [`hal::bus::BusHandle`](https://cooljackal.github.io/flintos/hal/bus/struct.BusHandle.html), [`led_strip`](https://cooljackal.github.io/flintos/led_strip/index.html) |
| A protocol bus (Layer 2) | [`api::bus::Bus`](https://cooljackal.github.io/flintos/hal/bus/trait.Bus.html) | e.g. [`spi_bus`](https://cooljackal.github.io/flintos/spi_bus/index.html), [`i2c_bus`](https://cooljackal.github.io/flintos/i2c_bus/index.html) |
| A peripheral register driver (Layer 1) | [`hal::bus::PhysicalBus`](https://cooljackal.github.io/flintos/hal/bus/trait.PhysicalBus.html) | the `esp32-*` driver crates |

Start with [Writing a Driver](Writing-a-Driver), which walks one example per layer.

## Portable libraries

The [`lib/`](https://cooljackal.github.io/flintos/led_strip/index.html) crates —
`led-strip`, `led-matrix`, `crypto`, `wpa`, `kvstore`, `heap` — hold device-class
contracts and pure code with no hardware. See [Libraries](Libraries).

## What's not here

The reference covers the crates that build on the host: the `api` surface, the
driver/bus traits, the logical drivers, and `lib/*`. Architecture- and
chip-specific internals (`arch/*`, `soc/*`) are documented in their own source
and in the [Hardware](Home#hardware) pages, not in this reference.
