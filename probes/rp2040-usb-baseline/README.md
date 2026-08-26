<!-- SPDX-License-Identifier: Apache-2.0 -->

# Independent Pico SDK USB baseline

This deliberately does not link FlintOS. It separates board/cable/host behavior
from the new driver. Tested with Pico SDK 2.1.1 and its TinyUSB submodule.

```powershell
$env:PICO_SDK_PATH='C:/path/to/pico-sdk'
cmake -S probes/rp2040-usb-baseline -B target/tmp/usb-baseline-build -G Ninja -DPICO_BOARD=pico -DPICO_NO_PICOTOOL=1
cmake --build target/tmp/usb-baseline-build
```

Download the resulting ELF through the selected SWD probe, or its UF2 through
the selected Pico's ROM drive. This replaces the target firmware. Native USB
CDC echoes received bytes. UART0 on GP0/GP1 at 115200 prints clock-independent
heartbeat/USB status and accepts `B` to request SDK ROM BOOTSEL. GPIO24 reads the
Pico's VBUS sense; this wiring is Pico-specific. `connected` includes CDC DTR,
so false does **not** by itself prove failed USB enumeration. The SDK's normal
1200-baud reset is also enabled. No manual BOOTSEL was needed for the measured
enumeration/echo/reset/UF2-reconnect sequence.
