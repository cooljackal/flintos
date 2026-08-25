// SPDX-License-Identifier: Apache-2.0

//! M5Stack Core2 ILI9342C bring-up: DMA-streamed fills and blits, plus a
//! throughput benchmark.
//!
//! ```text
//!   Layer 3   ili9342c    the panel protocol + draw primitives (portable)
//!   hal       DisplayInterface   the command/pixel seam
//!   board     Esp32DisplayInterface   SPI DMA + D/C/CS GPIO (built by the board)
//! ```
//!
//! The board already powered the panel (LDO2) and its backlight (DCDC3) at boot
//! via `power_init`, so the screen is lit; this app initialises the controller
//! and draws.
//!
//! # Why this is a probe, not an example
//!
//! The pixel path is DMA, and DMA completion is an interrupt. Its top-half
//! acknowledges the SPI controller and wakes the transfer through the kernel's
//! DMA broker — naming both a physical driver and the kernel, which only an
//! apps/tests probe may do. Everything else (the panel driver, the transport)
//! is layer-clean; this one interrupt line is the reason the bring-up lives here.

#![no_std]
#![no_main]

use api::task;
use api::time::now_us;
use hal::types::Priority;
use soc_esp32::addr;

use ili9342c::color::{BLACK, BLUE, CYAN, GREEN, MAGENTA, ORANGE, RED, WHITE, YELLOW};
use ili9342c::{color, Config, Ili9342c};
use kernel::board::active as manifest;

const _: () = assert!(
    manifest::BOARD.display.is_some(),
    "`lcd` drives an SPI display, and only the M5Stack Core2 declares one.\n\n\
     \tmake flash APP=lcd BOARD=board-m5-core2\n"
);

kernel::flint_app!(main, abi = 2);

/// DMA-completion top-half for the LCD's SPI controller. Acknowledge the
/// peripheral (level-triggered — returning without clearing re-enters forever)
/// and hand the finished transfer's id to the broker so the drawing task wakes.
fn lcd_dma_isr() {
    if let Ok(spi) = board::lcd_spi() {
        spi.ack_interrupts();
        if let Some(id) = spi.take_pending_dma() {
            kernel::dma_broker::signal_complete(id);
        }
    }
}

fn main() {
    if task::spawn("lcd", run, Priority::Normal(2), 12288).is_none() {
        api::log_error!("could not spawn the lcd task");
    }
}

fn run() {
    task::sleep_ms(150);

    // Route the SPI3 DMA interrupt to our top-half before any pixel moves, or
    // the first transfer would time out waiting for a completion nobody signals.
    if let Err(e) = unsafe { api::interrupt::connect(addr::IRQ_SPI3, lcd_dma_isr) } {
        api::log_error!("could not connect the SPI3 interrupt: {:?}", e);
        return;
    }

    let iface = match board::display_interface() {
        Ok(i) => i,
        Err(e) => {
            api::log_error!("no display interface: {}", e);
            return;
        }
    };
    let mut lcd = Ili9342c::new(iface, Config::CORE2);

    // The panel's reset line is AXP192 GPIO4. Configure it as an output and pass
    // a real hardware reset into init — a freshly-powered ILI9342C needs it, not
    // just the software reset. The PMIC is on the shared internal I2C bus the
    // board opened at boot.
    let pmic = match board::pmic_bus() {
        Ok(c) => c,
        Err(e) => {
            api::log_error!("no PMIC bus for LCD reset: {}", e);
            return;
        }
    };
    let pmic_dev = pmic.device(board::axp192::ADDR);
    let axp = board::axp192::Axp192::new(&pmic_dev);
    // The M5Core2 wires the LCD's reset line to the AXP192's GPIO4 — a board
    // fact, so the pin lives here, not in the chip-agnostic driver.
    const LCD_RESET_GPIO: u8 = 4;
    if axp.set_gpio_open_drain_output(LCD_RESET_GPIO).is_err() {
        api::log_error!("could not configure the LCD reset GPIO");
        return;
    }
    if let Err(e) = lcd.init(
        |asserted| {
            let _ = axp.set_gpio(LCD_RESET_GPIO, !asserted);
        },
        task::sleep_ms,
    ) {
        api::log_error!("panel init failed: {:?}", e);
        return;
    }
    api::log_info!("ILI9342C up: {}x{}", lcd.width(), lcd.height());

    // 1. A cycle of solid fills — the first proof pixels reach the panel.
    for (name, c) in [
        ("black", BLACK),
        ("red", RED),
        ("green", GREEN),
        ("blue", BLUE),
        ("white", WHITE),
    ] {
        if lcd.fill_screen(c).is_err() {
            api::log_error!("fill {} failed", name);
            return;
        }
        api::log_info!("filled {}", name);
        task::sleep_ms(400);
    }

    // 2. Throughput: time one full-screen DMA fill.
    let px = lcd.width() as u32 * lcd.height() as u32;
    let bytes = px * 2;
    let t0 = now_us();
    let _ = lcd.fill_screen(BLUE);
    let us = now_us().wrapping_sub(t0).max(1) as u32;
    // KB/s = bytes / us * 1000; fps = 1_000_000 / us.
    let kbps = (bytes.wrapping_mul(1000)) / us;
    let fps = 1_000_000u32 / us;
    api::log_info!(
        "full-screen fill: {} px, {} KiB in {} us = {} KB/s (~{} fps)",
        px,
        bytes / 1024,
        us,
        kbps,
        fps
    );

    // 3. A blit: a computed gradient sprite, DMA-streamed as a pixel run.
    // 48x48 keeps the sprite (4.5 KiB) comfortably inside the task stack.
    const S: usize = 48;
    let mut sprite = [0u16; S * S];
    for (y, row) in sprite.chunks_mut(S).enumerate() {
        for (x, p) in row.iter_mut().enumerate() {
            *p = color::rgb565((x * 4) as u8, (y * 4) as u8, 128);
        }
    }
    let _ = lcd.fill_screen(BLACK);
    let _ = lcd.blit(20, 20, S as u16, S as u16, &sprite);
    api::log_info!("blitted a {}x{} gradient", S, S);

    // 4. A grid of rectangles — many windowed fills, exercising the batching.
    let colors = [RED, GREEN, BLUE, YELLOW, CYAN, MAGENTA, ORANGE, WHITE];
    for (i, &c) in colors.iter().enumerate() {
        let x = 20 + (i as u16 % 4) * 70;
        let y = 120 + (i as u16 / 4) * 55;
        let _ = lcd.fill_rect(x, y, 60, 45, c);
    }
    api::log_info!("drew the rect grid — bring-up PASS");

    // 5. A random animation: random-colored blocks at random positions, forever.
    // Clearly alive and clearly *rendering* — a broken panel shows noise or
    // nothing, not crisp colored squares. Seeded from the microsecond clock so
    // each boot differs.
    let _ = lcd.fill_screen(BLACK);
    const BOX: u16 = 30;
    let w = lcd.width() - BOX;
    let h = lcd.height() - BOX;
    let mut rng = (now_us() as u32) | 1;
    api::log_info!("random animation running — watch the screen");
    loop {
        let r = xorshift(&mut rng);
        let x = (r % w as u32) as u16;
        let y = ((r >> 9) % h as u32) as u16;
        let color = (xorshift(&mut rng) & 0xFFFF) as u16;
        let _ = lcd.fill_rect(x, y, BOX, BOX, color);
        task::sleep_ms(40);
    }
}

/// A tiny xorshift32 PRNG — enough for a lively animation, no crate needed.
fn xorshift(state: &mut u32) -> u32 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    x
}
