<!-- SPDX-License-Identifier: Apache-2.0 -->

# Wi-Fi and BLE — implementation plan

How FlintOS gets a radio: by linking Espressif's binary blobs and providing the
OS services they call back into.

**This supersedes issue #39**, which recorded the position that radios were not
coming. That position was correct while the cost was unexamined; this document
is the examined version. The route is viable, the price is known, and the price
is high.

**Status:** not started as a radio. Three of Phase 0's four prerequisites have
landed for their own reasons — the DPORT stall (#56), general-purpose timers
(#25) and the DMA engine (#18). Persistent configuration (#32) is the one
left, and nothing in Phase 1 onward has been attempted.

---

## Why blobs

The ESP32 TRM documents every peripheral **except** the Wi-Fi MAC and the BT
baseband. There is no public register map and no published PHY calibration
procedure. Clean-room implementation is not available to us, and an external
radio module would not be ESP32 support.

So: link `libpp.a`, `libnet80211.a`, `libphy.a`, `librtc.a`, `libcoexist.a`,
`libbtdm_app.a`, and implement what they call.

The blobs reach the OS through one struct of function pointers
(`wifi_osi_funcs_t`). Filling that struct in is the whole integration.

---

## Sourcing the blobs

Researched for step 3.2, because "vendor or fetch" was the one open question
that was not technical.

**Every archive is Apache-2.0 — the same licence as FlintOS.** esp-idf pulls
them from four submodules, and each carries a plain Apache-2.0 `LICENSE`:

| Submodule | Repository | ESP32 archives |
|---|---|---|
| `components/esp_wifi/lib` | `espressif/esp32-wifi-lib` | 3.53 MB, 7 files |
| `components/esp_phy/lib` | `espressif/esp-phy-lib` | 3.54 MB, 4 files |
| `components/bt/controller/lib_esp32` | `espressif/esp32-bt-lib` | 0.86 MB, `libbtdm_app.a` |
| `components/esp_coex/lib` | `espressif/esp-coex-lib` | small |

They are ordinary `ar` archives, not Git LFS pointers — checked, they begin
`!<arch>`. So roughly **8 MB** for everything, and less for a station-only
Wi-Fi plus BLE build: `libmesh.a`, `libespnow.a` and `libsmartconfig.a` are
about 1.2 MB of that and are not needed.

So the licence does not decide this. What decides it is what belongs in a
source repository, and the projects nearest to us both **fetch rather than
vendor**:

- **NuttX** clones `esp-hal-3rdparty` and runs `git submodule update --init`
  for `esp_phy/lib`, `esp_wifi/lib`, `bt/controller/lib_esp32` and
  `esp_coex/lib` from its `Make.defs`.
- **Zephyr** declares them in a blob manifest and pulls them with
  `west blobs fetch`, recording a licence and a checksum per file.
- **Arduino** vendors them, but arduino-esp32 ships a *distribution* rather
  than a source tree, which is a different problem.

**Recommendation: fetch at build, pinned by revision and checksum.** Eight
megabytes of binaries in git history is permanent — every clone pays it
forever, and each update pays it again — and fetching also means we are not
redistributing someone else's binaries, so the Apache-2.0 attribution
obligations stay with Espressif where they already are.

It also satisfies this issue's own bar directly: *a clean clone builds without
a manual download step, or fails with a clear message saying why*. A fetch step
that pins a commit and verifies a checksum does both, and a checksum mismatch
is exactly the warning wanted for step 3.4, where a version skew otherwise
shows up as a radio that corrupts memory.

None of the above is legal advice; it is what the licences say and what three
other projects do with them.

## Ground rules

Decide these once, at the start. Changing any of them later is a rewrite.

1. **Pin one ESP-IDF version and stay on it.** The adapter struct is
   version-checked at runtime (`_version`, `_magic`) and is *not* stable across
   IDF releases. Treat a version bump as a port, not an upgrade.
2. **BLE first, Wi-Fi second.** BLE skips the TCP/IP stack and the coexistence
   arbiter. It is the cheaper proof that the whole approach works.
3. **The radio heap is separate from the kernel.** The kernel stays statically
   allocated. Only the blob and its adapter allocate. This keeps real-time
   behaviour a property of the kernel rather than a hope about the radio.
4. **The blob does not get to define the public API.** The adapter is an
   internal crate. Applications see FlintOS's API, not FreeRTOS shapes.
5. **`no vendor SDK` leaves the README** when step 6.1 lands. Say it plainly
   rather than quietly dropping the line.

---

## Phase 0 — Prerequisites

Independent of the radio. Do these anyway.

| Step | Work | Done when |
|---|---|---|
| 0.1 | ✅ **DPORT cross-core stall** (#56) | Done. Not a stall in the end — the documented workaround is an APB pre-read. Both cores hammer DPORT without a lost update. |
| 0.2 | ✅ **General-purpose timers** (#25) | Done. `esp32-timg` gives 64-bit microseconds. A monotonic `esp_timer_get_time` on top is still to write. |
| 0.3 | ✅ **Persistent config (#32)** | Done. `kvstore` over the `nvs` partition, round-tripping across a reboot. Three faults in one driver: reads driven as native commands transferred nothing and returned the stale data buffer, `CMD_ANY` never waited for user transactions, and every user transaction needs one dummy cycle. See `doc/nvs-flash-handover.md`. |
| 0.4 | ✅ **DMA transfer engine** (#18) | Done. Descriptor chains, start/stop, completion by interrupt, proven over SPI. |

Steps 0.2–0.4 were already P1 issues. The radio was one more reason to do
them, not the reason, and three of the four have now landed on their own
merits — which is exactly how this phase was meant to be paid for.

---

## Phase 1 — A heap

The blob allocates constantly. FlintOS currently has no allocator at all.

| Step | Work | Done when |
|---|---|---|
| 1.1 | ✅ Reclaim heap memory **above** `0x3FFDC200` at runtime | Done. `kernel::heap::init` takes SRAM1 as two regions around the ROM's data, plus whatever SRAM2 the static map leaves. Proven on hardware, not assumed: the on-target suite writes patterns through the allocator and reads them back. |
| 1.2 | ✅ Implement the allocator | Done. `lib/heap`, a free-list allocator with address-ordered coalescing. Fourteen host tests including a 2,000-operation churn that asserts every byte comes back. |
| 1.3 | ✅ The flavours the blob wants | Done as **two**, not three — see below. `Caps::Internal` and `Caps::Dma`, each proven by asserting the returned address's region on the chip. |
| 1.4 | ✅ Report exhaustion honestly | Done. `free_bytes` is the sum of the free blocks and `largest_free_block` is what a request can actually get; exhaustion returns null and is tested on host and target. |

**Two flavours, not three.** The plan asked for general, internal-only and
DMA-capable. Internal-only and general are the same thing until PSRAM exists,
which is a non-goal, so a third that aliased the first would only invite a
caller to pick the wrong one.

**And all of it is DMA-capable**, so `Caps` is an API shape rather than two
different pools. This was got wrong first: the heap was built as two pools on
the belief that the DMA engines could not reach SRAM1, and an earlier revision
of this section warned that DMA memory was scarce at about 16 KiB. It is not.
esp-idf:

```c
#define SOC_DMA_LOW  0x3FFAE000
#define SOC_DMA_HIGH 0x40000000

inline static bool IRAM_ATTR esp_ptr_dma_capable(const void *p)
{
    return (intptr_t)p >= SOC_DMA_LOW && (intptr_t)p < SOC_DMA_HIGH;
}
```

Its heap marks the SRAM1 regions `MALLOC_CAP_DMA`, and NuttX puts ordinary
heap regions at `0x3ffe0450` onward. The belief came from a comment in
FlintOS's own linker script that had said "must be inside SRAM2 to be reachable
by the DMA engines" since the DMA work, and `soc-esp32`'s `reachable()` had
been rejecting valid SRAM1 buffers on the strength of it. Both are corrected;
the whole ~126 KiB is available to the adapter for DMA, not 16 KiB.

The lesson is the same one #32 taught: **check the claim against esp-idf rather
than against this repository's own comments.**

**Note.** Static allocation is a real-time *feature* — no fragmentation, bounded
latency. Confining the heap to the radio (ground rule 3) is what keeps that
true for everything else.

---

## Phase 2 — Dynamic kernel objects

The largest piece of work. FlintOS's primitives are compile-time sized
(`Queue<T, const N: usize>`); the blob creates and destroys them at runtime with
sizes it chooses.

| Step | Work | Done when |
|---|---|---|
| 2.1 | **Dynamic queues** — runtime length and item size, byte-copy rather than typed | Create/send/recv/delete under host tests, including send-from-ISR and timeout. |
| 2.2 | **Counting semaphores** — none exist today | Take with timeout, give from ISR, tested against the race suite. |
| 2.3 | **Recursive mutexes** — a *second* type; the existing mutex deliberately refuses re-entry and logs it | Nested lock/unlock by one owner, with the non-recursive mutex unchanged. |
| 2.4 | **Task lifecycle** — delete, delay-in-ticks, current-task handle, max priority, yield-from-ISR | Each callable from the adapter; task delete proven not to leak its stack. |
| 2.5 | **Event groups** — 24-bit, wait-for-any and wait-for-all | Host tests for both wait modes and for clear-on-exit. |
| 2.6 | **Spinlocks as opaque handles** — create/delete, wrapping the existing `Spinlock` | The blob's lock/unlock pairs map onto FlintOS's, with the interrupt-masking order preserved. |

Everything here is testable on the host. None of it needs a radio, and none of
it should wait for one.

---

## Phase 3 — The adapter

| Step | Work | Done when |
|---|---|---|
| 3.1 | New crate for the shim; decide where it sits in the layer check | `make check-layers` passes with the new tier documented. |
| 3.2 | Vendor or fetch the `.a` files; record the licence terms | A clean clone builds without a manual download step, or fails with a clear message saying why. |
| 3.3 | ✅ Confirm the Xtensa ABI matches (windowed) and list what is unresolved | Done. ABI confirmed and all 57 symbols listed; see below. |
| 3.4 | Fill in `wifi_osi_funcs_t` against the pinned IDF version | The struct's version and magic checks pass at runtime. |
| 3.5 | Place blob ISR paths in IRAM | Nothing the radio calls from an interrupt lives in flash. |
| 3.6 | PHY enable/disable, PHY init data, RF calibration | Calibration data persists across a reboot via 0.3. |

**Warning.** Step 3.4 is where a wrong IDF version shows up — as a magic-number
mismatch if you are lucky, and as a working radio that corrupts memory if you
are not.

---

### Step 3.3, done: the ABI and the whole symbol list

**Windowed ABI, confirmed rather than assumed.** `libpp.a` disassembles to 644
`entry`, 813 `retw` and 1361 `call4/8/12` instructions. Those exist only in the
windowed ABI, which is what FlintOS uses — so the blobs and the kernel agree
and no call0 shim is needed.

**Fifty-seven symbols the linked archives need and none of them define.**
`make blob-symbols` produces this list from the archives directly, rather than
by iterating on link failures, which was this step's bar. Where each will come
from:

| Source | Count | Notes |
|---|---|---|
| ESP32 ROM | 9 | `ets_delay_us`, `uart_div_modify`, `phy_get_romfuncs`, `roundup2`, `crc32_le`, and four libgcc routines. FlintOS does not yet include a ROM symbol script — adding one covers all nine at once |
| `compiler_builtins` | 10 | the remaining `__divdi3`-style routines, plus `memcpy`/`memset`/`memmove`/`memcmp` |
| Mesh stubs | 13 | see below |
| PHY and RTC | 9 | `phy_enter_critical`, `rtc_init_clk`, `rtc_get_xtal` and friends — step 3.6 |
| C library | 8 | `abort`, `puts`, `sprintf`, and five `str*` |
| Logging shims | 4 | `phy_printf`, `rtc_printf`, `net80211_printf`, `coexist_printf` |
| Already ours | 2 | `malloc`/`free` onto the radio heap, `esp_dport_access_reg_read` onto `soc-esp32` |
| Odds and ends | 2 | `WIFI_EVENT` (a data symbol), `hexstr2bin` |

So the real writing is around twenty-five functions, most of them small. That
is a much better position than "121 function pointers" suggested.

**Mesh has to be stubbed, not merely skipped.** `libnet80211.a` references
thirteen mesh symbols unconditionally, so excluding `libmesh.a` is not free.
Linking it instead resolves those thirteen but introduces seven more —
including `esp_event_handler_register` and `esp_mesh_send_event_internal`,
which need an event loop FlintOS does not have. Thirteen stubs are the smaller
surface, and they are unreachable in a station-only build because nothing ever
starts mesh.

---

## Phase 4 — BLE

| Step | Work | Done when |
|---|---|---|
| 4.1 | Bring up the controller blob (`libbtdm_app.a`) | The controller initialises and reports a version. |
| 4.2 | Choose a host stack — NimBLE (C) or a Rust host | Decision recorded here, with the reason. |
| 4.3 | GAP: advertise | A phone sees the device. |
| 4.4 | GATT: one read/write characteristic | A phone reads and writes it. |
| 4.5 | On-target self-test | Advertise-and-connect runs in `make test-target`. |

---

## Phase 5 — Wi-Fi

| Step | Work | Done when |
|---|---|---|
| 5.1 | Bring up the Wi-Fi blobs | `esp_wifi_init` equivalent succeeds. |
| 5.2 | Station mode: scan | A scan returns the APs actually in the room. |
| 5.3 | Station mode: associate | The device joins a WPA2 network and holds the association. |
| 5.4 | Coexistence, if BLE and Wi-Fi run together | Both work concurrently under load, not just separately. |
| 5.5 | On-target self-test | Scan and associate run in `make test-target`, skipped cleanly when no AP is configured. |

---

## Phase 6 — Network stack

| Step | Work | Done when |
|---|---|---|
| 6.1 | `smoltcp` — Rust, `no_std`, no C build | It compiles into the tree and `make check-layers` still passes. |
| 6.2 | Wire the MAC to smoltcp's device trait | Frames move in both directions. |
| 6.3 | DHCP | The device gets a lease. |
| 6.4 | DNS, then TCP | A name resolves; a socket connects. |
| 6.5 | TLS — only if someone actually needs HTTPS | Deferred by default. It is a large dependency for a use case nobody has asked for yet. |

---

## Memory budget

Checked, with real numbers, before writing any of the above. It closes — but
only one way.

`make size` on `apps/smp` today:

| Region | Used | Capacity |
|---|---|---|
| `dram_seg` | 20.6 KiB | 64 KiB |
| `task_stacks` | 96 KiB reserved | 96 KiB |
| `iram_seg` | 766 B | 127 KiB |
| `vectors_seg` | 963 B | 1 KiB |

The static map runs `0x3FFB0000`–`0x3FFDB000` and leaves **4.5 KiB spare**
below the `0x3FFDC200` bound. Wi-Fi wants roughly 50 KB and BLE is similar, so
a statically placed heap does not fit and never will.

It does not need to. The linker script already records the way out: memory
above the bound is the ROM's during boot and **reclaimable afterwards**. That
is SRAM2 up to `0x3FFDFFFF` plus SRAM1 (`0x3FFE0000`–`0x3FFFFFFF`, 128 KiB),
which FlintOS does not touch at all today. Espressif's own builds put the heap
there for exactly this reason.

So the heap is reclaimed at runtime, not placed at link time — see step 1.1.

**How much of SRAM1 does the BT controller reserve? None.** That question is
answered, and the answer inverts it. esp-idf takes the reservation off the
*bottom of SRAM2*, not SRAM1:

```text
dram0_0_seg (RW) : org = 0x3FFB0000 + CONFIG_BTDM_RESERVE_DRAM,
                   len = DRAM0_0_SEG_LEN - CONFIG_BTDM_RESERVE_DRAM

config BTDM_RESERVE_DRAM
    hex
    default 0xdb5c if BT_ENABLED
    default 0
```

`0x3FFB0000` is exactly where FlintOS's `dram_seg` starts, so Bluetooth
collides with the static map rather than with the heap. SRAM1 loses only the
ROM's own data — `0x3FFE0000`–`0x3FFE0440` and `0x3FFE3F20`–`0x3FFE4350`, about
2 KiB together — plus 32 KiB of trace memory if trace is enabled, which it is
not. That leaves roughly **126 KiB** for the heap against Wi-Fi's ~50 KB.

Two consequences, both good:

- **The cost is binary, not additive.** `0xdb5c` is the same for BLE-only,
  BR/EDR-only and dual-mode, and Wi-Fi adds nothing. Wi-Fi + BLE costs no more
  static DRAM than BLE alone.
- **Wi-Fi-only pays nothing.** Phase 5 needs no change to the static map, so
  the map surgery is deferred to whenever Bluetooth actually lands.

That surgery is implemented and behind a feature already — see
`tools/build/src/map.rs` and `kernel/src/radio.rs`. A `radio-ble` or
`radio-bt-classic` build reserves the bottom 56 KiB and shifts everything up,
paying for it out of the task stack pool (96 KiB → 80 KiB); a default or
`radio-wifi` build produces byte-for-byte the map that shipped before.

**Configuring it.** Radios are Cargo features, like boards and debug levels:

```bash
make flash APP=demo BOARD=board-m5-atom-matrix EXTRA_FEATURES=radio-wifi,radio-ble
```

`radio-ble` and `radio-bt-classic` both imply the internal `radio-bt`, which is
what the memory map keys on — the reservation is caused by the controller being
enabled, not by which mode it runs. Boards declare `HAS_WIFI` / `HAS_BT` in
their manifest, and asking for a radio the board has not got is a build error.
BLE Mesh is not a flag at this layer: it is a host stack above BLE, so it costs
heap rather than static DRAM.

**IRAM is not a constraint.** 766 bytes of 127 KiB are used, so the blob's
ISR paths have room. `vectors_seg` at 94 % is tight but fixed-size and
unrelated.

---

## Non-goals

- **Asymmetric-core packages.** Out of scope for the whole project.
- **Mesh, ESP-NOW, Wi-Fi Direct.** Not until station mode is solid.
- **PSRAM-backed radio buffers.** Internal RAM only, at least at first.
- **Matching ESP-IDF's API.** FlintOS's applications see FlintOS's API.

---

## What would make this the wrong plan

Recorded so the decision can be revisited on evidence rather than mood:

- The blob's memory demands do not fit alongside a useful application.
- The adapter's dynamic objects degrade the kernel's real-time behaviour in a
  way the existing race tests can measure.
- Open-source PHY work matures enough to make clean-room viable — the original
  condition in #39, and still the better outcome if it ever arrives.
