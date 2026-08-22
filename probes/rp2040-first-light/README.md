<!-- SPDX-License-Identifier: Apache-2.0 -->

# Wio RP2040 first-light probe

This disposable Pico SDK program separates RP2040 boot/image/clock and board-pin
facts from FlintOS kernel work. It emits `FLINTOS-RP2040-FIRST-LIGHT` once at
115200 8N1 on UART0 TX (GP0), then drives GP13 high for 250 ms and low for
750 ms. After ten cycles it returns to ROM BOOTSEL, so `RPI-RP2` reappearing is
an automated proof that the application executed and leaves the board ready
for the next image. It has no USB stack and does not use the ESP8285.

## Evidence and assumptions

- Raspberry Pi's RP2040 datasheet, section 2.8, is the source for the boot ROM,
  256-byte second stage and core-0 boot behavior.
- Seeed's Wio RP2040 mini documentation identifies a 2 MiB flash, BOOT and RUN
  buttons, and the user-programmable LED on GP13.
- GP0/GP1 are UART0 TX/RX according to the RP2040 function table. Only TX is an
  acceptance signal; RX is configured to keep the pair explicit.
- LED polarity is intentionally not assumed. A visible 250/750 ms pattern
  proves the pin and reveals polarity; absence does not prove a boot failure.

Sources:

- https://datasheets.raspberrypi.com/rp2040/rp2040-datasheet.pdf
- https://wiki.seeedstudio.com/Wio_RP2040_mini_Dev_Board-Onboard_Wifi/

## Build without touching hardware

Use the official `raspberrypi/pico-sdk` tag `2.1.1` (commit
`bddd20f928ce76142793bef434d4f75f4af6e433`) with submodules, export its
absolute path as `PICO_SDK_PATH`, then run:

```sh
cmake -S probes/rp2040-first-light -B build/rp2040-first-light \
  -G Ninja -DPICO_BOARD=pico -DCMAKE_BUILD_TYPE=MinSizeRel
cmake --build build/rp2040-first-light
```

On Windows, run CMake from a Visual Studio Developer Command Prompt. Picotool
is a host program; the SDK builds it from its pinned source and needs the
Windows SDK libraries and tools in that prompt. If those tools are unavailable,
add `-DPICO_NO_PICOTOOL=1`. That still produces the inspectable ELF, BIN, HEX,
map, and disassembly, but deliberately does not produce a UF2.

Inspect `rp2040-first-light.elf`, `.bin`, and `.uf2`. `PICO_BOARD=pico` selects
the RP2040 plus a 2 MiB, generic Winbond-style flash configuration; it does not
claim that this Seeed board is electrically identical to a Pico.

Verify the UF2 with the picotool built by the same SDK checkout:

```sh
picotool info build/rp2040-first-light/rp2040-first-light.uf2
```

The expected report identifies the `rp2040` family and the flash range starting
at `0x10000000`. Record the UF2 SHA-256 before copying it to a board.

## Hardware acceptance

1. Record the PCB/module revision and photograph the header labels.
2. Hold BOOT, tap RUN, and verify ROM USB `2e8a:0003` plus an `RPI-RP2` volume.
3. Copy only this probe's UF2 to that volume. This overwrites the existing
   application in external flash and is therefore not a read-only test.
4. Confirm ten cycles of the GP13 LED pattern and that `RPI-RP2` reappears.
5. Attach a 3.3 V UART receiver to GP0 and ground; confirm the exact marker on
   every reset. Never attach an RS-232-level receiver.

The currently observed `2e8a:000a` device is measured only as Raspberry Pi SDK
CDC firmware. That PID does not prove BOOTSEL mode, the board model, or SWD
wiring.

## Measured run (2026-08-21)

- Setting COM8 to the SDK's 1200-baud reset value entered ROM BOOTSEL; Windows
  reported `2e8a:0003` and mounted `RPI-RP2` as drive H:.
- The flashed UF2 had SHA-256
  `DAA97B251B007D61ABEDC869413293B2A25A14F427944D48DAC7191220ACAC6A`.
- The mass-storage device disappeared when the image booted and reappeared
  after the programmed ten-cycle window. Because only this image requests that
  return, the transition proves boot2 handed off and `main` ran to completion.
- No receiver was connected to GP0, so the physical UART marker and visible LED
  polarity remain unmeasured even though their code path completed.
