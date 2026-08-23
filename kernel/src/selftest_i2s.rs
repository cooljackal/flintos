// SPDX-License-Identifier: Apache-2.0

//! I2S DMA loopback self-test. Included by [`crate::selftest`].
//!
//! The ESP32's I2S has no CPU FIFO, so this is a real DMA transfer: a known
//! pattern is transmitted, looped back to the receiver internally
//! (`sig_loopback`), and DMA'd into a second buffer. A byte-for-byte match
//! proves the serialiser, deserialiser, both FIFOs and both DMA engines.
//!
//! `sig_loopback` shares the clocks internally, but the serial data has to loop
//! over one pad, so this needs a free GPIO — `board::active::LOOPBACK_SCRATCH_GPIO`.
//!
//! Buffers and descriptors are static and word-aligned, which on the ESP32 puts
//! them in DMA-reachable DRAM.

use super::Check;

#[cfg(target_os = "none")]
const WORDS: usize = 32; // 128 bytes = 64 sixteen-bit samples

#[cfg(target_os = "none")]
static mut TX: [u32; WORDS] = [0; WORDS];
#[cfg(target_os = "none")]
static mut RX: [u32; WORDS] = [0; WORDS];
#[cfg(target_os = "none")]
static mut TX_DESC: [soc_esp32::dma::Descriptor; 1] = [soc_esp32::dma::Descriptor::zeroed(); 1];
#[cfg(target_os = "none")]
static mut RX_DESC: [soc_esp32::dma::Descriptor; 1] = [soc_esp32::dma::Descriptor::zeroed(); 1];

/// Transmit a pattern and require it back byte-for-byte through the loopback.
#[cfg(target_os = "none")]
pub(crate) fn i2s_dma_loopback_round_trips(data_pin: u8) -> Check {
    use core::ptr::addr_of_mut;
    use esp32_i2s::I2sLoopback;
    use soc_esp32::dma::Descriptor;

    const BYTES: usize = WORDS * 4;

    // Build slices from the statics without taking references to them.
    let tx = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(TX) as *mut u8, BYTES) };
    let rx = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(RX) as *mut u8, BYTES) };
    let tx_desc = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(TX_DESC) as *mut Descriptor, 1) };
    let rx_desc = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(RX_DESC) as *mut Descriptor, 1) };

    // A recognisable ramp, and a receive buffer that starts wrong so a transfer
    // that moves nothing cannot pass by coincidence.
    for (i, b) in tx.iter_mut().enumerate() {
        *b = (i as u8) ^ 0x5A;
    }
    rx.fill(0);

    let i2s = unsafe { I2sLoopback::new(data_pin) }.map_err(|_| "the I2S data pin would not route")?;
    let got = unsafe { i2s.loopback(tx, rx, tx_desc, rx_desc) }
        .map_err(|_| "the I2S loopback DMA never completed")?;

    {
        use crate::debug::fault::{raw_dec, raw_print};
        raw_print("[FLINT]   i2s received=");
        raw_dec(got as u32);
        raw_print(" of ");
        raw_dec(BYTES as u32);
        raw_print("\r\n");
    }

    if got != BYTES {
        return Err("the I2S loopback received a different number of bytes than were sent");
    }
    if rx != tx {
        return Err("the I2S loopback data did not match what was sent");
    }
    Ok(())
}

// Host stand-in: there is no I2S peripheral or DMA to drive.
#[cfg(not(target_os = "none"))]
pub(crate) fn i2s_dma_loopback_round_trips(_data_pin: u8) -> Check {
    Ok(())
}

// ── Continuous (ring) stream loopback ─────────────────────────────────────────
//
// The one-shot test above proves the data path moves one bounded buffer. This
// one proves the *streaming* contract: a ring of buffers the DMA engine cycles
// forever while the CPU refills each buffer as it frees up, with no gap or repeat
// at any buffer boundary. A discontinuity is exactly what an under/overrun would
// produce, so byte-continuity across many buffers and several ring cycles is the
// underrun-free property stated directly.

#[cfg(target_os = "none")]
const S_COUNT: usize = 4; // buffers in the ring
#[cfg(target_os = "none")]
const S_CHUNK: usize = 64; // bytes per buffer (16 words)
#[cfg(target_os = "none")]
const S_BYTES: usize = S_COUNT * S_CHUNK; // whole ring
#[cfg(target_os = "none")]
const S_CYCLES: usize = 10; // ring cycles to run — well past one lap

