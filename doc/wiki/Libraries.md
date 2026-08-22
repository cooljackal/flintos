# Libraries

`lib/` holds portable code: device-class contracts and pure algorithms. No
registers, no part numbers, no pins. A `lib` crate may depend only on other
`lib` crates — `tools/check-layers.sh` enforces it — which is what lets the same
code run on any MCU FlintOS supports, and on the host under test.

| Crate | What it is |
|---|---|
| `led-strip` | The `LedStrip` contract an addressable strip promises, plus effects written against it. A driver like `ws2812` implements it. |
| `led-matrix` | Chained panels: turns `(x, y)` into a position along the strip. Depends on nothing, not even `api`. |
| `crypto` | First-party `no_std` primitives — SHA-1/256, HMAC, PBKDF2, AES, CMAC, CCM, key-wrap. Exists for WPA2/WPA3, not as a general crypto lib. |
| `wpa` | A first-party WPA2/WPA3-Personal supplicant: the 4-way handshake, over `crypto`. Replaces the vendored C one. |
| `kvstore` | A key/value store that survives an interrupted write — for calibration and config that must outlive a reboot. |
| `heap` | A free-list allocator over caller-supplied regions. FlintOS has no global allocator by default; this is opt-in. |

## Why this is a separate tier

A driver knows a part number and its output is destined for a pin. A lib knows
neither. Filing `led-matrix` under `drivers/` would be a false statement about
what it is — it turns coordinates into an integer and touches no hardware, so it
is testable on the host with no board at all.

Device-class contracts live here so a driver and the code that uses it never
name each other directly: an application written against `LedStrip` swaps a
`ws2812` panel for another chip by changing one line. `make device-matrix` shows
which drivers keep which contract.

See [Architecture](Architecture) for how `lib/` sits against the driver layers,
and [Writing a Driver](Writing-a-Driver) for implementing a contract.
