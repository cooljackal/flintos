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

## Boot self-test

Separate from the levels:

```bash
make flash EXTRA_FEATURES=self-test
```

Drives a deep windowed recursion after interrupts are unmasked and checks the
result — the regression test for register-window corruption across a trap. Fails
the boot loudly if the trap path breaks. Off by default.

## Cost

Measure it, don't guess:

```bash
make build DEBUG=debug-level-0 && make build DEBUG=debug-level-3
```

`make build` prints the size table each time.
