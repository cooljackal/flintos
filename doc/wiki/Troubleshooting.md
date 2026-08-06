# Troubleshooting

## The board says nothing

Each banner line proves the step before it, so **the last line you see tells you
where it died**.

| Last line | What it means |
|---|---|
| *(nothing)* | Died before Rust, or your monitor baud is wrong. Must be 115200 |
| `FlintMain reached` | Vector table didn't install — check `VECBASE` |
| `VECBASE ... MISMATCH` | Traps go to ROM. Nothing will ever schedule |
| `WOE=0` | Register windows off. Every windowed call is unreliable |
| `cpu_hz=... (ASSUMED)` | Clock measurement failed. Every timeout is scaled by a guess |
| `entering idle`, then silence | Died in its first interrupt — trap entry or `_flint_trap` |
| `DBL <cause> <epc1> <depc> <vaddr>` | Double exception. Those four words locate it exactly |

## The board boots but prints nothing after that

You built with `DEBUG=debug-level-0`, which compiles logging out. Tasks are
running; nothing is printing. Rebuild:

```bash
make flash
```

Recent builds say so on the console. Older ones just went quiet.

## Tasks run, then it hangs or misbehaves

Turn on the trap heartbeat: set `TRAP_DIAGNOSTICS = true` in
`kernel/src/switch.rs`, rebuild.

```
[FLINT] t=1000 cur=0:idle ready=0x00000001 pc=0x400d12a9 ws=0x00000001
```

Every 1000 ticks: the tick, the running task, the ready mask, where the
interrupted task actually is, and `WINDOWSTART`. A kernel that never schedules
and a kernel whose timer never ticks produce byte-identical silence otherwise.

## Reading a `DBL` line

```
DBL 00000009 400d22f8 400804ab 3f401d67
    ^cause   ^epc1    ^depc    ^excvaddr
```

- **cause** — 9 = alignment, 28 = load prohibited, 29 = store prohibited
- **epc1** — where the *first* exception happened
- **depc** — where the *second* one did, inside the handler
- **excvaddr** — the address that faulted

Resolve the addresses:

```bash
xtensa-esp32-elf-addr2line -e target/xtensa-esp32-none-elf/debug/demo 400d22f8
```

An `excvaddr` that isn't in RAM (`0x3FFB0000`–`0x3FFDFFFF`) usually means a
stack pointer got corrupted. Include all four words in a bug report.

## Timing is off by a constant factor

The CPU clock. Check the `cpu_hz=` line says `measured`, not `ASSUMED`.

## Stack overflow

It reports itself:

```
[FLINT] FATAL: stack overflow in task worker
```

Raise the stack in your `spawn` call. Remember traps run on the interrupted
task's own stack — see
[Writing an Application](Writing-an-Application#stack-sizes).

## Flashing fails

**`Error while connecting`, right after `Using flash stub`** — baud switch
failed. This is the common one.

```bash
make flash FLASH_BAUD=115200
```

**`Error while connecting`, before the stub** — not in download mode. Hold
**BOOT**/**GPIO0**, tap **EN**/**RST**, release BOOT, then flash. On an M5Stack
Atom the reset is the small side button.

**Wrong or busy port** — close any other monitor; only one process can hold it.
`--port COM5` or `--port /dev/ttyUSB0`.

**A previous image is wedging the board**

```bash
make erase
```

**Console is garbage after a successful flash** — baud mismatch, not a kernel
fault. `--monitor-baud` must be 115200. It's a *different* flag from `--baud`.

## Build fails

**`--target takes a target architecture as an argument`** — `HOST_TARGET` came
back empty. Check `rustc --print host-tuple` prints something.

**`can't find crate for 'core'`** — you're not on the esp toolchain. Run
`. $HOME/export-esp.sh`.

**`more than one board-* feature is enabled`** — working as intended. Pass
`--no-default-features`, or use `make`, which does.

**Linker: `Cannot create temporary file in C:\WINDOWS`** — set `TMP` and `TEMP`
to somewhere writable.

## Still stuck

Open an [issue](https://github.com/cooljackal/flintos/issues) with your board,
the full serial output (garbled is fine — it's data), and the command you ran.
