<!-- SPDX-License-Identifier: Apache-2.0 -->

# RP2040 capability audit (#173)

This audits the README against the RP2040 and the two supported board manifests.
The original audit added no runtime support or new hardware PASS result; later
completed follow-ups are identified below. Green rows retain their bounded
acceptance evidence; capability in a datasheet is not a
FlintOS test result. The plain Raspberry Pi Pico is not a Pico W. [Seeed identifies
the Wio's ESP8285 companion][wio-module], which is separate from its RP2040.

## Kernel rows

| README row(s) | ARM disposition and evidence |
|---|---|
| Boots; preemption / 48 priorities; context switch | Keep verified status: ARM startup, PendSV and scheduler paths, exercised by `apps/tests/arm-selftest`. |
| Interrupts / nesting / critical sections; peripheral routing | Keep verified status: ARM exception/PRIMASK handling and RP2040 NVIC routing, with target IRQ and race fixtures. |
| Tick timer | Keep verified status: SysTick and timer-alarm tests. This does not establish CPU frequency-counter support. |
| Measured CPU clock | **Implemented and Pico-verified, [#174].** The SoC owns the bounded frequency counter; ARM boot initializes SysTick from its result and explicitly labels an unavailable measurement's fallback. Both cores measured the 12/125 MHz profiles against the nominal crystal reference, not an independently calibrated source. See [clock acceptance](rp2040-clock-acceptance.md). |
| Mutexes / inheritance; task and ISR queues; race tests | Keep verified status from the kernel/ARM task and interrupt fixtures; no new scheduling claim in this audit. |
| Watchdogs; reset cause; logging / metrics / panic; stack watermarks | Keep verified status. Recovery tests arm specific watchdog paths; this is not a claim that a system-wide watchdog supervises every application. See [USB recovery limits](rp2040-usb-acceptance.md). |
| Second core / task pinning; DMA | Keep verified status from the SMP and DMA fixtures. This does not imply every peripheral has a DMA driver; I²C/SPI are polled. |
| Task memory isolation | **Unimplemented, [#139].** The RP2040 has an eight-region MPU (datasheet §2.4.6). Neither the ARM context-switch path nor boot configures it. `hal::mpu::MpuManager` is an unwired contract, not protection. The README reports software isolation absent on both architectures, not identical protection hardware. |

The original [ARM target matrix](../tools/rp2040-target-matrix.md) is a planning
contract, not evidence that every proposed test or fixture has been completed.
Current test implementations, issue acceptance results and the specific
acceptance documents below take precedence for measured claims.

## Peripheral, storage and network rows

The hardware inventory comes from the [RP2040 datasheet][datasheet], especially
the summary/address map, PIO chapter and peripheral chapter. A dash means the
named **dedicated block** is absent; it does not mean an equivalent function is
impossible using programmable I/O, bit-banging or an external device.

| Capability | Classification | FlintOS evidence or remaining work |
|---|---|---|
| UART, GPIO, pin routing | Implemented / existing target evidence | Board-owned physical drivers; kernel/driver target fixtures. No universal pin-combination claim. |
| I²C, SPI | Implemented / existing target evidence | [#168 bus acceptance](rp2040-bus-acceptance.md): wired I²C and internal SPI loopback, bounded failure/recovery. External SPI wiring, DMA and automatic chip-select are not verified/provided by these tests. |
| PWM, hardware timers | Implemented / existing target evidence | [#169]: Pico GP2→GP3, 2,000 edges, measured 1,001 µs period and 526 µs high time, zero ISR errors. Timer/alarm coverage is distinct from CPU frequency measurement. Names are generic, not ESP32 LEDC/TIMG blocks. |
| ADC | Implemented / existing target evidence | [#170]: 1,024 internal-temperature samples. Not external-channel accuracy or calibrated temperature. |
| Second ADC, DAC | No dedicated blocks | One ADC, no second independent converter or DAC. External devices need their own driver/fixture. |
| RMT | No dedicated block | ESP32 peripheral; a future RP2040 pulse engine would need its own PIO implementation/test, not an RMT PASS. |
| Cryptographic hardware RNG | Not provided | [#170] measures spaced ROSC samples, exposed as conditioned best-effort entropy. Neither a statistical sample nor conditioning establishes a cryptographic RNG. |
| Flash key/value | Implemented / existing target evidence | [#171 flash acceptance](rp2040-flash-acceptance.md): reserved partition, dual-core exclusion, persistence and watchdog recovery. Whole-store compaction remains non-atomic across power loss. |
| Dedicated CAN/TWAI, I2S | No dedicated blocks | PIO or external controllers are separate implementations. Raspberry Pi's [PIO I2S source][pio-i2s] demonstrates a possible route, not FlintOS support. |
| Native Wi-Fi / BLE | No native radio | Existing ESP32 radio code is not an RP2040 transport. No ARM networking PASS. |
| Wio ESP8285 Wi-Fi companion | Board-supported, no driver | [#176]; Seeed identifies the companion, but its actual interconnect/firmware must be verified before construction. `HAS_WIFI = false` excludes the native SoC radio path, not the board's physical companion. Not applicable to the plain Pico. |
| Touch controller | No dedicated block | An external touch device or capacitive-sensing implementation is separate. Host-tested logical touch drivers do not establish ARM board support. |
| Dedicated SD/SDIO controller | No dedicated block | Raspberry Pi's [PIO SD-card implementation][pico-extras] is a possible alternative, not current support. |
| SD card over SPI | Supported route, no card driver | Reuse cross-architecture [#28]; requires board-owned chip-select, a known card/socket and bounded read/write/removal tests. A working SPI controller is not an SD-card PASS. |
| Ethernet MAC | No dedicated block | An external network controller would need selected hardware, a board transport and its own driver/acceptance scope. Do not claim the ESP32 MAC path works on RP2040. |
| PIO engine | Hardware present, no FlintOS driver | [#175] covers resource ownership and bounded loopback. Protocol implementations such as I2S/CAN/SDIO are later work, not included in that driver's acceptance. |
| USB device (CDC) | Implemented / existing target evidence | [#172 USB acceptance](rp2040-usb-acceptance.md): native data/reset/update/reconnect, soak and bounded fault recovery. Other device classes are not claimed. |
| USB host | Hardware present, no FlintOS driver | [#177]; needs a separate host contract and a safe, verified VBUS fixture before target testing. A PC-to-Pico device cable is not a host fixture. |

## Follow-up boundaries

- [#174] now measures the CPU clock through the existing SoC contract, using the
  [Pico SDK counter sequence][sdk-clocks] with exclusive ownership and bounded
  waits. Its [Pico acceptance](rp2040-clock-acceptance.md) supersedes the original
  audit's configured-only clock finding.
- [#175], [#176] and [#177] require physical resource ownership at HAL/SoC level,
  board construction and portable contracts above it. No chip register access
  belongs in an application or logical driver.
- [#28] is shared storage work, not an ARM-only duplicate. A filesystem is a
  separate layer from the card/block driver.
- [#176] must reuse the shared IP work ([#68]) and applicable connection/recovery
  ([#74]) and TLS ([#131]) work. First determine whether the companion offers
  frames or socket offload; a socket-only interface cannot be passed off as an
  Ethernet frame interface. The attached Wio is also the debug probe: do not
  overwrite its firmware to develop Wi-Fi without a separate fixture or consent.
- [#139] needs actual privilege/region enforcement and negative access tests,
  including scheduling on both cores and explicit DMA limits. MPU presence alone
  is not task isolation.

[datasheet]: https://datasheets.raspberrypi.com/rp2040/rp2040-datasheet.pdf
[wio-module]: https://www.seeedstudio.com/Wio-RP2040-Module-p-4932.html
[sdk-clocks]: https://github.com/raspberrypi/pico-sdk/blob/2.1.1/src/rp2_common/hardware_clocks/clocks.c
[pico-extras]: https://github.com/raspberrypi/pico-extras
[pio-i2s]: https://github.com/raspberrypi/pico-extras/blob/master/src/rp2_common/pico_audio_i2s/include/pico/audio_i2s.h
[#28]: https://github.com/cooljackal/flintos/issues/28
[#68]: https://github.com/cooljackal/flintos/issues/68
[#74]: https://github.com/cooljackal/flintos/issues/74
[#131]: https://github.com/cooljackal/flintos/issues/131
[#139]: https://github.com/cooljackal/flintos/issues/139
[#169]: https://github.com/cooljackal/flintos/issues/169
[#170]: https://github.com/cooljackal/flintos/issues/170
[#174]: https://github.com/cooljackal/flintos/issues/174
[#175]: https://github.com/cooljackal/flintos/issues/175
[#176]: https://github.com/cooljackal/flintos/issues/176
[#177]: https://github.com/cooljackal/flintos/issues/177
