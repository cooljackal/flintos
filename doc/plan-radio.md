<!-- SPDX-License-Identifier: Apache-2.0 -->

# Wi-Fi and BLE — implementation plan

How FlintOS gets a radio: by linking Espressif's binary blobs and providing the
OS services they call back into.

**This supersedes issue #39**, which recorded the position that radios were not
coming. That position was correct while the cost was unexamined; this document
is the examined version. The route is viable, the price is known, and the price
is high.

**Status:** Phases 0–3 are done, and Phase 5 station mode is up through the WPA2-PSK connection (5.1–5.3): the radio scans, associates, and completes a full four-way handshake, verified on hardware against a WPA2/WPA3-transition AP and holding the link for minutes (issue #67, closed). What is left is staying connected — there is no IP stack yet, no keepalive, and no GTK rekey (#74). Phase 4 (BLE, #66) has not started.

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
  hardware by `apps/tests/flashprobe`, which starts core 1, joins it to the
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
| 5.2 | Station mode: scan | ✅ **Done.** 14–16 networks per scan on an ESP32-DevKitC, radio interrupts serviced, repeatable across scans. Root cause of the long "zero networks" hunt: the two common-clock OSI callbacks were raw bit operations rather than reference-counted, so the driver's temporary release gated the PHY clock off mid-operation; plus the supplicant callback table the public `esp_wifi_init` registers and the direct internal call skipped. See the closing section. |
| 5.3 | Station mode: associate | ✅ **Done.** Joins a WPA2-PSK network and holds it for minutes on an ESP32-DevKitC, against a WPA2/WPA3-transition AP (#67). The four-way handshake and key derivation run in FlintOS's own Rust supplicant (`lib/wpa` on `lib/crypto`), not a vendored C supplicant; the blob provides MAC/PHY only. Staying connected past the AP's inactivity timeout is #74. |
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
  now. It started as a synchronous call on the blob's own task; the
  piece-by-piece comparison below found that both references queue instead, and
  `radio_esp32::events` is that queue — a bounded ring filled by `_event_post`
  and drained by a dedicated task, so a handler runs on its own stack and may
  call back into `esp_wifi_*`.
### Where 5.2 actually is, measured

**The scan works.** On an ESP32-DevKitC, from an erased flash:

```text
[wifi] driver up
radio: RF calibration stored                              <- on wifiT, not ours
[wifi] event WIFI_EVENT id=2 (sta-start) 0 bytes
[wifi] irq: source 0 -> cpu-int 0 on core 0 (connected)
[wifi] event WIFI_EVENT id=1 (scan-done) 8 bytes
[wifi] scan 1 done in 2102 ms (scan-done event: yes)
```

2.1 seconds for thirteen channels is the right number. Two bugs stood between
that and the previous state, and both were the same kind: **code that existed,
was documented, was host-tested, and had never been executed.**

**1. `wifi_init_config_t::event_handler` had the wrong signature.** The obvious
reading of `system_event_handler_t` is a handler taking a `system_event_t *`,
and that reading was written into `wifi.rs`. The v4.4 header says otherwise:

```c
typedef esp_err_t (*system_event_handler_t)(esp_event_base_t event_base,
                                            int32_t event_id, void* event_data,
                                            size_t event_data_size,
                                            TickType_t ticks_to_wait);
```

The same five arguments as `_event_post`, and esp-idf binds it to
`esp_event_send_internal`, which is a two-line function that calls
`esp_event_post` — the OSI entry — and then forwards to the legacy loop.
So both routes converge, and both now reach `adapter::event_post`.

A one-argument stub sat in the field and linked, ran, and returned `ESP_OK`
for every event: on the windowed ABI a callee may read fewer arguments than it
was passed, so a wrong signature is invisible until you ask where the events
went.

**2. Nothing started the software-timer service.** `ets_timer::start` spawns
the task that fires what the blob arms, and it had **no caller anywhere in the
tree**. The five timer entries were in the OSI table, `collect_due` was
written and host-tested, and no task ever ran it. A scan hops channels on a
software timer, so it advanced only when the driver gave up on each channel:
8.7 s to `ESP_ERR_WIFI_TIMEOUT`, then a late `SCAN_DONE` with zero results.
`wifi::init` calls it now — a service the adapter depends on should not be
each application's job to remember.

### The soak, and what it overturned

Run because "is what I already measured true" had gone unasked for too long.
Two parts, and the second failed.

**Steady state is solid.** One boot, 46 consecutive scans over 5.5 minutes:
0 missed `SCAN_DONE`, 0 faults, 0 dropped events, 0 refused mutex unlocks,
one event per scan and no duplicates, and scan times between 2065 and 2080 ms
— a 15 ms spread over 46 samples.

**Two things that spread corrects:**

1. **There is no heap leak.** It was reported as "~1.9 KB per scan, steady
   enough to be one allocation never freed". Over 46 scans it goes
   121928 → 119904 → 118064 and then stays at **118064 for the remaining 43**.
   That is the driver reaching its steady-state working set in the first three
   scans, not a leak. D1 is closed.

2. **The init hang is not fixed.** Five reboots of the same binary: boots 1
   and 2 reached `driver up` and scanned; boots **3, 4 and 5 hung inside
   `wifi::init`**, after `heap: 149392 bytes`, with no fault — the same
   signature that was attributed first to NVS and then to the missing timer
   service, and declared fixed twice.

**It is correlated with how much is in the `nvs` partition.** After
`make erase`, six consecutive boots all reached `driver up`. Before it, with a
store that had accumulated appends across dozens of boots this session, three
of five hung. `kvstore` is append-only with no compaction and the partition is
24 KiB; a calibration is ~18 records, so roughly ten stores fill it — and a
write onto bytes that are not erased returns `Ok` and lands as garbage,
because NOR flash only clears bits.

So the working theory is **`kvstore` log growth**, not the Wi-Fi driver, and
`make erase` has been masking it all session — which is also why every
"fixed" verdict held: each was measured shortly after an erase.

This outranks the missing MAC interrupt. Every measurement behind B1 was taken
on a board that boots reliably only some of the time.

#### Compaction, and the write that was too wide

`kvstore` now compacts, and the path from `nvs_set_blob` down to the retry is
proven on hardware — 400 writes of one key through the C entry points, two
compactions, `retry_ok=2`, `retry_err=0`, 44196 bytes reclaimed:

```text
[wifi] probe: 400 writes, last rc=0x0, used 9952
[wifi] probe: set_full=2 no_heap=0 compact_err=0
[wifi] probe: compacted=2 reclaimed=44196 retry_ok=2 retry_err=0
```

The first attempt failed, and the reason is worth keeping. `compact` wrote the
whole live set back in **one** `Storage::write`. The ESP32's implementation
copies through a 64-word scratch array and refuses anything longer
(`kernel/src/nvs.rs`, `SCRATCH_WORDS`), so any live set past 256 bytes came
back `Io` — *after* the erase, which is the one window where a failure costs
the store. The write-back now goes one entry at a time, which is what both
references do: esp-idf's `Page::copyItems` calls `writeEntry` per entry
(`nvs_page.cpp:535`), and Zephyr's `nvs_gc` pairs one `nvs_flash_block_move`
with one `nvs_flash_ate_wrt` per entry (`subsys/fs/nvs/nvs.c`, ~line 510) and
chunks even within an entry.

Five host tests passed over this bug because the test `Fake` had no write-size
limit — it modelled flash's *bit* behaviour and not its *transfer* behaviour.
It has the cap now, and the test that fails without the fix is
`a_live_set_wider_than_one_write_still_goes_back`.

#### N2: the init hang reproduces on demand, and compaction does not fix it

Five cold boots of one binary with 9988 of 24576 bytes in the store, no erase
between them: **five hangs out of five**, every one stopping at the same line
and the same millisecond.

```text
[  218][wificonnect] INFO  nvs: 9988 used, 14588 free, one get = 14114 us
                                   <- nothing further, no fault
```

That is a much better handle than the soak's "three of five". Three things it
establishes, and one it does not:

- **It is content-dependent.** The same binary with the partition erased boots,
  scans, and reports `0 networks` in 2070 ms.
- **Compaction does not prevent it.** The log only compacts when it is *full*,
  and 9988 bytes is not full. Nothing about the N1 fix touches this.
- **It is not fullness either.** 9988 of 24576 hangs.
- **But it is also timing-marginal, which rules the simple story out.** The
  same store, the same partition contents, with four `log_info!` lines added
  inside `wifi::init`, boots — and the markers all print, so the region they
  cover is not where it stops. A hang that four log lines move is not a hang
  caused by the number of bytes on flash; the byte count is a condition, not
  the mechanism.

The reads are the thing those two have in common: one `get` over a
9988-byte log costs **14106 us**, against 3347 us at 2272 bytes, and every one
of them runs with the instruction cache off (see "flash and the radio cannot
currently coexist" above).

**The second core is not involved, measured.** The obvious suspect was the one
place this tree diverges from both references: `esp32_flash` asks core 1 to
park and falls back to a *hardware stall* if it does not answer, and neither
reference has that fallback — NuttX's `esp32_spiflash_opstart` coordinates by
semaphore in SMP (`esp32_spiflash.c:676-682`) and Zephyr's ESP32 flash driver
requests remote-core work over IPM (`drivers/flash/flash_esp32.c:369-396`).
A stalled core can be holding a lock; a parked one provably is not. But after
400 writes, two compactions and a full-log read the counters say:

```text
[wifi] cache: 0 parks, fell_back=false, last_state=0x0
```

Zero. `appcpu::is_running()` is false in this application, so core 1 is never
asked and never stalled. The park path is not the mechanism, and the whole
second-core question is out of scope for N2.

#### Nine boots after an erase, all clean

The instrumented binary, `make erase`, then nine consecutive boots:

| Boot | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 |
|---|---|---|---|---|---|---|---|---|---|
| `used` | 0 | 2272 | 2308 | 2344 | 2380 | 2416 | 2452 | 2488 | 2524 |
| one get, us | 184 | 3846 | 3986 | 4090 | 4242 | 4378 | 4523 | 4651 | 4791 |
| up | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

Growth is +36 bytes and about +130 us a boot, dead linear. **No hang, so the
wait instrument caught nothing.** That is a longer clean run than the earlier
"three of five hung", on a binary that differs in two ways: the log is bounded
now, and `block_send`/`block_recv` take one more lock to record the wait. The
second may well have moved the timing — that is a real possibility and not a
reason to call N2 fixed.

The bound is `nvs::compact_if_grown`, at one sector. At +36 bytes a boot from
2524 it will not fire until boot ~53, so nine boots do not exercise it; what
they show is the growth it is there to stop.

#### N2 is a blocked task, not a dead system — and not the log length

The `wifiwatch` task, spawned above the application's priority precisely to
answer this, **runs for the whole hang**:

```text
[wifi] nvs: 2536 used, 22040 free, one get = 4097 us
[wifi] watch 1: still in stage 1
...
[wifi] watch 8: still in stage 1
```

Three boots in a row, eight seconds each. So the scheduler is alive, the tick
fires, tasks switch — **one task is blocked inside `wifi::init`** and
everything else is fine. That is a wait that is never signalled: a semaphore,
a queue, or a mutex the adapter hands the blob and never posts.

And it happens at **2536 bytes**, with a read costing 4097 us. So the log
length is not the condition either, and the section below — written when the
only evidence was 9988 bytes hanging and an erased partition not — claims a
fix that the next five boots refuted. The bounded log is still worth having
for its own reasons; it does not fix this.

The pattern across those five boots is **boots since erase**, not bytes: two
clean, then three hung, and the store frozen at 2536 because a hung boot never
writes a calibration. That matches the soak's "three of five" and the six
clean boots after `make erase`.

#### The log bound: Zephyr's invariant, not Zephyr's mechanism

`nvs_gc` (`subsys/fs/nvs/nvs.c`) reclaims the sector the write pointer is
leaving, so Zephyr's log is always about one sector long and a read never
walks more than that. Ours compacted only at `Full`, so it sat at ten
kilobytes for nearly all of its life and a read cost what the log was long.

The literal trigger does not transfer, and trying it proved that: a
calibration rewrite adds **36 bytes a boot**, so "the write pointer crossed a
sector" fires about once in a hundred boots. Boot two after that change hung
at 10096 bytes, having never crossed anything.

What transfers is the invariant — *keep the log inside a sector*. Checked once
at startup, after the heap exists and before the driver reads anything:

```text
[  218] nvs: 10132 used, 14444 free, one get = 14583 us   <- the hang condition
[  218] radio: nvs compacted, 7740 bytes reclaimed
[  219] nvs compacted at boot: 2392 used, 22184 free
[  275] driver up                                          <- 5/5 hung here before
```

That boot met the 10132-byte condition and survived. It does **not** follow
that the growth was the cause: the five boots after it hung three times at
2536 bytes, so whatever N2 is, it is not the log length. What this change
buys is a read that stays at ~4 ms instead of growing to ~15 ms, and a
partition that cannot fill.

`Store::compact` also no longer erases when nothing is superseded, so a
threshold check that fires on a store of distinct keys costs a scan and no
flash cycle.

**Size alone does not reproduce it, measured.** Filling the log to 10024
bytes — just past the 9988 that hung five times out of five — on the same boot
that then runs `init` produced a clean bring-up: `driver up` at 293 ms, scan
running. So "N bytes in the store" is not the condition. What separates the
two is that the hung boots read a log written by *earlier* boots, while this
one read a log it had just written itself. The erased-versus-9988 contrast
still stands; the byte count on its own does not explain it.

The tick was the other candidate and it is also out: `rearm_this_core_inner`
in `arch/xtensa/src/tick.rs` already catches up when it has fallen behind by
more than one period, so a long masked window costs ticks and not the timer.

What that leaves is single-core: each window masks every non-IRAM interrupt
(as both references do — `esp_intr_noniram_disable`, and NuttX's opstart at
`esp32_spiflash.c:687-689`, which also takes `sched_lock`), and a 14 ms read
is roughly seventy of those windows back to back. Where NuttX differs is size:
it caps one window at **64 bytes** (`SPI_FLASH_READ_BUF_SIZE`,
`esp32_spiflash.c:64`, loop at 2107-2125) against this tree's 256. Untested
whether that matters.

Reproducing it: set `NVS_FILL_PROBE` in `apps/tests/wifiscan` to `true`, boot once to
fill the partition, set it back, and reboot without erasing.

The counters behind those log lines live in `radio_esp32::nvs::probe()`, and
the fill loop is `NVS_FILL_PROBE` in `apps/tests/wifiscan`, off by default because it
writes junk that only `make erase` clears. They exist because the run before
them was read as "compaction ran and the retry failed" on the strength of a
log line that was *missing*, which turned out to say nothing at all.

### Where 5.2 stands

**The whole scan cycle runs, repeatably.** Three boots, three scans each, no
erase between them:

```text
[wifi] scan 1 started (0 refused mutex unlocks so far)
[wifi] event WIFI_EVENT id=1 (scan-done) 8 bytes      <- on radio-event
[wifi] 0 networks                                      <- read in the handler
[wifi] scan 1 done in 2105 ms, 2 events, 0 dropped
```

`esp_wifi_scan_get_ap_num` returns now. It did not before the event queue, and
the two earlier explanations for that were both wrong — first "a populated
`nvs` partition" (it was the missing timer service), then a priority inversion
(raising the timer task to 22, then above `wifiT`, changed nothing). What it
actually was is the third divergence in the comparison below: the driver
expects its results to be collected by whoever handled the event, on a task of
its own, and we were reading them from the application after a blocking scan.

**It finds nothing.** Zero networks, every scan, in a room with several access
points. So the scan machinery is right and the radio is not receiving. **Measured
now, not guessed: the MAC interrupt never fires.** `_set_intr` routes source 0
to CPU input 0, `INTENABLE` carries the bit, and
`interrupts::fires(0)` reads **zero** after every scan.

That counter is only possible because of the `SCOMPARE1` fix below; it used to
hang the board.

**The OS side is now ruled out, by measurement rather than inspection:**

| Checked | Reads | Verdict |
|---|---|---|
| Crossbar, `DPORT_PRO_WIFI_MAC_INTR_MAP` | `routed_to(0) == Some(0)` | source 0 → CPU int 0, as written |
| `INTENABLE`, core 0 | `0x000002c1` — bits 0, 6, 7, 9 | the MAC's input is unmasked |
| Raw `INTERRUPT`, sampled every 1 ms through a whole scan | `0x00018000` — bits 15, 16 only | **bit 0 never asserts** |

Both references do exactly what we do here and no more: esp-idf's
`set_intr_wrapper` is one call to `intr_matrix_set`, NuttX's is
`esp_rom_route_intr_matrix` plus bookkeeping for its own vector table, and
both leave the unmasking to `_ints_on`. There is nothing in either that this
adapter omits.

So the Wi-Fi MAC is not asserting its interrupt line, and the cause is
upstream of the crossbar — the RF/PHY receive path, or the driver never
putting the MAC into a receiving state. That is where 5.2 continues.

### Who touches the receiver's event word: exactly one function

From the blobs, no board needed. `hal_mac_interrupt_get_event` and
`hal_mac_interrupt_clr_event` are defined in `hal_mac.o` and referenced from
**one object only**, `wdev.o`, by **one function**, `wDev_ProcessFiq` — the
driver's own MAC interrupt handler.

That closes the loophole in the zero reading. Nothing else in the driver
reads or clears the event word, and `wDev_ProcessFiq` is not running, because
the CPU-side interrupt has never fired. So a raised event would still be
sitting there when sampled, and it never is. **The receiver is enabled and is
not raising events**, which is a stronger statement than the earlier one and
this time rests on measurement plus the blob's own symbols rather than on an
assumed register map.

In our image `wDev_ProcessFiq` links at `0x40082630`, in IRAM as it must be.

The event word is at `0x3ff73c48` and its clear at `0x3ff73c4c`. The
neighbouring `0x3ff73c40` is referenced eleven times and is the obvious
candidate for the enable, unidentified so far.

**The next check is one line and needs no new machinery.** The adapter
records the handler pointer the driver installs through `_set_isr`. Print it
and compare against `0x40082630`. If they match, the registration is right
and the silence is upstream of it. If they do not, the driver's handler is
not the one we wired up, and that would explain everything downstream of it.

### Retracted: the register readings behind this section were wrong

**The two addresses sampled were not interrupt registers.** `0x60033004` and
`0x60033010` are Wi-Fi *address-filter* slots. The value read back as evidence
of MAC activity, `0x0000cc13`, is this board's own MAC address suffix —
`c0:49:ef:d1:13:cc` — sitting in a filter slot, not an interrupt status word.
Reception is enabled at `0x60033084`; interrupt status lives at `0x60033c48`.
Neither was ever read.

So everything below that rests on "the MAC generates events and its interrupt
mask is zero" is unsupported. What survives is narrower: the scan completes
and reports nothing, and every OS-side path listed in the eliminations was
checked against the references and matched. Where the receive path actually
stops is unmeasured.

The eliminations themselves stand — the tuning data, the power domain, the
clocks, the enable sequence, the thread affinity — because none of them
depended on those two reads.

**And there is a defect in this tree that the evidence now points at
instead.** `Semaphore::take` calls `try_take`, which fails and releases its
lock, and only then calls `block_recv` to enrol as a waiter. A `give` landing
in that window wakes nobody and the permit is left sitting while the caller
blocks — for ever, when the timeout is infinite, which is what the driver
asks for. That is exactly the start-up hang: one give, one take, still
blocked, which was recorded here as unexplained. The same shape is in the
queue and event-group paths.

Fix that before remeasuring anything on the radio, then read the real
registers: `0x60033084` for receive enable, `0x60033088`-`0x60033090` for the
descriptors, `0x600332cc` and `0x600332d0` for the receive counters, and
`0x60033c48` for interrupt status.

### B1 narrowed: the receive-interrupt transition is not happening

Sampled every millisecond through a whole scan, OR-accumulated:

```text
[wifi] raw INTERRUPT 0x00018000 core 0 crossbar[src0]=Some(0)
[wifi] wmac raw 0x0000cc13 ena 0x00000000
```

`WMAC_INT_RAW` carries bits 0, 1, 4, 10, 11, 14 and 15 during a scan, so **the
MAC core is generating events**. That is activity, not decode: it does not
by itself establish that frames are being received and demodulated, and it
should not be read that far. What it does rule out is a block sitting inert.
`WMAC_INT_ENA` is **zero**, so none of it leaves the block. That is why every
downstream measurement was correct and useless: source 0 routes to CPU
interrupt 0, `INTENABLE` carries the bit, the handler is installed, and the
MAC never asks.

**The register belongs to `libpp`.** Searched every archive for the block's
base as a literal: `libpp.a` references `0x3ff73000` three times, and
`libnet80211`, `libcore`, `librtc` and `libphy` not at all. `0x3ff73000` and
the `0x60033000` the application reads are the ESP32's two windows onto the
same peripherals — `0x3ff40000` and `0x60000000`, same `0x33000` offset — so
the reads are valid and libpp is the only thing that touches it.

**Neither reference arms it from the adapter, and neither do we:**

| Entry | NuttX | Ours |
|---|---|---|
| `_set_isr` | `xt_set_interrupt_handler`, records only (`esp32_wifi_adapter.c:1086-1096`) | records into a slot behind a per-CPU-interrupt trampoline |
| `_set_intr` | `esp_rom_route_intr_matrix` + vector-table bookkeeping, no enable, no MAC write (1554-1589) | `interrupt::connect` + records the route |
| `_clear_intr` | empty stub (1591-1596) | empty stub |

Zephyr has no adapter of its own — it takes esp-idf's `wifi_os_adapter` from
`hal_espressif`, so its answer is esp-idf's, and esp-idf's `set_intr_wrapper`
is one `intr_matrix_set`.

So the mask is armed inside `libpp`, and **the transition that would arm it
is not happening**. Which prerequisite prevents it is not identified. Two
readings fit equally: the driver reaches the decision and declines, or it is
waiting on something it has not been given and never reaches the decision at
all. Nothing measured separates them.

The eliminations above are real and worth keeping — the tuning data, the
power domain, the clocks, the enable sequence, the thread affinity. They do
not add up to "our side is correct": every one of them rules out a specific
mechanism, and the leading theory remains that something this tree supplies
is subtly wrong in a way the driver accepts and then acts on.

The start-up hang has the same shape — a wait that never completes — and
**no evidence links the two**. They are tracked separately until something
does.

### B2 done, B1 unchanged

The bring-up order now matches Zephyr: `esp_wifi_init`, then
`esp_wifi_set_mode(NULL)`, then `esp_wifi_start`, and STA only afterwards
(`drivers/wifi/esp32/src/esp_wifi_drv.c:1854-1867` -- it sets NULL at init and
moves to STA when something asks). This tree went straight to STA before the
start. Changed, boots clean, scan completes in 2073 ms -- and still
**0 networks, 0 radio interrupts**. So the order was wrong and was not the
cause.

Two B1 candidates checked and eliminated without a board:

| Candidate | Verdict |
|---|---|
| `phy_wifi_enable_set(1)`, which NuttX calls beside `esp_phy_enable` (`esp32_wifi_adapter.c` ~2280-2310) | **Not applicable.** The symbol is absent from every archive in the pinned v4.4 blob set; NuttX tracks a newer one. `libphy.a` exports `enable_wifi_agc`, `mac_enable_bb` and `phy_enable_low_rate`, all called from inside `libpp`. |
| Wi-Fi clock mask | Correct. `_wifi_clock_enable` sets `RADIO_CLK_WIFI` = `DPORT_WIFI_CLK_WIFI_EN` (0x406, bits 1, 2, 10), which is what NuttX's `wifi_clock_enable` sets and nothing more. |

**The untested divergence left over from the same reading:** Zephyr registers
an RX callback during init --
`esp_wifi_internal_reg_rxcb(ESP_IF_WIFI_STA, eth_esp32_rx)`
(`esp_wifi_drv.c:1779`) -- and this tree registers none, having no netif to
deliver frames to. Whether the driver enables hardware receive at all without
one is unknown, and it is the next thing to try.

### What the references do with the blob's timers, and what ours does

Checked because the hang needs `radio-timer` to exist, and the answer is that
**our timer service is the wrong shape**, not merely the wrong priority.

All three references route the five OSI timer entries the same way:

| | `_timer_arm*` reaches | Backend | Dispatch |
|---|---|---|---|
| esp-idf v4.4 | `ets_timer_arm` → `esp_timer_start_once`/`_periodic` | **TG0 LAC timer**, 64-bit up-counter with a programmable alarm and a level interrupt | ISR does `vTaskNotifyGiveFromISR`; `timer_task` at `ESP_TASK_PRIO_MAX - 3` (22) on PRO_CPU |
| NuttX | `esp_timer_arm_us` → `esp_timer_start_once`/`_periodic`, `dispatch_method = ESP_TIMER_TASK` | the same `esp_timer` | the same |
| Arduino | esp-idf unchanged — arduino-esp32 ships the IDF as precompiled libraries | the same | the same |

Arduino is not independent evidence; it is esp-idf with a different build
system, and is listed so that is explicit rather than counted twice.

**Ours is a task that wakes every millisecond and polls sixteen slots.**
`_timer_arm_us(t, 100, false)` therefore fires at the next poll — up to ten
times late, and jittered by whatever else is runnable. That is a functional
gap rather than a performance one: the Wi-Fi MAC arms microsecond timers, and
a state machine given them ten times late is being lied to.

It is also the strongest remaining explanation for the hang, because it
predicts both of its properties: it needs the timer task to exist (with no
service, the driver blocks and times out cleanly instead), and it moves when a
few microseconds of UART output shift the phase between the poll and what the
blob expected.

**The port costs no general-purpose timer, which is the thing that would
otherwise block it.** The LAC timer is a separate counter inside a timer group
(`TIMG_LACTCONFIG_REG`), not one of the four GP timers — which is exactly why
esp-idf chose it. FlintOS has all four spoken for: TIMG1/T1 is
`kernel::clock`, and TIMG0/T0, TIMG0/T1 and TIMG1/T0 drive on-target
self-tests. TG0's LAC is free.

That is now done: `kernel::alarm` owns TG0's LAC counter,
`esp32-timg::lact` drives it, and `radio_esp32::ets_timer` sleeps on the alarm
instead of the tick. The handler is registered IRAM-safe, so the radio's timers
keep running through a flash operation.

Two smaller things, still open:

- **~184 bytes leak per scan**, steadily, across rounds.
- **An atomic `fetch_add` inside the interrupt trampoline hangs the board.**
  Reproducible, unexplained, and plausibly the same race — both are "add a few
  instructions to a path near the radio and the board stops".

- **`wpa_crypto_funcs`.** In esp-idf this is filled from
  `libwpa_supplicant`, which is C source and not one of the blobs, so WPA2
  (5.3) means providing AES, SHA, HMAC and PBKDF2 ourselves. That crypto now
  exists — `lib/crypto` (PBKDF2, HMAC-SHA1, AES, CMAC, keywrap and friends),
  all host-tested — and the four-way handshake runs above it in `lib/wpa`,
  FlintOS's own Rust supplicant, hardware-validated under 5.3. An **open** scan
  (5.2) did not need it, which is the reason 5.2 came before 5.3 rather than in
  issue order.

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

`make size` on `apps/tests/smp` today:

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

## Empty-scan resolution (2026-08-16)

- **Measured root cause:** the two common-clock OSI callbacks performed raw
  set/clear operations, while IDF reference-counts them. The driver's temporary
  release cleared `RADIO_CLK_COMMON` although the PHY still held a reference;
  all 21 PHY digital registers consequently read zero.
- **Measured result:** reference-counting the common clocks brought up PHY
  calibration, filled the receive path, and produced 10 AP records plus 58
  radio interrupts on the same board that previously returned zero.
- **Second required integration step:** IDF's public `esp_wifi_init` registers
  supplicant callbacks after `esp_wifi_init_internal`. A scan-safe table is now
  registered because station start and scan dereference it before association.
  Security metadata remains deliberately marked unparsed until the real
  supplicant and crypto functions are supplied.
- **Startup order corrected:** station mode is selected before start, matching
  IDF. The earlier NULL-start-STA order only appeared viable while PHY clocks
  were off.

---

## WPA2 station connect (2026-08-17)

- **Done, on hardware.** The station completes a full WPA2-PSK connection and
  holds the link for minutes against a WPA2/WPA3-transition AP. Scan and
  associate (5.2, 5.3) close with it; issue #67 is closed.
- **The supplicant is first-party Rust, not a vendored C one.** The four-way
  handshake and key derivation run in `lib/wpa`, built on `lib/crypto`
  (PBKDF2, HMAC-SHA1, AES, CMAC, keywrap — all host-tested). The Espressif blob
  (libnet80211/libpp) provides MAC/PHY only.
- **The integration seam.** The blob drives a `wpa_funcs` callback table
  registered through `esp_wifi_register_wpa_cb_internal`; the station callbacks
  (sta_init/connect/rx_eapol and the rest) are filled in
  `radio/esp32/src/supplicant.rs`. `parse_wpa_ie` classifies AP security; the
  station RSN IE is installed for the association request via
  `esp_wifi_set_appie_internal` (flag=0, copy path); the AKM list is masked to
  PSK so the blob does not select SAE; keys install through
  `esp_wifi_set_sta_key_internal` after message 4.
- **New app.** `apps/tests/wificonnect` joins a WPA2 network, with credentials
  supplied from the environment at build.
- **What is not done (#74).** Staying connected past the AP's inactivity
  timeout: there is no DHCP or IP stack yet, no keepalive, and no GTK-rekey
  handling — the supplicant handles the initial four-way only. Nothing above
  the link works: no networking, no sockets, no IP.

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
