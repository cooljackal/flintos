# Debug Levels

Additive. Everything above your level compiles out entirely — no runtime check,
no dead branch, no string in the binary.

```bash
make flash DEBUG=debug-level-2
```

Default is `debug-level-1`.

| Level | Turns on | Use it for |
|---|---|---|
| `debug-level-0` | nothing | Shipping |
| `debug-level-1` | `flint-log`, `flint-metrics`, `flint-panic` | **Development. The default.** |
| `debug-level-2` | + `flint-trace` | Following scheduler decisions |
| `debug-level-3` | + `flint-timing`, `flint-cpu-util` | Where the time goes |

## What each feature does

| Feature | Effect |
|---|---|
| `flint-log` | `log_info!` / `log_warn!` / `log_error!` reach the console, plus a ring buffer |
| `flint-metrics` | `Counter` and `Gauge` record instead of discarding |
| `flint-panic` | Panic writes a snapshot to SRAM that survives soft reset |
| `flint-trace` | `log_trace!` and kernel event tracing |
| `flint-timing` | Per-task timing |
| `flint-cpu-util` | CPU utilisation accounting |

## `debug-level-0` looks like a dead board

Tasks run. They print nothing. There is no logging *code* to print with.

That is indistinguishable from a hang unless you know, so the kernel says so
once over raw UART:

```
[FLINT] boot complete. Logging is COMPILED OUT (debug-level-0) -- tasks will run but print nothing.
[FLINT] Rebuild with DEBUG=debug-level-1 to see task output.
```

Leave it alone until you're shipping.

## Two switches that aren't levels

Both are `const bool` in source, not features. Flip and rebuild.

**`BOOT_DIAGNOSTICS`** in `kernel/src/boot.rs` — the `[FLINT] ...` boot banner.
On by default. It's how you bisect a failing boot; see
[Troubleshooting](Troubleshooting).

**`TRAP_DIAGNOSTICS`** in `kernel/src/switch.rs` — per-tick heartbeat from
inside the trap handler. **Off** by default, because on a working kernel it's
pure noise. Turn it on when the console goes quiet.

## On-target self-tests

Separate from the levels, and the only tests that exercise the machine rather
than the kernel's logic:

```bash
make test-target                                   # auto-detect the port
make test-target BOARD=board-m5-atom PORT=COM5     # ESP32-PICO on Windows
make test-target BOARD=board-m5-atom PORT=/dev/ttyUSB0
```

Flashes the board, captures the serial output, and exits non-zero if anything
failed. Five checks run after interrupts are unmasked, each chosen because a
host test cannot fail it:

| Test | What only silicon can show |
|---|---|
| `timer_preserves_windowed_context` | a trap corrupting the interrupted task's register windows |
| `deep_window_recursion_returns_intact` | a window spilled past the physical register file and misrestored |
| `tick_advances` | the timer interrupt actually fires |
| `tick_never_goes_backwards` | `tick()`'s wrapping `CCOUNT` re-base slipping |
| `critical_section_masks_the_tick` | that a critical section really masks — on a host `cs_with` masks nothing, so this is unfalsifiable there |

**Set `PORT` whenever more than one serial device is attached.** Otherwise
espflash prompts for a choice, the harness has no terminal to answer with, and
you get a timeout that reports "the board never reached the self-test" — a
confusing way to say "pick a port".

The harness is strict on purpose: it counts the `PASS`/`FAIL` lines that
actually arrived and rejects the run if they disagree with the board's own
summary. A dropped serial line reads as a void run, not a pass. If that
triggers, lower `MONITOR_BAUD`.

To flash the self-test image without the harness judging it — useful when you
want to watch the console yourself:

```bash
make flash EXTRA_FEATURES=self-test
```

## Cost

Measure it, don't guess:

```bash
make build DEBUG=debug-level-0 && make build DEBUG=debug-level-3
```

`make build` prints the size table each time.
