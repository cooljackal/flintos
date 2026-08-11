<!-- SPDX-License-Identifier: Apache-2.0 -->

# Wi-Fi and BLE — implementation plan

How FlintOS gets a radio: by linking Espressif's binary blobs and providing the
OS services they call back into.

**This supersedes issue #39**, which recorded the position that radios were not
coming. That position was correct while the cost was unexamined; this document
is the examined version. The route is viable, the price is known, and the price
is high.

**Status:** Phase 0, Phase 1 and Phase 2 are done, and Phase 3 is at 3.5.

Phase 0's four prerequisites all landed — the DPORT stall (#56), general-purpose
timers (#25), the DMA engine (#18) and persistent configuration (#32). Phase 1
gave the kernel a heap (`lib/heap`, `kernel::heap`); Phase 2 gave it the runtime
object model the blobs demand (`kernel::dynobj`: dynamic queues, semaphores,
recursive mutexes, event groups, and deletable heap-backed tasks). In Phase 3
the `radio/esp32` crate exists with its own tier in `check-layers`, `make blobs`
fetches the libraries at pinned revisions, the 115-pointer OSI table is
generated and implemented, the C-library and RTC symbols the blobs import are
answered, and their interrupt paths are placed in IRAM.

**What is left is 3.6** — PHY enable/disable, PHY init data, RF calibration and
persisting that calibration to flash — plus the two prerequisites this document
used to list under "before 3.6", both of which have since landed:
IRAM-safe interrupt registration (`kernel::interrupt::register_iram_safe` and
`mask_non_iram_safe`, so a flash write no longer masks everything) and
cross-core coordination (the APP CPU is stalled and its cache disabled across a
flash operation, #69).

Per-step ticks below are accurate; this header is the summary. It previously
read "nothing in Phase 1 onward has been attempted", which had been wrong for
some time.

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

The blob allocates constantly. FlintOS had no allocator at all; `lib/heap` and `kernel::heap` are the answer, and this phase is done.

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
| 3.5 | ✅ Place blob ISR paths in IRAM | Done. The blobs' own `.wifi*iram` and `.phyiram` sections are routed by the linker script, and the adapter's ISR-reachable entry points carry `.iram1.radio`. 45.2 KiB measured, against 127 KiB. |
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
| ESP32 ROM | 9 | `ets_delay_us`, `uart_div_modify`, `phy_get_romfuncs`, `roundup2`, `crc32_le`, and four libgcc routines. Covered by `arch/xtensa/esp32.rom.ld`, which provides four of them; the rest resolve elsewhere |
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

### Before 3.6: flash and the radio cannot currently coexist

Researched against esp-idf and NuttX before starting 3.6, because 3.6's own
acceptance criterion — *calibration data persists across a reboot* — means
writing to flash while the radio is up, which is exactly the case below.

**First, a correction.** An earlier note here said the danger was ISRs fetching
from flash while the cache is off. That is not what happens: `with_cache_off`
used to open with `rsil 5`, masking every maskable interrupt, so nothing
fetched anything. The real cost was **latency** — a flash operation blocked
the tick and every driver interrupt for its whole duration, and a sector erase
is tens of milliseconds. Wi-Fi would not have survived that: missed beacons,
and a link that drops under any write load.

**Both reference implementations solve it the same way, and it is not "put
everything in IRAM".** They mask *selectively*:

```c
void IRAM_ATTR esp_intr_noniram_disable(void)   // esp-idf, intr_alloc.c
{
    uint32_t non_iram_ints = non_iram_int_mask[cpu];
    ...
    interrupt_controller_hal_disable_interrupts(non_iram_ints);
}
```

An interrupt declares itself IRAM-safe when it is allocated
(`ESP_INTR_FLAG_IRAM`), and only the ones that did not are masked for a flash
operation. The radio's handlers are IRAM-safe and keep running throughout.

**NuttX does the same, and also parks the other core.** Its
`esp32_spiflash_opstart` — the function this project's flash handover named as
"the single most likely place the answer is", and never read — does, in order:

1. raise the calling task to maximum priority;
2. signal the other CPU through a semaphore and **wait** for it to confirm it
   has parked;
3. `sched_lock()`;
4. `esp_intr_noniram_disable()`;
5. disable the cache on **both** cores.

**Both of the changes this section called for landed under #69**, and 3.6 is no
longer blocked on either:

- **Selective masking.** `kernel::interrupt::register_iram_safe` records the
  promise and `mask_non_iram_safe` masks only the interrupts that did not make
  it, through `INTENABLE`. `PS.INTLEVEL` is still raised, but only across the
  two short windows where `INTENABLE` and the cache registers are changed.
  Nothing has opted in yet, so the behaviour is unchanged until something does
  — the mechanism is in place, the first caller is not.
- **Cross-core.** `with_cache_off` detects a running APP CPU, stalls it,
  disables its cache too, and restores both in reverse order. Proven on
  hardware by `apps/flashprobe`, which starts core 1, joins it to the
  scheduler, and fails the run if it stopped counting across the writes.

FlintOS's stall is a **hardware** stall rather than NuttX's voluntary park, and
that is the one place this still differs from both references. It is safe only
because nothing between the stall and the release takes a lock. The flash
driver's module docs say so and name `esp32_spiflash_opstart` as the shape to
adopt when that stops being true — and scheduling blob tasks on either core is
the most likely thing to make it stop being true, so expect to revisit this
during 3.6 rather than after it.

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
| 5.1 | Bring up the Wi-Fi blobs | **Done.** `esp_wifi_init_internal` returns `ESP_OK` in 179 ms on an ESP32-DevKitC, with all 115 OSI entries filled. |
| 5.2 | Station mode: scan | **In progress.** `esp_wifi_set_mode`, `esp_wifi_start` and `esp_wifi_scan_start` all run; the scan times out with no results. See below. |
| 5.3 | Station mode: associate | The device joins a WPA2 network and holds the association. |
| 5.4 | Coexistence, if BLE and Wi-Fi run together | Both work concurrently under load, not just separately. |
| 5.5 | On-target self-test | Scan and associate run in `make test-target`, skipped cleanly when no AP is configured. |

### What 5.1 actually needs, measured

The archive-wide symbol report (`make blob-symbols`) says 39 symbols are
missing. That number badly over-counts, because it is computed over whole
archives rather than over the members a station build pulls in. The useful
measurement is a link:

> Referencing `esp_wifi_init_internal` from an application and building it
> **links with zero undefined references**, pulling in 211 `ieee80211`/`pp`/
> `wDev_` symbols.

So the OSI table and the adapter already satisfy the Wi-Fi init path at link
time, and 5.1 was not a symbol-hunting exercise. What was left was calling it,
and the answer at run time was different from the answer at link time: **the
table had forty-nine null entries**, and the first call died at
`epc=0x00000000` with nothing to say which. `WifiOsiFuncs::for_each_null`
exists because of that — it names the gap before the blob finds it.

The struct, for the record:

`wifi_init_config_t` (from `esp_wifi.h` at v4.4) is `event_handler`,
`osi_funcs`, an inline `wpa_crypto_funcs_t`, eighteen `int` buffer-sizing
fields, a `uint64_t feature_caps`, a `bool sta_disconnected_pm`, and
`magic = 0x1F2F3F4F` last. Getting the layout wrong is the failure step 3.4
warns about — a magic mismatch if you are lucky, a working radio corrupting
memory if you are not — so the struct wants a layout assertion against the
header, the way `calibration::CAL_DATA_LEN` has one.

Two things are known to be needed beyond the struct:

- **`_event_post`.** The driver reports scan completion and association
  through it, so 5.2 cannot observe its own result without it. It is wired
  now — `adapter::set_event_handler` installs a callback that `_event_post`
  calls **synchronously, on the blob's own task**, rather than through a queue
  and an event task the way esp-idf does. That trade is the thing 5.2 tests:
  the handler must not block and must not re-enter `esp_wifi_*`, and if a real
  scan needs either, the queue is what replaces it.
### Where 5.2 actually is, measured

`apps/wifiscan` gets the driver all the way to a scan and the scan fails.
Everything up to that point works on an ESP32-DevKitC:

```text
[wifi] driver up
radio: no stored RF calibration; calibrating in full      <- on wifiT, not ours
radio: RF calibration stored
[wifi] station started
[wifi] irq: source 0 -> cpu-int 0 on core 0 (connected)
[wifi] intenable=0x000000c1
[wifi] scan 1 failed: 0x300c after 8702011 us             <- ESP_ERR_WIFI_TIMEOUT
[wifi] scan 2 done in 2 ms ... 0 networks, 0 events
```

So: the Wi-Fi task runs, the driver drives the PHY through `_phy_enable` and
calibrates on its own thread, and `_set_intr` routes the MAC interrupt (source
0) to a level-1 CPU input with the mask set. What does not happen is any
event, and any result.

**Two known causes, both concrete.**

1. **`wifi_init_config_t::event_handler` is still a stub.** There are two
   event routes out of the blob. `_event_post` in the OSI table is
   `esp_event_post`, and it is implemented. This field is
   `esp_event_send_internal`, the older `system_event_t` path — and on v4.4 it
   is the one that carries `WIFI_EVENT_STA_START` and `WIFI_EVENT_SCAN_DONE`.
   Wiring the wrong one of the two is easy and was done here. Implementing it
   needs `system_event_t`'s layout, a tagged union over every event payload.
2. **The scan itself times out**, which the event path does not explain — a
   blocking scan waits on the driver's own event group, not on the
   application. The next measurement is whether the MAC interrupt fires at
   all. `radio_esp32::interrupts::for_each_route` reports the routing;
   counting *deliveries* needs somewhere safe to count from, and an atomic
   increment inside the trampoline hangs the board before the driver finishes
   starting — unexplained, and worth understanding before it is worked around.

Two smaller things the run surfaced, both filed here rather than fixed:

- **A stored RF calibration wedges the next boot.** With `nvs` non-empty,
  `esp_wifi_init_internal` never returns; with `make erase` first it takes
  14 ms. `radioprobe` reads the same store without trouble, so the difference
  is the driver reading it from `wifiT` rather than from an application task.
- **~184 bytes leak per scan**, steadily, across rounds.

- **`wpa_crypto_funcs`.** In esp-idf this is filled from
  `libwpa_supplicant`, which is C source and not one of the blobs, so WPA2
  (5.3) means providing AES, SHA, HMAC and PBKDF2 ourselves. An **open** scan
  (5.2) should not need it, which is the reason to do 5.2 before 5.3 rather
  than in issue order.

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
