<!-- SPDX-License-Identifier: Apache-2.0 -->

# RP2040 I²C and SPI acceptance (#168)

The physical drivers live under `drivers/physical/rp2040/{i2c,spi}`. Board
accessors `expansion_i2c()` and `expansion_spi()` open and cache the complete
physical-driver / bus stack. Applications use `I2cController::device()` and
`Bus::transfer()`, with the existing priority-inheritance mutex around each
operation list. Initialize accessors once during board setup, then share them.

## Reproduce on a Raspberry Pi Pico

Connect these **target** pins, not the Wio Debug Probe's pins:

| Signal | GPIO connection | Pico physical pins |
|---|---|---|
| I²C SDA | GP4 ↔ GP6 | 6 ↔ 9 |
| I²C SCL | GP5 ↔ GP7 | 7 ↔ 10 |
| SPI | Internal PL022 loopback | No jumper |

GP2 ↔ GP3 from the GPIO/PWM test can remain. Debug Probe SWD, ground and UART
wiring are unchanged. The short I²C bench loop uses internal pull-ups; an
attached bus needs appropriate external pull-ups. Do not attach another device
while running this fixture: it deliberately holds SCL low during fault injection.

Run `make test-arm-buses`. Override `ARM_PROBE_SERIAL` and `ARM_BOOTSEL_SERIAL`
for a different fixture. The harness flashes through SWD and reads retained
status/counters; UART and manual BOOTSEL are not required for this suite.

## Measured acceptance

| Test | Pass condition |
|---|---|
| SPI0, configured 1 MHz / mode 0 | 4,096 exact patterned bytes, checksum 503,808 |
| I²C0 master → I²C1 slave, configured 100 kHz | 1,001 exact exchanges, 8,008 payload bytes |
| Unanswered I²C address 0x43 | `DeviceNotResponding`, followed by a successful 0x42 exchange |
| SPI shifter disabled with queued data | `Timeout`, FIFO reset, subsequent bytes uncorrupted |
| I²C SCL forced low | `Timeout`, release SCL, subsequent no-ACK and valid exchanges work |
| Both timeout paths | Return between 50 and 100 ms; first measured run: SPI 53,147 µs, I²C 54,098 µs |
| Ownership | Second opens refused; repeated board access returns the same cached bus |
| Both task results | Master and responder completion stages checked before PASS |

These rates are programmed settings, not logic-analyzer clock measurements.
The SPI test exercises the controller, not external wiring or a slave device.
I²C uses real pins, repeated STARTs and STOPs across the two controllers.

The fixture also caught a missing scheduler notification: the new pinned
responder remained Ready while core 1 stayed in idle. Spawn now publishes the
completed task through `make_ready`, notifying the destination after unlocking.
The responder is deliberately spawned at runtime to retain that regression test.

## Bounds and exclusions

- Byte-oriented, polled transfers; no peripheral DMA or ISR-driven bus engine.
- SPI pins: GP19 MOSI, GP16 MISO, GP18 SCK. No hardware CS is routed; callers
  manage their device's select pin. Internal loopback is only a test mode.
- I²C supports 7-bit addresses 0x08–0x77 and currently adjacent SDA/SCL pairs.
  A controller cannot send a zero-length address-only command; such requests
  return `InvalidConfig` instead of silently reading a potentially stateful
  device. The existing `scan()` therefore finds no devices on this controller.
- Transactions have a 50 ms deadline. Recovery resets FIFOs and restores the
  configuration; it cannot repair an external device permanently holding a line
  low. No claim is made for nine-clock bus recovery or multi-master arbitration.
- Not measured on silicon: SPI1, other SPI modes, I²C1 as a master, or other
  I²C rates. The fixture does exercise I²C1 as a slave.

## Vendor references

- [Pico SDK SPI implementation](https://github.com/raspberrypi/pico-sdk/blob/master/src/rp2_common/hardware_spi/spi.c): FIFO outstanding-byte limit and clock division.
- [Pico SDK I²C implementation](https://github.com/raspberrypi/pico-sdk/blob/master/src/rp2_common/hardware_i2c/i2c.c): timing policy, abort-source capture and zero-length restriction.
- [RP2040 I²C register definitions](https://github.com/raspberrypi/pico-sdk/blob/master/src/rp2040/hardware_regs/include/hardware/regs/i2c.h): addressed-only STOP filtering and slave FIFO/clock stretching.
- [Pico SDK slave handler](https://github.com/raspberrypi/pico-sdk/blob/master/src/rp2_common/pico_i2c_slave/i2c_slave.c): read-request and abort clearing.
