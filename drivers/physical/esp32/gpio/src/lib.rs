// SPDX-License-Identifier: Apache-2.0

#![no_std]

use hal::bus::{BusError, BusResult};
use soc_esp32::reg;

/// ESP32 GPIO driver (pins 0-39; 32-39 are input-only on real silicon but the
/// register plumbing is symmetric, so this driver does not special-case them).
/// Base address: 0x3FF44000.
pub struct Esp32Gpio {
    base: u32,
}

// ── Register map ─────────────────────────────────────────────────────────────
//
// ESP32 TRM chapter 4 (IO MUX and GPIO Matrix), GPIO register summary; offsets
// confirmed against esp-idf `soc/gpio_reg.h`. The ESP32 has 40 GPIOs split
// across two parallel register files: pins 0-31 use the base registers below,
// pins 32-39 use the `*1` variants at a fixed +0x2C/+0x2C-ish offset (the
// layout is not a uniform stride, hence the explicit table rather than
// `base + pin/32 * K`).
//
// A prior revision had these shifted by a whole register: ENABLE pointed at
// what is really OUT1 (0x10), IN pointed at what is really SDIO_SELECT
// (0x1C), STATUS pointed at what is really ENABLE_W1TS (0x24), and
// STATUS_W1TC pointed at what is really ENABLE_W1TC (0x28) -- so
// `clear_interrupt` was silently disabling the pin as an output instead of
// clearing its interrupt-status bit.

#[allow(dead_code)] // Documented for completeness of the map; writes go through OUT_W1TS/OUT_W1TC.
const GPIO_OUT: u32 = 0x04;
const GPIO_OUT_W1TS: u32 = 0x08;
const GPIO_OUT_W1TC: u32 = 0x0C;
#[allow(dead_code)] // Documented for completeness of the map; writes go through OUT1_W1TS/OUT1_W1TC.
const GPIO_OUT1: u32 = 0x10;
const GPIO_OUT1_W1TS: u32 = 0x14;
const GPIO_OUT1_W1TC: u32 = 0x18;
#[allow(dead_code)] // Not driven by this driver; documented for completeness of the map.
const GPIO_SDIO_SELECT: u32 = 0x1C;
#[allow(dead_code)] // Documented for completeness of the map; writes go through ENABLE_W1TS/ENABLE_W1TC.
const GPIO_ENABLE: u32 = 0x20;
const GPIO_ENABLE_W1TS: u32 = 0x24;
const GPIO_ENABLE_W1TC: u32 = 0x28;
#[allow(dead_code)] // Documented for completeness of the map; writes go through ENABLE1_W1TS/ENABLE1_W1TC.
const GPIO_ENABLE1: u32 = 0x2C;
const GPIO_ENABLE1_W1TS: u32 = 0x30;
const GPIO_ENABLE1_W1TC: u32 = 0x34;
const GPIO_IN: u32 = 0x3C;
const GPIO_IN1: u32 = 0x40;
#[allow(dead_code)] // Read-modify-capable status; not currently read wholesale.
const GPIO_STATUS: u32 = 0x44;
#[allow(dead_code)] // No interrupt-arm path implemented yet.
const GPIO_STATUS_W1TS: u32 = 0x48;
const GPIO_STATUS_W1TC: u32 = 0x4C;
// GPIO_STATUS1_REG is not in the offset table the review confirmed, but is
// needed to clear interrupt status for pins 32-39 without silently writing
// the pin-0-31 register instead. Confirmed against esp-idf `soc/gpio_reg.h`
// (`GPIO_STATUS1_W1TC_REG` = base + 0x58) as a direct continuation of the
// STATUS/STATUS_W1TS/STATUS_W1TC group above.
const GPIO_STATUS1_W1TC: u32 = 0x58;

