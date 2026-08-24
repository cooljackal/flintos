# API Overview

**The full, always-current API reference is the generated rustdoc:**
[**flintos API docs**](https://flintos.dev/api/api/). It is built
from the doc comments on every push to `main`, so it never drifts from the code.

This page is the map — what each part is and where to start. rustdoc answers
*what's callable*; this answers *where to look*.

## Writing an application — the system API

Everything an application calls lives in the [`api`](https://flintos.dev/api/api/) crate.

| Module | What it gives you |
|---|---|
| [`api::prelude`](https://flintos.dev/api/api/prelude/) | `use api::prelude::*;` — one glob for the logging macros, the `Task` builder, `sleep_ms`/`exit`/`wait_until`, `Error`/`Result`, the sync cells and the bus/pin/stream surface |
| [`api::task`](https://flintos.dev/api/api/task/) | The `Task` builder (`Task::new(name, entry).priority(..).stack(..).on_core(..).spawn()`), plus `spawn`/`spawn_on` free fns, `sleep_ms`, `yield_now`, `exit`, `wait_until` |
| [`api::queue`](https://flintos.dev/api/api/queue/) | Typed, bounded queues; `send`/`recv` with timeout, `send_isr` from an interrupt |
| [`api::mutex`](https://flintos.dev/api/api/mutex/) | `Mutex` with priority inheritance; `lock` returns a guard |
| [`api::sync`](https://flintos.dev/api/api/sync/) | `Once` and `CsCell` shared-static cells (also re-exported at the crate root) |
| [`api::timer`](https://flintos.dev/api/api/timer/) | `now_ms` and the monotonic tick |
| [`api::time`](https://flintos.dev/api/api/time/) | `now_us` — microsecond monotonic clock, for timing shorter than a tick |
| [`api::interrupt`](https://flintos.dev/api/api/interrupt/) | `connect(source, handler)` — route a peripheral IRQ to a top-half |
| [`api::dma`](https://flintos.dev/api/api/dma/) | `alloc`/`begin`/`await_transfer` for drivers that move blocks |
| `api::Error` / `api::Result` | The one error type an application `?`s into (re-exported from `hal`) |
| [`api::debug`](https://flintos.dev/api/api/debug/) | `log_error!`…`log_trace!`, metrics, stack high-water marks, panic capture |

Start with [Tutorial: Hello World](Tutorial-Hello-World).

## Writing a driver — the layer contracts

A driver implements one trait for its layer and calls only the layer below. The
traits are the contract; the [rustdoc](https://flintos.dev/api/hal/bus/)
carries the exact signatures.

| You are writing | Implement | Reference |
|---|---|---|
| A sensor / display / LED (Layer 3) | uses a `BusHandle`, or a `lib/` device trait like `LedStrip` | [`hal::bus::BusHandle`](https://flintos.dev/api/hal/bus/#bushandle), [`led_strip`](https://flintos.dev/api/led-strip/) |
| A protocol bus (Layer 2) | [`api::bus::Bus`](https://flintos.dev/api/hal/bus/#bus) | e.g. [`spi_bus`](https://flintos.dev/api/spi-bus/), [`i2c_bus`](https://flintos.dev/api/i2c-bus/) |
| A peripheral register driver (Layer 1) | [`hal::bus::PhysicalBus`](https://flintos.dev/api/hal/bus/#physicalbus) | the `esp32-*` driver crates |

Start with [Writing a Driver](Writing-a-Driver), which walks one example per layer.

## Portable libraries

The [`lib/`](https://flintos.dev/api/led-strip/) crates —
`led-strip`, `led-matrix`, `crypto`, `wpa`, `kvstore`, `heap` — hold device-class
contracts and pure code with no hardware. See [Libraries](Libraries).

## What's not here

The reference covers the crates that build on the host: the `api` surface, the
driver/bus traits, the logical drivers, and `lib/*`. Architecture- and
chip-specific internals (`arch/*`, `soc/*`) are documented in their own source
and in the [Hardware](Home#hardware) pages, not in this reference.
