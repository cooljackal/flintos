<!-- SPDX-License-Identifier: Apache-2.0 -->

# ARMv6-M atomic feasibility probe

## Result

Rust core atomics cannot provide the read-modify-write operations FlintOS uses
on `thumbv6m-none-eabi`. A `portable-atomic` backend protected by an RP2040 SIO
hardware spinlock does compile and link, and the linked code contains the
expected interrupt-mask and spinlock operations. This proves a viable backend
shape, not its behavior on silicon.

## Reproduce

With Rust 1.96.0 and `thumbv6m-none-eabi` installed:

```powershell
Push-Location tools/arm-atomic-probe
rustc --edition 2021 --crate-type lib --target thumbv6m-none-eabi core-atomics.rs
cargo build --release
Pop-Location
```

Run from this directory so its `.cargo/config.toml` selects ARM rather than the
repository root configuration selecting Xtensa.

The first command must fail because `AtomicU8::compare_exchange`,
`AtomicU32::compare_exchange`, and `AtomicUsize::fetch_add` do not exist for
this target. The second command must produce
`target/thumbv6m-none-eabi/release/arm-atomic-probe`.

## Measured evidence (2026-08-21)

| Measurement | Result |
|---|---|
| Rust target configuration | No `target_has_atomic` values are reported |
| Core atomic RMW compile | Fails with `E0599` for all three representative operations |
| `portable-atomic` 1.15.0 with `critical-section` 1.2.0 | Optimized binary links, 66,504 bytes including ELF metadata |
| Linked `reset` code | Three `mrs PRIMASK` / `cpsid i` / SIO-lock / `msr PRIMASK` sequences |
| SIO lock address | Literal `0xd0000138`, RP2040 spinlock 14 |
| Hardware image | Pico SDK 2.1.1 builds an ELF/BIN/UF2 for the Wio RP2040 |
| Hardware execution | Passed on the Wio RP2040; the exact PASS line repeated 20 times over 20 seconds on COM8 |

The Rust compile probe's critical-section implementation is intentionally minimal. It is
enough to inspect the compiler and linker path, but is not production code:
it does not handle nested entry, initialize/claim the lock, or test reset and
two-core contention.

## Decision

Proceed with a pinned `portable-atomic` backend whose platform critical section
combines exact PRIMASK save/restore with one claimed RP2040 hardware spinlock.
Keep core 1 disabled until single-core interrupt tests pass. Before enabling
core 1, add an on-target stress test that concurrently performs byte, word,
and pointer compare-exchange/fetch operations from both cores and interrupt
context, including nested critical-section cases.

The `hardware` probe implements that test independently of FlintOS. It uses
spinlock 14, per-core nesting depth, exact interrupt-state restoration, 1,000
timer-interrupt operations, and 100,000 word and pointer increments from each
core. A passing run must print:

```text
FLINTOS-ARM-ATOMIC PASS word=200000 pointer=200000 irq=1000 nested=1 depth=0,0
```

That exact result was measured on the attached Wio RP2040 on 2026-08-21. The
hardware probe validates the synchronization mechanism, but it is written
against the Pico SDK rather than the eventual Rust `portable-atomic` adapter.

This matches the vendor design: RP2040 has 32 SIO hardware spinlocks, spinlocks
are not re-entrant, and the SDK reserves locks 14 and 15 for an operating
system. The SDK's own RP2040 C11 atomics are also spinlock-protected.

Primary sources:

- [Raspberry Pi pico-sdk hardware synchronization API](https://github.com/raspberrypi/pico-sdk/blob/2.2.0/src/rp2_common/hardware_sync/include/hardware/sync.h)
- [Raspberry Pi pico-sdk atomic implementation](https://github.com/raspberrypi/pico-sdk/tree/2.2.0/src/rp2_common/pico_atomic)
- [`portable-atomic` 1.15.0 target support](https://github.com/taiki-e/portable-atomic/tree/v1.15.0#optional-features)
- [Zephyr SMP synchronization contract](https://github.com/zephyrproject-rtos/zephyr/blob/v4.2.0/doc/kernel/services/smp/smp.rst#synchronization)
