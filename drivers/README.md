<!-- SPDX-License-Identifier: Apache-2.0 -->

# Writing a driver

Copy [`physical/esp32/_template/`](physical/esp32/_template/) and change the
registers. It is a complete, compiling Layer-1 driver for a fictional
one-register peripheral that follows every convention on this page. The prose
here explains *why*; the template is the *what*.

## The three layers

| Tier | Directory | May depend on | Is |
| --- | --- | --- | --- |
| Physical (Layer 1) | `physical/<soc>/<periph>/` | `hal`, `soc-<soc>` | one peripheral's registers |
| Bus (Layer 2) | `bus/<name>/` | `api`, `lib/*` | a transport |
| Logical (Layer 3) | `logical/<part>/` | `api`, `lib/*` | one part number |

`tools/check-layers.sh` enforces the "may depend on" column from
`cargo metadata`; `make check-layers` runs it. A Layer-1 driver naming
anything but `hal` and its SoC crate is a build failure, not a review note.

The rest of this page is about **Layer-1 physical drivers**, the layer that
touches registers.

## Bottom line

| Convention | What it means | Verify against |
| --- | --- | --- |
| One constructor | `open(&Port) -> hal::Result<Self>`, claims the controller once | the template |
| `unsafe fn new(base)` | kept only so self-tests can skip the claim | `esp32-timg`, `esp32-i2c` |
| Registers via `soc_esp32::reg` | `reg::at` + `reg::{read,write,modify,set,clear}` — never a private `fn reg` | `soc/esp32/src/reg.rs` |
| Own error type | a small enum with `impl From<E> for hal::Error` | `esp32-timg`, `hal/src/error.rs` |
| Pad routing inside `open` | the driver routes its pins; the caller passes numbers, never a mux | `esp32-i2c` |
| `PhysicalTransfer` vs `PhysicalBus` | `&self` traffic vs `&mut` init, for bus-shaped drivers | `hal/src/bus.rs` |

## One constructor shape

```rust
pub fn open(port: &SomePort) -> hal::Result<Self>
```

`open` takes a `*Port` — a `Copy` value pairing a controller with its pin
configuration, defined in `soc_esp32::ctrl` (`I2cPort`, `SpiPort`,
`UartPort`). It claims the controller exactly once, brings the hardware up
(clock, reset, pads, registers), and returns an owned handle. A second `open`
of the same controller returns an error instead of a second alias to the same
registers.

Claim-once is a `static AtomicBool` and a `compare_exchange`; the template
shows the whole of it. A real controller reads its base, clock bit and IRQ
from its `*Ctrl` enum (`I2cCtrl::base()`, `.clock()`, `.irq()`) rather than
from a bare `u32`, so an invalid combination cannot be spelled.

> Status: the `*Port`/`*Ctrl` types and the constructor shape are landed
> (`soc_esp32::ctrl`); the safe `open` on the shipped `Esp32{I2c,Spi,Uart}`
> drivers is issue #109 and lands separately. Until then the template is the
> reference implementation of `open`, and the existing drivers still expose the
> older `unsafe fn new`.

`unsafe fn new(base)` stays, but only for the on-target self-test harness,
which points a driver at loopback or scratch addresses without the claim.
Application and board code call `open`.

## Registers: `soc_esp32::reg`, not a private `fn reg`

```rust
use soc_esp32::reg;

unsafe { reg::write(reg::at(self.base, OFFSET), value) };
unsafe { reg::modify(reg::at(self.base, OFFSET), FIELD_MASK, field << SHIFT) };
```

`reg::at(base, offset)` builds the pointer; `read`, `write`, `set`, `clear`
and `modify` do the access. `modify` clears the field before OR-ing the value
in, which is the bug a hand-written `r |= v` keeps re-introducing — the field
accumulates every value it ever held. `reg.rs` explains it and tests the
arithmetic on a host.

**Do not** write the private helper this replaces:

```rust
fn reg(&self, offset: u32) -> *mut u32 { (self.base + offset) as *mut u32 } // NO
```

Seven copies of exactly that signature are still in the tree and are the
anti-pattern to delete, not to copy — confirm with:

```console
$ grep -rn 'fn reg(&self, offset: u32) -> \*mut u32' drivers/physical/esp32/
```

which today finds them in `gpio`, `i2c`, `spi` (`lib.rs` and `slave.rs`),
`timg` (`lib.rs` and `lact.rs`) and `uart`. `soc_esp32::dport` has its own
`modify`/`enable`/`disable` for clock gating — the plain `reg` helpers must
**not** be used on the DPORT block, which needs the erratum workaround.

## Its own error type, `From` into `hal::Error`

A driver keeps a small enum that says precisely what failed, and an
`impl From<ThatEnum> for hal::Error` so an application can `?` a driver call
into its one error type without a `map_err`:

```rust
impl From<ScratchError> for hal::Error {
    fn from(e: ScratchError) -> Self {
        match e {
            ScratchError::InUse => hal::Error::Other("esp32 scratch already open"),
        }
    }
}
```

Map to the richest `hal::Error` variant that fits (`Unsupported`,
`WrongDevice`, `NotInitialised`, ...); fall back to `Other(&'static str)` only
when nothing more specific applies. Put the `impl` above the `#[cfg(test)]`
module, where the rest of the tree keeps it.

## Pad routing belongs to the driver

The caller passes pin *numbers* in the `*Port`; the driver does the routing.
Inside `open`, route each signal through `Esp32PinMux` before first use:

```rust
let mux = Esp32PinMux::new();
mux.route(Signal::I2cSda(instance), sda, PinConfig::OPEN_DRAIN_PULLUP)?;
mux.route(Signal::I2cScl(instance), scl, PinConfig::OPEN_DRAIN_PULLUP)?;
```

`esp32-i2c` is the worked example. A caller must never have to touch a pin
mux to use a driver; if it does, the routing is in the wrong place.

## `PhysicalTransfer` vs `PhysicalBus`

For a bus-shaped driver (`hal/src/bus.rs`, split in #110):

- `PhysicalTransfer::exchange(&self, tx, rx)` — ordinary traffic. `&self`,
  because the hardware holds the state, not the struct, so many transfers
  share one handle.
- `PhysicalBus` (supertrait of `PhysicalTransfer`) — `&mut` reconfiguration,
  the init-time surface that changes the controller's settings.

The template is not a bus, so it implements neither; it still keeps the same
`&self`-traffic / owned-init split in its own methods, which is the point.

## Naming and layout

Package name is `<soc>-<leaf>` and the directory leaf is `<leaf>`:
`drivers/physical/esp32/uart/` is `esp32-uart`. Every crate sets
`publish = false`. `tools/check-names.sh` (`make check-names`) enforces both.
The naming rule in full is at the top of the root `Cargo.toml`.

## Before you commit

```console
$ make test-host && make lint && make check-layers && make check-names
$ make check-all   # includes the Xtensa build of every workspace crate
```
