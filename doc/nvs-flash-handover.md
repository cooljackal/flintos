<!-- SPDX-License-Identifier: Apache-2.0 -->

# SPI flash on ESP32 from bare-metal Rust — handover

An unsolved problem, written up for someone else to look at. Everything below
was observed on hardware unless it says otherwise.

## The goal

Implement `kvstore::Storage` over the ESP32's `nvs` partition, so that
`lib/kvstore` — an append-only key/value store that is finished and passing 16
host tests — has somewhere to live. Issue #32.

## The setup

| | |
|---|---|
| OS | FlintOS: `no_std` Rust, **no ESP-IDF**, no vendor SDK |
| Board | M5Stack Atom Matrix, ESP32-PICO-D4 |
| Flash | GigaDevice `0xC84016`, 4 MB, in-package (SiP) |
| Image | ESP-IDF bootloader format, flashed with `espflash` |
| Target partition | `nvs` at `0x9000`, length `0x6000`, read from the boot log |
| Execution | Single core for this work; XIP from flash through the cache |

## The symptom

The board boot-loops: `rst:0x1 (POWERON_RESET)` repeating, and the *bootloader's
own banner comes back truncated* (`boot:0x13 (Sg ha...`). It recovers fully on a
reflash, so the flash contents are not damaged.

It dies at the **first flash read**, inside `Store::open`'s scan. Not on an
erase, not on a write.

The truncated bootloader banner is the interesting part: it suggests SPI1 is
left in a state the cache cannot fetch through, rather than the image being
corrupt.

## Ruled out, with the evidence

**1. The ROM flash driver cannot be used at all.** `esp_rom_spiflash_read`
never returns. Espressif's own linker script says why:

```text
/* always using patched versions of these functions
PROVIDE ( esp_rom_spiflash_wait_idle = 0x400622c0 );
PROVIDE ( esp_rom_spiflash_unlock = 0x400????? );
*/
```

Both commented out; `unlock`'s address is not even recorded.
`spi_flash/esp32/spi_flash_rom_patch.c` exists to replace them. A `read` that
waits on a broken `wait_idle` is a read that never returns.

**2. The instruction cache is not the cause.** Skipping the disable/restore
entirely — leaving the cache running — and calling the ROM read produces an
identical hang. Two sessions went into `Cache_Read_Disable`,
`DPORT_PRO_CACHE_CTRL` and waiting for the cache to report idle; none of it was
relevant.

**3. IRAM placement is correct.** Verified by reading the ELF, not assumed. All
`with_cache_off` monomorphisations sit at `0x400806xx–0x40080784`. Note that a
plain `make build` compiles the self-test out and dead-strips the crate, so the
check must be done with `--features kernel/self-test`.

Related trap: `#[link_section]` says where a function *body* goes and nothing
about a copy the optimiser folded into a caller. It must be paired with
`#[inline(never)]`. Without that, every function here was inlined into `.text`
and the attribute did nothing.

**4. The ROM's chip description is valid.** `apps/flashprobe` prints it off a
running board: device `0x00C84016`, size `0x400000`, block `0x10000`, sector
`0x1000`, page `0x100`, status mask `0xFFFF`. The bootloader populated it
correctly, so theories about missing `esp_rom_spiflash_attach` / `config_param`
are wrong.

**5. The self-test harness is the wrong venue.** It runs in boot context before
the scheduler, and a read that works fine from a task killed the board there
mid-`raw_print`. All flash work now happens in `apps/flashprobe`. This cost a
whole session of confusing results and is worth knowing before reproducing
anything.

## Two implementations tried, both boot-loop

**A. Build the transaction each call.** Set `SPI_USER`/`USER1`/`USER2`, clear
`SPI_CTRL`'s fast-read mode bits, use single-line `0x03` with a 24-bit address
and no dummy cycles. Save and restore `CTRL`, `USER`, `USER1`, `USER2`.

Did **not** save/restore `SPI_ADDR`, `MISO_DLEN`, `MOSI_DLEN` or the `W0..W15`
data buffer, on the assumption the cache reprograms those per fetch. **That
assumption is untested.**

**B. Reuse the cache's setup** (following NuttX's `esp32_readonce`). Leave
`USER`/`USER1`/`USER2`/`CTRL` completely alone — they already describe a
working read in whatever mode the flash is running — and override only
`MISO_DLEN` and `ADDR`, restoring those two. Poll `SPI_CMD != 0` rather than
the `USR` bit.

Also boot-loops, at the same place.

## The contradiction in B, which is probably the lead

B reuses the cache's transaction configuration **while still disabling the
cache around the transaction**. Those two ideas are in tension and it was not
noticed while implementing.