/// Highest valid ESP32 GPIO number.
const MAX_PIN: u8 = 39;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinMode {
    Input,
    Output,
    InputPullUp,
    InputPullDown,
    OutputOpenDrain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinLevel {
    Low = 0,
    High = 1,
}

/// Resolve a GPIO number to (use the `*1` register file, bit mask within it).
///
/// `None` if `pin` is not a valid ESP32 GPIO number (> 39).
fn pin_bit(pin: u8) -> Option<(bool, u32)> {
    if pin > MAX_PIN {
        return None;
    }
    if pin < 32 {
        Some((false, 1u32 << pin))
    } else {
        Some((true, 1u32 << (pin - 32)))
    }
}

impl Esp32Gpio {
    /// Bind a driver instance to the GPIO register block at `base_addr`.
    ///
    /// # Safety
    /// `base_addr` must be the base address of the real ESP32 GPIO register
    /// block (0x3FF44000) and must not be concurrently owned by another
    /// `Esp32Gpio` instance or any other code performing raw MMIO at the same
    /// address -- this type performs unchecked `read_volatile`/
    /// `write_volatile` at `base_addr + offset` with no further validation of
    /// the address itself. Passing an arbitrary address lets otherwise-safe
    /// code corrupt unrelated memory-mapped state.
    pub unsafe fn new(base_addr: u32) -> Self {
        Self { base: base_addr }
    }

    /// The chip's one GPIO controller, shared.
    ///
    /// Apps and self-tests used to each call `Esp32Gpio::new(GPIO_BASE)`,
    /// which spelled the address in four places and made every caller take on
    /// an `unsafe` whose only obligation was "this really is the GPIO block".
    /// This is that obligation discharged once.
    ///
    /// Sharing one instance is sound where several `new` instances were
    /// not: the struct is only a base address, every method takes `&self`,
    /// and each operation is a single write to a write-1-to-set /
    /// write-1-to-clear register or a single read, so two callers working
    /// different pins never read-modify-write the same word. Two callers
    /// driving the *same* pin are a wiring-level conflict, not a memory-safety
    /// one, and are no more possible through this than through two `new`s.
    /// Nothing here takes `&mut self`.
    pub fn instance() -> &'static Self {
        static GPIO: Esp32Gpio = Esp32Gpio {
            base: soc_esp32::addr::GPIO_BASE,
        };
        &GPIO
    }

    /// Set pin direction.
    pub fn set_mode(&self, pin: u8, mode: PinMode) -> BusResult<()> {
        let (hi, bit) = pin_bit(pin).ok_or(BusError::InvalidConfig)?;
        let (set_off, clr_off) = if hi {
            (GPIO_ENABLE1_W1TS, GPIO_ENABLE1_W1TC)
        } else {
            (GPIO_ENABLE_W1TS, GPIO_ENABLE_W1TC)
        };
        match mode {
            PinMode::Output => unsafe {
                // Route the pad to the GPIO function before enabling output. A
                // pad whose reset IO_MUX function is a peripheral -- GPIO12-15
                // are the JTAG pins (GPIO15 = MTDO), GPIO6-11 the flash -- never
                // reflects the GPIO output register until this is set, so the
                // output silently goes nowhere and the pin looks dead. esp-idf
                // writes the same function unconditionally in
                // `gpio_pad_select_gpio`. Input stays enabled so a driven pad can
                // still be read back (the PWM example does this).
                soc_esp32::io_mux::configure(
                    pin,
                    soc_esp32::io_mux::gpio_function(pin),
                    true,
                    hal::pinmux::PinPull::None,
                )?;
                reg::at(self.base, set_off).write_volatile(bit);
            },
            PinMode::Input => unsafe {
                soc_esp32::io_mux::configure(
                    pin,
                    soc_esp32::io_mux::gpio_function(pin),
                    true,
                    hal::pinmux::PinPull::None,
                )?;
                reg::at(self.base, clr_off).write_volatile(bit);
            },
            // Pull-up/-down and open-drain need the IO_MUX / GPIO_PIN pad
            // registers this driver does not program yet. Refuse them rather
            // than silently configuring a plain input/output the caller did not
            // ask for -- a floating pin that reads as "handled" is the worse
            // failure.
            PinMode::InputPullUp | PinMode::InputPullDown | PinMode::OutputOpenDrain => {
                return Err(BusError::InvalidConfig);
            }
        }
        Ok(())
    }

    /// Set pin high or low.
    pub fn write(&self, pin: u8, level: PinLevel) -> BusResult<()> {
        let (hi, bit) = pin_bit(pin).ok_or(BusError::InvalidConfig)?;
        let (set_off, clr_off) = if hi {
            (GPIO_OUT1_W1TS, GPIO_OUT1_W1TC)
        } else {
            (GPIO_OUT_W1TS, GPIO_OUT_W1TC)
        };
        match level {
            PinLevel::High => unsafe { reg::at(self.base, set_off).write_volatile(bit) },
            PinLevel::Low => unsafe { reg::at(self.base, clr_off).write_volatile(bit) },
        }
        Ok(())
    }

    /// Read pin level.
    pub fn read(&self, pin: u8) -> BusResult<PinLevel> {
        let (hi, bit) = pin_bit(pin).ok_or(BusError::InvalidConfig)?;
        let off = if hi { GPIO_IN1 } else { GPIO_IN };
        let val = unsafe { reg::at(self.base, off).read_volatile() };
        Ok(if val & bit != 0 {
            PinLevel::High
        } else {
            PinLevel::Low
        })
    }

    /// Clear interrupt status for a pin.
    pub fn clear_interrupt(&self, pin: u8) -> BusResult<()> {
        let (hi, bit) = pin_bit(pin).ok_or(BusError::InvalidConfig)?;
        let off = if hi { GPIO_STATUS1_W1TC } else { GPIO_STATUS_W1TC };
        unsafe { reg::at(self.base, off).write_volatile(bit) };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_is_the_gpio_block_and_is_one_object() {
        let a = Esp32Gpio::instance();
        let b = Esp32Gpio::instance();
        assert_eq!(a.base, soc_esp32::addr::GPIO_BASE);
        assert!(core::ptr::eq(a, b));
    }

    #[test]
    fn pin_bit_selects_base_registers_for_pins_0_to_31() {
        assert_eq!(pin_bit(0), Some((false, 1)));
        assert_eq!(pin_bit(5), Some((false, 1 << 5)));
        assert_eq!(pin_bit(31), Some((false, 1 << 31)));
    }

    #[test]
    fn pin_bit_selects_1_registers_for_pins_32_to_39() {
        assert_eq!(pin_bit(32), Some((true, 1)));
        assert_eq!(pin_bit(33), Some((true, 1 << 1)));
        assert_eq!(pin_bit(39), Some((true, 1 << 7)));
    }

    #[test]
    fn pin_bit_rejects_pins_beyond_39() {
        assert_eq!(pin_bit(40), None);
        assert_eq!(pin_bit(255), None);
    }

    #[test]
    fn register_offsets_match_trm_gpio_summary() {
        // Regression guard for the whole-register-block shift: the previous
        // revision had ENABLE=0x10 (really OUT1), IN=0x1C (really
        // SDIO_SELECT), STATUS=0x24 (really ENABLE_W1TS), and
        // STATUS_W1TC=0x28 (really ENABLE_W1TC).
        assert_eq!(GPIO_OUT, 0x04);
        assert_eq!(GPIO_OUT_W1TS, 0x08);
        assert_eq!(GPIO_OUT_W1TC, 0x0C);
        assert_eq!(GPIO_OUT1, 0x10);
        assert_eq!(GPIO_OUT1_W1TS, 0x14);
        assert_eq!(GPIO_OUT1_W1TC, 0x18);
        assert_eq!(GPIO_ENABLE, 0x20);
        assert_eq!(GPIO_ENABLE_W1TS, 0x24);
        assert_eq!(GPIO_ENABLE_W1TC, 0x28);
        assert_eq!(GPIO_ENABLE1, 0x2C);
        assert_eq!(GPIO_ENABLE1_W1TS, 0x30);
        assert_eq!(GPIO_ENABLE1_W1TC, 0x34);
        assert_eq!(GPIO_IN, 0x3C);
        assert_eq!(GPIO_IN1, 0x40);
        assert_eq!(GPIO_STATUS_W1TC, 0x4C);
        assert_eq!(GPIO_STATUS1_W1TC, 0x58);
    }

    #[test]
    fn enable_and_status_no_longer_alias() {
        // The core of the reported bug: ENABLE-family and STATUS-family
        // offsets must be disjoint, or "clear interrupt" and "disable
        // output" collide.
        assert_ne!(GPIO_ENABLE_W1TC, GPIO_STATUS_W1TC);
        assert_ne!(GPIO_ENABLE, GPIO_IN);
    }
}
