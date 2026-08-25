// SPDX-License-Identifier: Apache-2.0

//! RP2040 UART transfers driven by the chip's shared DMA engine.

use hal::{DmaError, DmaHandle, DmaTransferId};
use soc_rp2040::{dma, UART1_BASE};

use super::{Rp2040Uart, DMACR, DMACR_RXDMAE, DMACR_TXDMAE, DR, FR, FR_RXFE};

const DMA_AWAIT_TIMEOUT_MS: u32 = 100;

fn broker_begin_pair(
    source: &DmaHandle,
    destination: &DmaHandle,
) -> Result<DmaTransferId, DmaError> {
    extern "Rust" {
        fn _flint_sys_dma_begin_pair(
            source: &DmaHandle,
            destination: &DmaHandle,
        ) -> Result<DmaTransferId, DmaError>;
    }
    unsafe { _flint_sys_dma_begin_pair(source, destination) }
}

fn broker_await(id: DmaTransferId, timeout_ms: u32) -> Result<(), DmaError> {
    extern "Rust" {
        fn _flint_sys_dma_await(id: DmaTransferId, timeout_ms: u32) -> Result<(), DmaError>;
    }
    unsafe { _flint_sys_dma_await(id, timeout_ms) }
}

/// A full-duplex UART DMA exchange that owns both hardware channels until it
/// completes or is cancelled.
#[must_use = "a started DMA transfer should be awaited"]
pub struct Transfer<'a> {
    uart: &'a Rp2040Uart,
    id: DmaTransferId,
    tx: dma::Channel,
    rx: dma::Channel,
}

impl Transfer<'_> {
    /// Block for the receive-channel interrupt, then verify neither channel
    /// reported a bus error. A timeout cancels both channels before returning.
    pub fn await_done(mut self) -> hal::Result<()> {
        broker_await(self.id, DMA_AWAIT_TIMEOUT_MS)?;
        let failed = self.tx.hardware_error().map_err(map_dma_error)?
            || self.rx.hardware_error().map_err(map_dma_error)?;
        self.stop();
        if failed {
            Err(hal::bus::BusError::DmaError.into())
        } else {
            Ok(())
        }
    }

    fn stop(&mut self) {
        unsafe { self.uart.reg(DMACR).write_volatile(0) };
        let _ = self.rx.cancel();
        let _ = self.tx.cancel();
    }

    pub fn id(&self) -> DmaTransferId {
        self.id
    }
}

impl Drop for Transfer<'_> {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Rp2040Uart {
    /// Start a simultaneous transmit and receive using two broker-owned SRAM
    /// buffers. UART1 is the RP2040 acceptance port; UART0 remains the console.
    pub fn exchange_dma<'a>(
        &'a self,
        tx: &DmaHandle,
        rx: &DmaHandle,
        len: usize,
    ) -> hal::Result<Transfer<'a>> {
        let count = u32::try_from(len)
            .ok()
            .filter(|count| *count != 0 && *count <= tx.size() && *count <= rx.size())
            .ok_or(hal::bus::BusError::InvalidConfig)?;
        if self.base != UART1_BASE {
            return Err(hal::bus::BusError::InvalidConfig.into());
        }

        let rx_channel = dma::claim().map_err(map_dma_error)?;
        let tx_channel = dma::claim().map_err(map_dma_error)?;
        rx_channel
            .configure(dma::TransferConfig::peripheral_to_memory(
                self.base + DR,
                rx.addr(),
                count,
                dma::Dreq::UART1_RX,
            ))
            .map_err(map_dma_error)?;
        tx_channel
            .configure(dma::TransferConfig::memory_to_peripheral(
                tx.addr(),
                self.base + DR,
                count,
                dma::Dreq::UART1_TX,
            ))
            .map_err(map_dma_error)?;
        rx_channel.enable_irq0(true).map_err(map_dma_error)?;

        while unsafe { self.reg(FR).read_volatile() } & FR_RXFE == 0 {
            let _ = unsafe { self.reg(DR).read_volatile() };
        }

        let id = broker_begin_pair(tx, rx)?;
        rx_channel
            .publish_completion(id.raw())
            .map_err(map_dma_error)?;

        unsafe { self.reg(DMACR).write_volatile(DMACR_RXDMAE | DMACR_TXDMAE) };
        if dma::start_mask((1 << rx_channel.number()) | (1 << tx_channel.number())).is_err() {
            unsafe { self.reg(DMACR).write_volatile(0) };
            return Err(hal::bus::BusError::DmaError.into());
        }

        Ok(Transfer {
            uart: self,
            id,
            tx: tx_channel,
            rx: rx_channel,
        })
    }

    /// Acknowledge DMA IRQ0 and take the broker id published by the active
    /// UART transfer. Intended for the application's interrupt top-half.
    pub fn take_pending_dma() -> Option<DmaTransferId> {
        dma::take_irq0_completion().map(DmaTransferId::from_raw)
    }
}

fn map_dma_error(_: dma::Error) -> hal::Error {
    hal::bus::BusError::DmaError.into()
}
