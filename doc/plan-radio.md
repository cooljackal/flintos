<!-- SPDX-License-Identifier: Apache-2.0 -->

# Wi-Fi and BLE — implementation plan

How Flint gets a radio: by linking Espressif's binary blobs and providing the
OS services they call back into.

**This supersedes issue #39**, which recorded the position that radios were not
coming. That position was correct while the cost was unexamined; this document
is the examined version. The route is viable, the price is known, and the price
is high.

**Status:** not started. No step below has been attempted.

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
   internal crate. Applications see Flint's API, not FreeRTOS shapes.
5. **`no vendor SDK` leaves the README** when step 6.1 lands. Say it plainly
   rather than quietly dropping the line.

---

## Phase 0 — Prerequisites

Independent of the radio. Do these anyway.

| Step | Work | Done when |
|---|---|---|
| 0.1 | **DPORT cross-core stall.** `dport::enable/disable` read-modify-writes from either core with no protection. The ESP32 hazard is real and the blob needs the same primitive. | Both cores hammer DPORT in an on-target test without a wrong read. |
| 0.2 | **General-purpose timers (#25)** | `esp_timer_get_time` equivalent: 64-bit, microseconds, monotonic across a tick rollover. |
| 0.3 | **Persistent config (#32)** | Key/value in flash, blob values included. PHY calibration lives here — it is load-bearing, not a nicety. |
| 0.4 | **DMA transfer engine (#18)** | Descriptor chains, start/stop, completion interrupt. The MAC moves frames by DMA. |

Steps 0.2–0.4 are already P1 issues. The radio is one more reason to do them,
not the reason.

---

## Phase 1 — A heap

The blob allocates constantly. Flint currently has no allocator at all.

| Step | Work | Done when |
|---|---|---|
| 1.1 | Pick and place a heap region in the memory map | The map still fits below `0x3FFDC200`, and `make size` reports where it went. |
| 1.2 | Implement the allocator | Alloc/free under a host test suite, including fragmentation behaviour. |
| 1.3 | Add the three flavours the blob wants: general, internal-only (DRAM, never PSRAM), DMA-capable | Each returns memory in the right region, proven by asserting the address range. |
| 1.4 | Report exhaustion honestly | `get_free_heap_size` is accurate; an out-of-memory path is tested, not assumed. |

**Note.** Static allocation is a real-time *feature* — no fragmentation, bounded
latency. Confining the heap to the radio (ground rule 3) is what keeps that
true for everything else.

---

## Phase 2 — Dynamic kernel objects

The largest piece of work. Flint's primitives are compile-time sized
(`Queue<T, const N: usize>`); the blob creates and destroys them at runtime with
sizes it chooses.

| Step | Work | Done when |
|---|---|---|
| 2.1 | **Dynamic queues** — runtime length and item size, byte-copy rather than typed | Create/send/recv/delete under host tests, including send-from-ISR and timeout. |
| 2.2 | **Counting semaphores** — none exist today | Take with timeout, give from ISR, tested against the race suite. |
| 2.3 | **Recursive mutexes** — a *second* type; the existing mutex deliberately refuses re-entry and logs it | Nested lock/unlock by one owner, with the non-recursive mutex unchanged. |
| 2.4 | **Task lifecycle** — delete, delay-in-ticks, current-task handle, max priority, yield-from-ISR | Each callable from the adapter; task delete proven not to leak its stack. |
| 2.5 | **Event groups** — 24-bit, wait-for-any and wait-for-all | Host tests for both wait modes and for clear-on-exit. |
| 2.6 | **Spinlocks as opaque handles** — create/delete, wrapping the existing `Spinlock` | The blob's lock/unlock pairs map onto Flint's, with the interrupt-masking order preserved. |

Everything here is testable on the host. None of it needs a radio, and none of
it should wait for one.

---

## Phase 3 — The adapter

| Step | Work | Done when |
|---|---|---|
| 3.1 | New crate for the shim; decide where it sits in the layer check | `make check-layers` passes with the new tier documented. |
| 3.2 | Vendor or fetch the `.a` files; record the licence terms | A clean clone builds without a manual download step, or fails with a clear message saying why. |
| 3.3 | Confirm the Xtensa ABI matches (windowed) and the blobs link | The linker resolves every symbol; unresolved ones are listed, not discovered one at a time. |
| 3.4 | Fill in `wifi_osi_funcs_t` against the pinned IDF version | The struct's version and magic checks pass at runtime. |
| 3.5 | Place blob ISR paths in IRAM | Nothing the radio calls from an interrupt lives in flash. |
| 3.6 | PHY enable/disable, PHY init data, RF calibration | Calibration data persists across a reboot via 0.3. |

**Warning.** Step 3.4 is where a wrong IDF version shows up — as a magic-number
mismatch if you are lucky, and as a working radio that corrupts memory if you
are not.

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

Check this before Phase 1, not after Phase 5.

Today everything is statically placed below `0x3FFDC200`, with 64 KB DRAM and
96 KB of task stacks. Wi-Fi wants roughly 50 KB of working RAM and BLE is
similar. IRAM is 127 KB and already holds the vectors and the second core's
entry.

If the budget does not close, that is worth knowing while the work is still a
document.

---

## Non-goals

- **Asymmetric-core packages.** Out of scope for the whole project.
- **Mesh, ESP-NOW, Wi-Fi Direct.** Not until station mode is solid.
- **PSRAM-backed radio buffers.** Internal RAM only, at least at first.
- **Matching ESP-IDF's API.** Flint's applications see Flint's API.

---

## What would make this the wrong plan

Recorded so the decision can be revisited on evidence rather than mood:

- The blob's memory demands do not fit alongside a useful application.
- The adapter's dynamic objects degrade the kernel's real-time behaviour in a
  way the existing race tests can measure.
- Open-source PHY work matures enough to make clean-room viable — the original
  condition in #39, and still the better outcome if it ever arrives.