#[cfg(target_os = "none")]
static mut STX: [u32; S_BYTES / 4] = [0; S_BYTES / 4];
#[cfg(target_os = "none")]
static mut SRX: [u32; S_BYTES / 4] = [0; S_BYTES / 4];
#[cfg(target_os = "none")]
static mut STX_DESC: [soc_esp32::dma::Descriptor; S_COUNT] =
    [soc_esp32::dma::Descriptor::zeroed(); S_COUNT];
#[cfg(target_os = "none")]
static mut SRX_DESC: [soc_esp32::dma::Descriptor; S_COUNT] =
    [soc_esp32::dma::Descriptor::zeroed(); S_COUNT];

/// Fill `buf` with the segment of a global byte ramp beginning at production
/// index `prod`: byte `j` is `(prod * S_CHUNK + j) mod 256`. Consecutive
/// production indices therefore continue one seamless ramp — the property the
/// receiver checks.
#[cfg(target_os = "none")]
fn fill_ramp(buf: &mut [u8], prod: usize) {
    for (j, b) in buf.iter_mut().enumerate() {
        *b = (prod.wrapping_mul(S_CHUNK).wrapping_add(j)) as u8;
    }
}

/// Run one continuous stream to completion and require the received bytes to
/// form an unbroken ramp, then require the peripheral to come to rest.
#[cfg(target_os = "none")]
fn run_stream(i2s: &esp32_i2s::I2sLoopback) -> Check {
    use core::ptr::addr_of_mut;
    use soc_esp32::dma::Descriptor;

    let tx = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(STX) as *mut u8, S_BYTES) };
    let rx = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(SRX) as *mut u8, S_BYTES) };
    let txd = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(STX_DESC) as *mut Descriptor, S_COUNT) };
    let rxd = unsafe { core::slice::from_raw_parts_mut(addr_of_mut!(SRX_DESC) as *mut Descriptor, S_COUNT) };

    // Prime the ring: buffers 0..S_COUNT carry production indices 0..S_COUNT, so
    // the very first lap already spans several buffers of the ramp.
    for p in 0..S_COUNT {
        fill_ramp(&mut tx[p * S_CHUNK..(p + 1) * S_CHUNK], p);
    }
    rx.fill(0);

    let mut stream = unsafe { i2s.start_stream(tx, rx, txd, rxd, S_COUNT) }
        .map_err(|_| "the I2S stream would not start")?;

    // Service every buffer boundary for several full cycles. On each: check the
    // received buffer continues the ramp, then refill its transmit buffer with
    // the next segment — the refill-while-in-flight the engine needs to not
    // underrun on the next lap.
    // The refill segment for a freed buffer is its production index: buffers
    // 0..S_COUNT were pre-filled, so refills continue from S_COUNT upward, one
    // per serviced boundary.
    let mut prev: Option<u8> = None;
    for prod_next in S_COUNT..(S_COUNT + S_COUNT * S_CYCLES) {
        let idx = unsafe { stream.wait() }
            .map_err(|_| "the I2S stream stalled — a buffer boundary never arrived")?;
        for &b in stream.rx_buffer(idx) {
            if let Some(pv) = prev {
                if b != pv.wrapping_add(1) {
                    return Err("the I2S stream lost byte-continuity at a buffer boundary (underrun)");
                }
            }
            prev = Some(b);
        }
        fill_ramp(stream.tx_buffer_mut(idx), prod_next);
        stream.commit();
    }

    if prev.is_none() {
        return Err("the I2S stream received nothing");
    }

    unsafe { stream.stop() };

    // Quiescent: no start bit set, and the engine raises no further boundary.
    if i2s.is_running() {
        return Err("I2S still running after the stream was stopped");
    }
    i2s.clear_eof();
    crate::selftest::spin_cycles(2_000_000);
    if i2s.eof_pending() {
        return Err("I2S DMA kept cycling buffers after stop — the channel was not released");
    }
    Ok(())
}

/// Stream a ramp through the loopback ring, require unbroken continuity, then
/// stop and start a *second* stream to prove the peripheral restarts clean.
#[cfg(target_os = "none")]
pub(crate) fn i2s_continuous_stream_stays_continuous(data_pin: u8) -> Check {
    use esp32_i2s::I2sLoopback;

    let i2s = unsafe { I2sLoopback::new(data_pin) }.map_err(|_| "the I2S data pin would not route")?;

    // First stream: continuity across many buffers and cycles, then a clean stop.
    run_stream(&i2s)?;
    // Second stream on the same peripheral: proves stop left it restartable.
    run_stream(&i2s)?;
    Ok(())
}

// Host stand-in: no peripheral, no DMA. The ring math it rests on is unit-tested
// in `soc_esp32::dma`.
#[cfg(not(target_os = "none"))]
pub(crate) fn i2s_continuous_stream_stays_continuous(_data_pin: u8) -> Check {
    Ok(())
}
