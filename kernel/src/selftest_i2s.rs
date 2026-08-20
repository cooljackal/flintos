// SPDX-License-Identifier: Apache-2.0

//! I2S DMA loopback self-test. Included by [`crate::selftest`].
//!
//! The ESP32's I2S has no CPU FIFO, so this is a real DMA transfer: a known
//! pattern is transmitted, looped back to the receiver internally
//! (`sig_loopback`), and DMA'd into a second buffer. A byte-for-byte match
//! proves the serialiser, deserialiser, both FIFOs and both DMA engines.
//!
//! No pin is involved — the loopback is internal — so this runs on any board.
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
pub(crate) fn i2s_dma_loopback_round_trips() -> Check {
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

    let i2s = unsafe { I2sLoopback::new() };
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
pub(crate) fn i2s_dma_loopback_round_trips() -> Check {
    Ok(())
}
