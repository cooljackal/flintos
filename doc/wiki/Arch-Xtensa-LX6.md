# Xtensa LX6


### What the kernel relies on here

| | |
|---|---|
| Context switch | Register-window spill through the canonical `SPILL_ALL_WINDOWS` sequence |
| Critical sections | `rsil` token, restored on drop — not unmasked |
| Tick | `CCOUNT`/`CCOMPARE`, with the CPU frequency measured against the RTC slow clock rather than assumed |
| Interrupts | Level 1 only. Levels 2–5 reach the unhandled stub, which is why `intr_map` refuses to route a peripheral above level 1 |

**CPU core only.** Traps, the context switch, register windows and the core
timer. Chip registers do not belong here even when this is the only chip that
uses the core: the ESP32 interrupt crossbar was sitting in `registers.rs`,
duplicated by `soc_esp32::intr_map` and dead, and the ESP-IDF image header has
moved to `soc-esp32`.

One violation is left and is marked in the source: `registers::rtc_cntl` is an
ESP32 register block, still here because `tick.rs` measures the CPU frequency
against the RTC slow clock. Fixing it means this crate stops owning that
measurement and takes `cpu_hz` from its caller.

The CPU core in the ESP32. Crate: `arch/xtensa` (`arch-xtensa`).

You only need this page if you're touching the trap path, the tick, or a new
Xtensa chip. Application code never sees any of it.

## Register windows — the thing that will bite you

Xtensa has 64 physical registers. Code sees 16 (`a0`–`a15`) through a sliding
window. A call rotates the window instead of pushing registers.

Two consequences:

1. **A task's outer call frames live in the register file, not on its stack**,
   until a window overflow evicts them.
2. **Saving `a0`–`a15` is not saving the task.** Everything deeper is still in
   physical registers a different task is about to use.

So the trap entry spills every live window to its owner's stack before doing
anything else.

| Register | Meaning |
|---|---|
| `WINDOWBASE` | Which physical group `a0` currently maps to |
| `WINDOWSTART` | One bit per group: is a frame live there |
| `PS.WOE` | Window overflow enable — must be 1 |
| `PS.EXCM` | Exception mode. **While set, a window exception escalates to DoubleException** |
| `PS.CALLINC` | Rotation amount the last call used |

### The spill

The canonical sequence, same as Zephyr and NuttX:

```asm
and a12, a12, a12
rotw 3          // ×4, then rotw 4 — 16 slots, a full trip
```

`and a12, a12, a12` is an identity write. Its only job is to *reference* a
register in an older live window, which raises the overflow that writes that
window to memory. `rotw` steps around the ring. Nothing is destroyed.

**Preconditions: `PS.WOE=1`, `PS.EXCM=0`, and `a1` holding the task's real
stack pointer.**

That last one is not optional and is the subtlest bug in this codebase's
history. An overflow writes a caller's registers to `[sp - 16]` of the frame
below it; the matching underflow reads them back from the same place. Both
addresses come from `a1`. Move `a1` first — to make room for a trap frame — and
every spill lands somewhere the underflow will never look. Symptom: a stack
pointer of `0x3f401da7`, a rodata address, and a double exception.

## Trap entry

`vectors.S`, `_flint_trap_entry`. Order matters throughout.

1. Stash `a2`/`a3` in `EXCSAVE1`/`EXCSAVE2`
2. Write specials (`EPC1`, `PS`, `SAR`, `LBEG`, `LEND`, `LCOUNT`) through a
   scratch frame pointer — **`a1` untouched**
3. `PS = 0x40003` (WOE=1, INTLEVEL=3, EXCM=0) so overflows can dispatch
4. Reload `a2`/`a3` — register file pristine
5. **Spill** — still on the task's real `sp`
6. `a1 -= 112`, save `a0`–`a15`, `WINDOWBASE`, `WINDOWSTART`
7. `callx4 _flint_trap`
8. Restore in reverse; `PS` last with `EXCM` forced; `rfe`

`EPC1` is saved before the spill because window exceptions clobber it.
`EXCCAUSE` is not clobbered — window exceptions use their own vectors.

The frame is **112 bytes**: 96 for `TaskContext` plus the 16-byte ABI save area
above it, which the spill just wrote to.

`TaskContext` offsets (`hal/src/types.rs`, `#[repr(C)]`, asserted at compile
time):

```
0x00 pc     0x04 ps     0x08 sar    0x0C lbeg
0x10 lend   0x14 lcount 0x18 a[0..15]
0x58 windowbase         0x5C windowstart
```

## Vector table

VECBASE-relative, installed by `startup.S`.

| Offset | Vector |
|---|---|
| `0x000` / `0x040` | WindowOverflow4 / Underflow4 |
| `0x080` / `0x0C0` | WindowOverflow8 / Underflow8 |
| `0x100` / `0x140` | WindowOverflow12 / Underflow12 |
| `0x180`–`0x2C0` | Level 2–5, Debug, NMI → `_flint_unhandled` |
| `0x300` / `0x340` | Kernel / User exception → `_flint_trap_entry` |
| `0x3C0` | DoubleException → `_double_exception` |

The table is **1 KB and currently 94% full**. `make size` reports it.

## Task dispatch

A new task can't just have its PC pointed at the entry function — the window
state that produces is one the hardware never generates. `_flint_task_start`
(`context.S`) is entered with `a0=0`, `a1=sp`, `a3=entry` and reaches the task
through a real `callx4`, so the hardware sets `CALLINC`, the return address and
the window state itself.

## Tick

Timer0 CCOMPARE. The CPU frequency is **measured** at boot against the RTC slow
clock, not assumed — the ESP32 boots at 80 MHz but can be 160 or 240, and every
timeout in the system scales by it. `XtensaTick::freq_measured()` says whether
the measurement worked; the boot banner prints it.

## Assembly build

`global_asm!` routes through LLVM's integrated assembler, which **rejects**
`s32e`, `l32e`, `rfwo`, `rfwu` — the instructions the vectors are built from.
`build.rs` assembles the `.S` files with `xtensa-esp32-elf-gcc` instead and
links them with `+whole-archive`, because the vectors are reached via VECBASE
and never called.

## Adding another Xtensa chip

The S2 and S3 are also LX-family. Most of this crate is reusable; what changes
is the peripheral map and the tick source, which live in the SoC crate. See
[Architecture](Architecture).
