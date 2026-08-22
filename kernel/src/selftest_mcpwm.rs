// SPDX-License-Identifier: Apache-2.0

//! MCPWM self-test. Included by [`crate::selftest`].
//!
//! Proves the three things #30 is about, with no motor and no wire — the chip
//! is its own instrument:
//!
//! - **A complementary pair runs.** PWM0A and PWM0B are routed to two pads and
//!   tapped straight back into two PCNT units. Both must count edges at the
//!   configured frequency: if B were dead, or ran at a different rate, it is not
//!   a complementary pair.
//! - **Dead time is inserted and matches the configured value.** The MCPWM
//!   capture unit timestamps PWM0A's own edges, so its high time can be
//!   measured to the 160 MHz capture clock. The rising-edge delay shortens that
//!   high time by exactly the dead time programmed, which is what capture reads
//!   back — the same kind of on-chip measurement esp-idf's capture examples do.
//! - **A fault shuts the outputs down in hardware.** A pad driven high in
//!   software feeds the fault input; the one-shot trip must latch and force the
//!   outputs low with no code in the loop, PCNT then counting nothing. Clearing
//!   the latch brings the outputs back.
//!
//! Needs three free pads the board declares (`board::active::MCPWM_SELFTEST_GPIOS`
//! as `[pwm_a, pwm_b, fault]`); a board that declares `None` skips this.

use super::Check;

