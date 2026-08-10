# Getting Started

## 1. Toolchain

Xtensa needs Espressif's Rust fork.

```bash
cargo install espup espflash
espup install
. $HOME/export-esp.sh
```

Windows PowerShell: `. $env:USERPROFILE\export-esp.ps1`

## 2. Flash

```bash
git clone https://github.com/cooljackal/flintos
cd flintos
make flash
```

Builds `apps/demo`, flashes over USB serial, opens a monitor.

## 3. Pick something else

```bash
make apps                                  # what's available
make flash APP=hello                       # one task instead of three
make flash APP=demo BOARD=board-m5-atom-lite   # different board
```

Defaults: `APP=demo`, `DEBUG=debug-level-1`.

> **`BOARD` has no default and must be given.** A board manifest is the pin
> map, the bus map and the IRQ numbers; defaulting it means flashing a board
> you did not choose. `make flash` with no board lists them and stops.
>
> For a plain ESP32 devboard use `BOARD=board-esp32-devkitc` — that is the
> board the on-target suite is verified against. See
> [ESP32-DevKitC](Board-ESP32-DevKitC).

Enabling two boards is a compile error, not a warning — a wrong pin map looks
like broken hardware.

## What a good boot looks like

```
[FLINT] VECBASE=0x40080000 _vector_table_start=0x40080000 MATCH (vector table installed)
[FLINT] PS=0x0006000f WOE=1 (window overflow/underflow enabled)
[FLINT] SP=0x3ffb41d0 task_stack_pool=[0x3ffc0000, 0x3ffd8000)
[FLINT] cpu_hz=80000000 (measured: CCOUNT timed against RTC slow clock)
[    2][task:1] INFO  [sensor] prio=Normal(1) n=1
[    5][task:2] INFO  [consumer] prio=Normal(5) n=1
[  505][task:1] INFO  [sensor] prio=Normal(1) n=2
[ 3010][task:3] INFO  [housekeep] prio=Background(1) n=1
```

The bracketed number is the tick. sensor every 500 ms, consumer every 1000 ms,
housekeep every 3000 ms.

Each banner line proves the step before it, so **the last line you see tells you
where it died**. See [Troubleshooting](Troubleshooting).

## Size report

`make build` prints it. `make size` on its own.

```
+----------------+------------+------------+----------------------+--------+
| REGION         |       USED |   CAPACITY | USAGE                |   FULL |
+----------------+------------+------------+----------------------+--------+
| dram_seg       |   16.5 KiB |   64.0 KiB | #####............... |  25.8% |
| vectors_seg    |      963 B |    1.0 KiB | ###################. |  94.0% |
| irom_seg       |   28.4 KiB |    3.2 MiB | #................... |   0.9% |
+----------------+------------+------------+----------------------+--------+
```

Per region, not per section — IRAM or DRAM runs out long before flash.

## Next

[Writing an Application](Writing-an-Application).