NuttX wraps its transactions in `esp32_spiflash_opstart()` / `opdone()`. Those
were assumed to be equivalent to the cache disable here and **have never been
read**. That is the single most likely place the answer is.

## Reference implementations, ranked by usefulness

- **NuttX** `arch/xtensa/src/esp32/esp32_spiflash.c` — the only one that drives
  SPI1 directly. This is the one to follow.
- **esp-idf** `spi_flash/cache_utils.c` — the cache disable/restore sequence,
  and the comment explaining why the ROM routines are replaced.
- **Zephyr** `drivers/flash/flash_esp32.c` and **Arduino** — both delegate to
  esp-idf's `esp_flash` layer and never touch the ROM. Useful only as evidence
  that nobody calls the ROM directly.

A recurring lesson across this whole effort: **register headers describe bits,
drivers describe order.** Three separate bugs this week were invisible in the
headers and obvious in the driver source.

## Confirmed register facts

SPI1 base `0x3FF42000`. Offsets: `CMD` `0x00` (`USR` bit 18), `ADDR` `0x04`
(address left-justified at `[31:8]`), `CTRL` `0x08`, `USER` `0x1C`, `USER1`
`0x20`, `USER2` `0x24`, `MOSI_DLEN` `0x28`, `MISO_DLEN` `0x2C`, `W0` `0x80`.

`SPI_USER`: `USR_COMMAND` 31, `USR_ADDR` 30, `USR_DUMMY` 29, `USR_MISO` 28,
`USR_MOSI` 27. `USER1`: `ADDR_BITLEN` `[31:26]`, `DUMMY_CYCLELEN` `[7:0]`.
`USER2`: `COMMAND_BITLEN` `[31:28]`, `COMMAND_VALUE` `[15:0]`. Fast-read mode
bits in `CTRL`: QIO 24, DIO 23, QUAD 20, DUAL 14, FASTRD 13.

**Every length field holds n−1.**

Cache control: `DPORT_PRO_CACHE_CTRL` `0x3FF00040` (`ENABLE` bit 3),
`CTRL1` `0x3FF00044` (window mask `[5:0]`), `PRO_DCACHE_DBUG0` `0x3FF003F0`
(`CACHE_STATE` `[18:7]`, idle == 1).

## Where the code is

Branch `wip/nvs-flash`, on top of `main`. Nothing is on `main`.

- `drivers/physical/esp32/flash/src/spi1.rs` — the SPI-NOR commands
- `drivers/physical/esp32/flash/src/lib.rs` — region bounds, cache window
- `kernel/src/nvs.rs` — the `Storage` impl (a newtype: orphan rules)
- `apps/flashprobe/` — the venue for testing this
- `lib/kvstore/` — finished, on `main`, 16 host tests

Reproduce with `make flash APP=flashprobe BOARD=board-m5-atom-matrix PORT=COM5`.
Recover a boot-looping board with `make flash APP=demo BOARD=board-m5-atom-matrix`.

## Correction: reads have never worked

Most of this document, and several commit messages, say reads work and only
erase and program fail. That is wrong, and the way it was wrong is worth
keeping.

A native `SPI_CMD` read whose address register is written unshifted transfers
**nothing**. The loop that follows then copies out `W0..W15` — the controller's
data buffer — which still holds whatever the last page program put there. So a
read returns the bytes most recently written. It looks exactly like a working
round trip.

Measured, by writing a known pattern at `0x100` and then reading both `0x100`
and `0`:

```text
direct wrote [c3, a5, 07, 05, 11, 22, 33, 44, 55, 66, 77, ...]  at 0x100
direct read  [80, 00, 00, 00, 11, 22, 33, 44, 55, 66, 77, ...]  at 0x100
raw@0        [80, 00, 00, 00, 11, 22, 33, 44, 55, 66, 77, ...]  at 0x000
```

Two different addresses returning identical bytes is the tell. `opened, 0 bytes
used` — quoted all week as evidence the read path was sound — never read the
partition at all.

The three native commands do not agree on how the address register is loaded,
which is what made this survive so long:

```c
erase:   WRITE_PERI_REG(PERIPHS_SPI_FLASH_ADDR, addr & 0xffffff);
program: WRITE_PERI_REG(PERIPHS_SPI_FLASH_ADDR, (addr & 0xffffff) | (len << 24));
read:    WRITE_PERI_REG(PERIPHS_SPI_FLASH_ADDR, temp_addr << 8);
```

Shifting the read address to match has **not** fixed it — the symptom is
unchanged — so the transfer is still not happening for some further reason.
Erase and program are believed good; the read is the open item.

## What would most help

A logic analyser on SPI1's clock and data. Every failure so far has been "the
board stopped and I inferred why." Two probes would show in seconds whether the
transaction is even well-formed.