/// A software-driven complementary pair must run at frequency, insert the
/// configured dead time, and shut down on a hardware fault.
#[cfg(target_os = "none")]
pub(crate) fn mcpwm_complementary_pair_deadtime_and_fault(pins: [u8; 3]) -> Check {
    use esp32_gpio::{Esp32Gpio, PinLevel, PinMode};
    use esp32_mcpwm::{Config, Mcpwm};
    use esp32_pcnt::{ChannelMode, Filter, PcntUnit};
    use soc_esp32::addr::GPIO_BASE;
    use soc_esp32::gpio_matrix;

    let [pwm_a, pwm_b, fault] = pins;

    // 10 kHz, 50% pre-dead-time duty, 10 us dead time on each edge.
    // 160 MHz / (0+1) / (159+1) / (99+1) = 10 kHz; compare 50 of 100 = 50%.
    // dead time 1600 cycles / 160 MHz = 10 us.
    const CLK_PRE: u16 = 0;
    const TIM_PRE: u16 = 159;
    const PERIOD: u16 = 99;
    const COMPARE: u16 = 50;
    const DEAD: u16 = 1600;
    const FREQ: u32 = 10_000;

    let cfg = Config {
        clk_prescale: CLK_PRE,
        timer_prescale: TIM_PRE,
        period: PERIOD,
        compare_a: COMPARE,
        dead_red: DEAD,
        dead_fed: DEAD,
    };

    let pwm = unsafe { Mcpwm::new(&cfg) };
    unsafe { pwm.route_output_a(pwm_a) }.map_err(|_| "PWM0A output would not route")?;
    unsafe { pwm.route_output_b(pwm_b) }.map_err(|_| "PWM0B output would not route")?;

    // A PCNT unit on each output, counting rising edges upward.
    let pcnt_a = unsafe { PcntUnit::new(0, ChannelMode::UP_DOWN_ON_RISING, Filter::Off) }
        .map_err(|_| "PCNT unit for A would not configure")?;
    let pcnt_b = unsafe { PcntUnit::new(1, ChannelMode::UP_DOWN_ON_RISING, Filter::Off) }
        .map_err(|_| "PCNT unit for B would not configure")?;
    unsafe { pcnt_a.route_signal(pwm_a) }.map_err(|_| "PCNT A signal would not route")?;
    unsafe { pcnt_b.route_signal(pwm_b) }.map_err(|_| "PCNT B signal would not route")?;
    unsafe { pcnt_a.route_control_level(true) }.map_err(|_| "PCNT A control would not tie high")?;
    unsafe { pcnt_b.route_control_level(true) }.map_err(|_| "PCNT B control would not tie high")?;

    unsafe { pwm.start() };

    // ── 1. Complementary pair runs at frequency ──────────────────────────────
    const WINDOW_MS: u32 = 50;
    let expected = (FREQ * WINDOW_MS / 1000) as i32; // 500 edges
    let tol = expected / 4;
    pcnt_a.clear();
    pcnt_b.clear();
    pcnt_a.resume();
    pcnt_b.resume();
    super::spin_ticks(WINDOW_MS as u64);
    pcnt_a.pause();
    pcnt_b.pause();
    let ca = pcnt_a.count() as i32;
    let cb = pcnt_b.count() as i32;
    if (ca - expected).abs() > tol {
        return Err("PWM0A edge count is not near the configured frequency");
    }
    if (cb - expected).abs() > tol {
        return Err("PWM0B did not pulse with A -- not a running complementary pair");
    }
    if (ca - cb).abs() > tol {
        return Err("PWM0A and PWM0B run at different rates");
    }

    // ── 2. Dead time shortens A's high pulse by the configured amount ─────────
    // Capture A's own edges and measure its high time. The PWM timer counts at
    // 160 MHz / (CLK_PRE+1) / (TIM_PRE+1), but the MCPWM capture timer is
    // clocked from the 80 MHz APB — a different rate, confirmed on hardware — so
    // both the expected high time and the configured dead time are converted
    // into capture ticks before comparing. Dead-time cycles are at the PWM clock
    // (160 MHz / (CLK_PRE+1)); the rising-edge delay shortens A's high pulse by
    // exactly that many, which is what capture reads back.
    const CAP_HZ: u64 = 80_000_000;
    let pwm_hz = esp32_mcpwm::PWM_CLK_HZ as u64;
    unsafe { pwm.route_capture_input(pwm_a) }.map_err(|_| "capture input would not route")?;
    unsafe { pwm.enable_capture_both_edges() };
    let a_high = measure_high_pulse(&pwm)? as u64;

    // Raw (no-dead-time) high = COMPARE timer ticks, expressed in capture ticks.
    let cap_per_timer = (TIM_PRE as u64 + 1) * (CLK_PRE as u64 + 1) * CAP_HZ / pwm_hz;
    let expected_high = COMPARE as u64 * cap_per_timer;
    // Configured dead time in capture ticks.
    let dead_cap = DEAD as u64 * (CLK_PRE as u64 + 1) * CAP_HZ / pwm_hz;
    if a_high >= expected_high {
        return Err("dead time did not shorten A's high pulse");
    }
    let dead_measured = expected_high - a_high;
    if dead_measured.abs_diff(dead_cap) > dead_cap / 3 {
        return Err("measured dead time does not match the configured value");
    }

    // ── 3. A hardware fault shuts the outputs down ───────────────────────────
    let gpio = unsafe { Esp32Gpio::new(GPIO_BASE) };
    gpio.set_mode(fault, PinMode::Output).map_err(|_| "fault pad would not become an output")?;
    gpio.write(fault, PinLevel::Low).map_err(|_| "fault pad would not drive low")?;
    unsafe {
        gpio_matrix::connect_output(fault, gpio_matrix::SIG_GPIO_OUT, true, false)
            .map_err(|_| "fault pad would not route to GPIO_OUT")?;
    }
    unsafe { pwm.route_fault_input(fault) }.map_err(|_| "fault input would not route")?;
    unsafe { pwm.enable_fault_oneshot(true) };

    // Outputs are live before the fault.
    pcnt_a.clear();
    pcnt_a.resume();
    super::spin_ticks(5);
    pcnt_a.pause();
    if pcnt_a.count() == 0 {
        return Err("PWM0A produced no edges before the fault; nothing to shut down");
    }

    // Assert the fault: the trip must latch and the outputs stop.
    gpio.write(fault, PinLevel::High).map_err(|_| "fault pad would not drive high")?;
    super::spin_ticks(1);
    if !pwm.fault_tripped() {
        return Err("asserting the fault input did not latch the trip");
    }
    pcnt_a.clear();
    pcnt_a.resume();
    super::spin_ticks(5);
    pcnt_a.pause();
    if pcnt_a.count() != 0 {
        return Err("PWM0A kept toggling after the fault -- outputs were not shut down");
    }

    // Clear the latch with the source removed: outputs come back.
    gpio.write(fault, PinLevel::Low).map_err(|_| "fault pad would not release")?;
    unsafe { pwm.clear_fault() };
    if pwm.fault_tripped() {
        return Err("the fault latch would not clear");
    }
    pcnt_a.clear();
    pcnt_a.resume();
    super::spin_ticks(5);
    pcnt_a.pause();
    let resumed = pcnt_a.count();
    unsafe { pwm.stop() };
    if resumed == 0 {
        return Err("PWM0A did not resume after the fault cleared");
    }

    Ok(())
}

/// Measure PWM0A's high time in capture-timer ticks: the interval from a rising
/// edge to the next falling edge, read from the MCPWM capture unit.
#[cfg(target_os = "none")]
fn measure_high_pulse(pwm: &esp32_mcpwm::Mcpwm) -> Result<u32, &'static str> {
    let mut rise: Option<u32> = None;
    // Generous: a 10 kHz pulse is ~100 us; this bounds a dead controller.
    let mut spins = 4_000_000u32;
    loop {
        if let Some(c) = unsafe { pwm.poll_capture() } {
            if !c.falling {
                rise = Some(c.timestamp);
            } else if let Some(r) = rise {
                return Ok(c.timestamp.wrapping_sub(r));
            }
        }
        spins -= 1;
        if spins == 0 {
            return Err("capture never timestamped a full A high pulse");
        }
        core::hint::spin_loop();
    }
}

/// Host stand-in: there is no MCPWM to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn mcpwm_complementary_pair_deadtime_and_fault(_pins: [u8; 3]) -> Check {
    Ok(())
}
