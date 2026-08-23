// SPDX-License-Identifier: Apache-2.0

#![no_std]

use core::ptr::addr_of_mut;
use core::sync::atomic::{AtomicBool, Ordering};

use hal::bus::{BusConfig, BusError, BusResult, BusSpeed, PhysicalBus, PhysicalTransfer, SpiMode};
use hal::dma::DmaReach as _;
use hal::pinmux::{PinConfig, PinMux, Signal};
use soc_esp32::addr;
use soc_esp32::ctrl::{SpiCtrl, SpiPort};
use soc_esp32::dma::{self, build_chain, descriptors_needed, Channel, Descriptor, Direction, Host};
use soc_esp32::reg;
use soc_esp32::{dport, Esp32PinMux, APB_HZ};

/// One claim flag per general-purpose SPI controller (SPI2, SPI3). `open`
/// wins exactly one of these per boot, which is what discharges the
/// "not concurrently owned" invariant that `new`'s `# Safety` rests on — the
/// `svd2rust` `Peripherals::take` pattern, with `core::sync::atomic` rather
/// than `portable_atomic` because a physical driver may not name a crates.io
/// crate (see `tools/check-layers.sh`) and the Xtensa core has native atomics.
static SPI_CLAIMED: [AtomicBool; 2] = [AtomicBool::new(false), AtomicBool::new(false)];

/// Index into [`SPI_CLAIMED`] for a controller.
const fn claim_index(ctrl: SpiCtrl) -> usize {
    match ctrl {
        SpiCtrl::Spi2 => 0,
        SpiCtrl::Spi3 => 1,
    }
}

/// ESP32 SPI2 (HSPI) / SPI3 (VSPI) physical driver (polled mode).
///
/// Bases: SPI2/HSPI 0x3FF64000, SPI3/VSPI 0x3FF65000.
// DMA transfers. Own file: the FIFO path above is complete in itself and the
// engine is a separate contract with separate registers, so interleaving them
// would make both harder to check against the header.
#[path = "dma.rs"]
mod dma_impl;

// SPI slave mode. Own file for the same reason as the DMA path: the slave's
// register sequence and completion bit are a separate contract from the
// master's, and interleaving them would make either harder to check against the
// esp-idf reference it follows.
#[path = "slave.rs"]
mod slave_impl;

pub use dma_impl::{SPI_IN_SUC_EOF, SPI_OUT_EOF};
pub use slave_impl::Esp32SpiSlave;

pub struct Esp32Spi {
    base: u32,
    /// CPHA for the configured mode: the clock-out edge (`SPI_USER.ck_out_edge`,
    /// bit 7) that every per-transfer USER write must carry.
    cpha: bool,
}

// ── DMA under the Bus ────────────────────────────────────────────────────────
//
// A transfer past the FIFO cap runs over a DMA descriptor chain instead,
// decided in `exchange` and invisible to the caller. The channel, the
// descriptor scratch, and the enable flag cannot live in `Esp32Spi` — it is a
// lightweight handle, reconstructed freely (see the ISR in `apps/tests/spidma`) — so
// they live here, one slot per general-purpose host (SPI2, SPI3), claimed once
// at `init` and held for the driver's life. The descriptor scratch is a static,
// which puts it in internal DRAM and therefore in DMA-reachable memory.

/// Largest DMA transfer the descriptor scratch covers: [`MAX_DESCS`]
/// descriptors of up to 4092 bytes each (~64 KiB). A bulk op past this is
/// rejected rather than silently truncated.
const MAX_DESCS: usize = 16;

struct DmaSlot {
    channel: Option<Channel>,
    enabled: bool,
    /// Which descriptor bank the next transfer uses. Consecutive transfers
    /// alternate banks so each re-arm hands the engine a *fresh* descriptor
    /// address: the ESP32 DMA caches the descriptor at a given link address and
    /// a tight polled loop re-arming the same address back-to-back does not see
    /// it re-fetched, so the second transfer clocks out zeros. esp-idf reuses
    /// one bank but its ISR-driven transfers leave a gap that hides this.
    next_bank: usize,
    tx_descs: [[Descriptor; MAX_DESCS]; 2],
    rx_descs: [[Descriptor; MAX_DESCS]; 2],
}

impl DmaSlot {
    const fn new() -> Self {
        Self {
            channel: None,
            enabled: true,
            next_bank: 0,
            tx_descs: [[Descriptor::zeroed(); MAX_DESCS]; 2],
            rx_descs: [[Descriptor::zeroed(); MAX_DESCS]; 2],
        }
    }
}

/// Slot 0 is SPI2, slot 1 is SPI3. SPI1 drives the boot flash and gets none.
static mut SPI_DMA: [DmaSlot; 2] = [DmaSlot::new(), DmaSlot::new()];

/// The DMA slot for a general-purpose SPI instance, if it has one.
fn dma_slot(instance: u8) -> Option<usize> {
    match instance {
        2 => Some(0),
        3 => Some(1),
        _ => None,
    }
}

// ── Register map ─────────────────────────────────────────────────────────────
//
// ESP32 TRM chapter 7 (SPI Controller), register summary; offsets confirmed
// against esp-idf `soc/spi_reg.h`. A prior revision had CLOCK/USER/USER1/
// PIN/SLAVE shifted down by one register slot (CLOCK=0x0C, USER=0x10,
// USER1=0x14, PIN=0x18, SLAVE=0x1C), which is really CTRL1/RD_STATUS/CTRL2/
// CLOCK/USER -- every one of those writes landed on the wrong register.

pub(crate) const SPI_CMD: u32 = 0x00;
#[allow(dead_code)] // Not needed for the byte-oriented polled transfer this driver implements.
const SPI_ADDR: u32 = 0x04;
#[allow(dead_code)]
const SPI_CTRL: u32 = 0x08;
#[allow(dead_code)] // Not needed for the byte-oriented polled transfer this driver implements.
const SPI_CTRL1: u32 = 0x0C;
#[allow(dead_code)]
const SPI_RD_STATUS: u32 = 0x10;
#[allow(dead_code)]
const SPI_CTRL2: u32 = 0x14;
const SPI_CLOCK: u32 = 0x18;
pub(crate) const SPI_USER: u32 = 0x1C;
#[allow(dead_code)] // Superseded by MOSI_DLEN/MISO_DLEN for byte-length transfers.
const SPI_USER1: u32 = 0x20;
#[allow(dead_code)]
const SPI_USER2: u32 = 0x24;
pub(crate) const SPI_MOSI_DLEN: u32 = 0x28;
pub(crate) const SPI_MISO_DLEN: u32 = 0x2C;
const SPI_PIN: u32 = 0x34;
pub(crate) const SPI_SLAVE: u32 = 0x38;
pub(crate) const SPI_W0: u32 = 0x80; // Data buffer: 16 words (W0..W15), 64 bytes.

/// `SPI_SYNC_RESET`, `SPI_SLAVE` bit 31: reset the SPI core transfer FSM.
/// Pulsed between transactions so a slave left mid-shift starts each one clean.
pub(crate) const SPI_SYNC_RESET: u32 = 1 << 31;
/// `SPI_TRANS_DONE`, `SPI_SLAVE` bit 4: the transaction finished. It is
/// write-zero-to-clear and enabled at reset (the `SPI_INT_EN` default), so it
/// must be acknowledged or the completion interrupt re-enters forever.
pub(crate) const SPI_TRANS_DONE: u32 = 1 << 4;
/// `SPI_CK_I_EDGE`, `SPI_USER` bit 6: the slave's clock-input edge, the mirror
/// of the master's `ck_out_edge` (bit 7).
pub(crate) const SPI_CK_I_EDGE: u32 = 1 << 6;

/// SPI_CMD_REG: start a user-defined transaction. bitpos [18], confirmed
/// against esp-idf `soc/spi_reg.h` (`SPI_USR`). A prior revision wrote/polled
/// bit 0, which is `SPI_DOUTDIN` (a mode bit, not the start-transaction
/// strobe) -- the poll loop could spin forever since nothing ever clears it.
pub(crate) const SPI_CMD_USR: u32 = 1 << 18;

/// SPI_USER_REG bits (bitpos confirmed against esp-idf `soc/spi_reg.h`).
pub(crate) const SPI_USR_MISO: u32 = 1 << 28;
pub(crate) const SPI_USR_MOSI: u32 = 1 << 27;

/// `SPI_CK_OUT_EDGE`, bit 7: the clock output edge that encodes CPHA in master
/// mode (paired with `SPI_PIN.ck_idle_edge` for CPOL).
pub(crate) const SPI_CK_OUT_EDGE: u32 = 1 << 7;

/// `SPI_DOUTDIN`, bitpos [0]: "Set the bit to enable full duplex
/// communication." Without it the MOSI and MISO phases run one after the
/// other, so a full-duplex `fifo_exchange` sends all its bytes and *then* clocks in
/// the reply -- reading a line nothing is driving.
///
/// The signature of `fifo_exchange(tx, rx)` promises simultaneous exchange, which
/// is what every SPI device expects. This bit is what makes that true.
pub(crate) const SPI_DOUTDIN: u32 = 1 << 0;
/// Assert CS during the prepare phase, using CTRL2.setup_time.
const SPI_CS_SETUP: u32 = 1 << 5;
/// Keep CS asserted during the done phase, using CTRL2.hold_time.
const SPI_CS_HOLD: u32 = 1 << 4;

/// Data buffer capacity: 16 32-bit words.
const SPI_DATA_BUF_WORDS: usize = 16;
const SPI_MAX_BYTES: usize = SPI_DATA_BUF_WORDS * 4;

/// Bound on `SPI_CMD_USR` poll iterations before giving up. A polled byte
/// transfer at the slowest supported clock completes in well under a
/// millisecond; this bound is generous enough to absorb scheduling jitter
/// while still failing a genuinely wedged peripheral instead of hanging
/// forever.
pub(crate) const SPI_TIMEOUT_SPINS: u32 = 1_000_000;

// ── Pin routing ──────────────────────────────────────────────────────────────
//
// Bases, DPORT clock bits, native pads and the IO_MUX offset table all live in
// `soc-esp32`. This driver used to carry its own copy of that table with
// a comment saying to keep it in sync with the one in `esp32-uart` by hand --
// which is exactly the arrangement the SoC layer exists to end.

/// Route MOSI, MISO and SCK for controller `instance`.
///
/// Any pads will do; `PinMux` takes the IO_MUX direct path when the requested
/// pad is native to the signal and the GPIO matrix otherwise. Before the SoC
/// layer existed this driver accepted only the native triple.
///
/// Off-native routing costs a couple of cycles of latency, which matters at
/// this bus's top speeds -- so it is reported, not silently accepted, once
/// there is somewhere to report it to.
fn route_pins(instance: u8, mosi: u8, miso: u8, sck: u8) -> BusResult<()> {
    if mosi == miso || mosi == sck || miso == sck {
        return Err(BusError::InvalidConfig);
    }
    let mux = Esp32PinMux::new();
    let sigs = [
        (Signal::SpiMosi(instance), mosi),
        (Signal::SpiMiso(instance), miso),
        (Signal::SpiSck(instance), sck),
    ];
    for (sig, pin) in sigs {
        mux.can_route(sig, pin)?;
    }
    for (sig, pin) in sigs {
        mux.route(sig, pin, PinConfig::PUSH_PULL)?;
    }
    Ok(())
}

/// Pack up to 4 bytes (little-endian: `bytes[0]` is the first byte
/// transmitted) into one `SPI_Wn` word. The ESP32 SPI data buffer packs 4
/// bytes per 32-bit word; a prior revision wrote/read one byte per word
/// (`word_addr` advancing 4 bytes per *byte* index), which both wasted 3 of
/// every 4 buffer words and misaligned every byte after the first.
fn pack_word(bytes: &[u8]) -> u32 {
    let mut word = 0u32;
    for (i, &b) in bytes.iter().take(4).enumerate() {
        word |= (b as u32) << (i * 8);
    }
    word
}

/// Inverse of `pack_word`: unpack up to 4 bytes from a `SPI_Wn` word into
/// `out`, honouring the default little-endian `SPI_WR_BYTE_ORDER`/
/// `SPI_RD_BYTE_ORDER` = 0 reset state (first byte transferred = LSB).
fn unpack_word(word: u32, out: &mut [u8]) {
    for (i, b) in out.iter_mut().take(4).enumerate() {
        *b = ((word >> (i * 8)) & 0xFF) as u8;
    }
}

/// Compute the `SPI_CLOCK` register value for a target bus clock, a port of
/// esp-idf's `spi_ll_master_cal_clock` (`hal/esp32/include/hal/spi_ll.h`) at a
/// fixed 50% duty cycle.
///
/// The register packs a two-stage divider: `clkdiv_pre` [30:18] (pre−1) then a
/// counter `clkcnt_n` [17:12] (n−1) with high/low phase boundaries `clkcnt_h`
/// [11:6] and `clkcnt_l` [5:0]. Effective clock is `fapb / (pre · n)`. `n` alone
/// is only six bits, so it maxes at 64: for any divider above that the prescaler
/// **must** engage. The previous code never set `clkdiv_pre`, so a 1 MHz request
/// off an 80 MHz APB needed n = 80, which overflowed its field to 15 while the
/// high-phase boundary stayed 39 — h > n, an inconsistent register the hardware
/// never completes, so `SPI_CMD.usr` never self-cleared and `fifo_exchange()` hung
/// (issue #91). Brute-forcing n and deriving the best pre, as esp-idf does,
/// reaches low frequencies with a consistent register.
///
/// For a target above ¾·fapb the divider cannot help, so `clk_equ_sysclk`
/// (bit 31) runs the bus straight off the APB.
fn spi_clock_reg(fapb: u32, hz: u32) -> u32 {
    /// 50% duty in esp-idf's 0..256 scale; h = round(duty·n / 256).
    const DUTY: u32 = 128;
    let hz = hz.max(1);

    // Above three-quarters of the APB, drive the bus from the APB directly.
    if hz > (fapb / 4) * 3 {
        return 1 << 31; // clk_equ_sysclk
    }

    // Bruteforce n (2..=64), pick the best pre for each, keep the lowest error;
    // on a tie prefer the higher n for finer duty-cycle resolution (the `<=`).
    let mut best_n = 2u32;
    let mut best_pre = 1u32;
    let mut best_err = u32::MAX;
    for n in 2..=64u32 {
        let mut pre = ((fapb / n) + hz / 2) / hz; // round((fapb/n)/hz)
        pre = pre.clamp(1, 8192);
        let err = (fapb / (pre * n)).abs_diff(hz);
        if err <= best_err {
            best_err = err;
            best_n = n;
            best_pre = pre;
        }
    }

    let n = best_n;
    let pre = best_pre;
    let l = n;
    let h = ((DUTY * n + 127) / 256).max(1);

    (((pre - 1) & 0x1FFF) << 18)
        | (((n - 1) & 0x3F) << 12)
        | (((h - 1) & 0x3F) << 6)
        | ((l - 1) & 0x3F)
}

impl Esp32Spi {
    /// Bind a driver instance to the SPI register block at `base_addr`.
    ///
    /// # Safety
    /// `base_addr` must be the base address of a real ESP32 SPI2 or SPI3
    /// register block (0x3FF64000 / 0x3FF65000) and must not be concurrently
    /// owned by another driver instance -- this type performs unchecked
    /// `read_volatile`/`write_volatile` at `base_addr + offset` with no
    /// further validation of the address itself.
    pub unsafe fn new(base_addr: u32) -> Self {
        Self { base: base_addr, cpha: false }
    }

    /// Claim a SPI controller once and bring it up.
    ///
    /// This is the safe constructor. It wins the controller's claim flag (a
    /// second `open` of the same controller returns [`BusError::Busy`]), then
    /// does exactly what [`PhysicalBus::init`] does — clock-gate, pad-route and
    /// configure — from the [`SpiPort`]'s controller and config. Because the
    /// claim proves single ownership, no `unsafe` is needed at the call site;
    /// [`Esp32Spi::new`] stays for the kernel self-tests, which step through
    /// bring-up deliberately.
    pub fn open(port: &SpiPort) -> hal::Result<Self> {
        let idx = claim_index(port.ctrl);
        SPI_CLAIMED[idx]
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Acquire)
            .map_err(|_| BusError::Busy)?;

        // SAFETY: the claim above is exclusive, so this is the only live driver
        // on this controller's base address.
        let mut spi = unsafe { Self::new(port.ctrl.base()) };
        if let Err(e) = spi.init(&BusConfig::Spi(port.cfg)) {
            // Give the claim back so a corrected config can be tried.
            SPI_CLAIMED[idx].store(false, Ordering::Release);
            return Err(e.into());
        }
        Ok(spi)
    }

    pub(crate) fn reg(&self, offset: u32) -> *mut u32 {
        (self.base + offset) as *mut u32
    }

    /// Program SPI_CLOCK for `speed_hz` off the APB clock.
    fn apply_clock(&self, speed_hz: u32) {
        unsafe {
            self.reg(SPI_CLOCK)
                .write_volatile(spi_clock_reg(APB_HZ, speed_hz));
        }
    }

    /// One polled full-duplex exchange through the 64-byte data FIFO.
    ///
    /// The byte count is the shorter of `tx` and `rx`; past [`SPI_MAX_BYTES`]
    /// it is `Err(InvalidConfig)`, never a silent truncation. Callers that may
    /// exceed the FIFO go through [`PhysicalTransfer::exchange`], which chunks
    /// or uses DMA. Public so the on-target self-tests can drive the FIFO path
    /// directly.
    pub fn fifo_exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let len = tx.len().min(rx.len());
        if len > SPI_MAX_BYTES {
            return Err(BusError::InvalidConfig);
        }
        if len == 0 {
            return Ok(());
        }
        let nwords = len.div_ceil(4);

        unsafe {
            // Write TX data into the data buffer, 4 bytes per word.
            for w in 0..nwords {
                let start = w * 4;
                let end = (start + 4).min(len);
                let word = pack_word(&tx[start..end]);
                self.reg(SPI_W0 + (w as u32 * 4)).write_volatile(word);
            }

            // Bit-length fields, not byte count minus one: SPI_MOSI_DLEN /
            // SPI_MISO_DLEN hold (bits - 1). A prior revision wrote the bit
            // count into USER1, which holds an address-phase bit-length
            // field, not the data-phase length.
            let bits = (len as u32) * 8 - 1;
            self.reg(SPI_MOSI_DLEN).write_volatile(bits);
            self.reg(SPI_MISO_DLEN).write_volatile(bits);

            // Configure the transfer: full duplex, MOSI + MISO phases, byte
            // order left at its little-endian reset default (bits 10/11 unset)
            // to match `pack_word`/`unpack_word`.
            let ck_out = if self.cpha { SPI_CK_OUT_EDGE } else { 0 };
            // `enable_cs0` marks a hardware-framed master by setting these
            // bits. Preserve them across this whole-register USER write;
            // otherwise CTRL2's setup/hold values never take effect.
            let cs_timing = self.reg(SPI_USER).read_volatile() & (SPI_CS_SETUP | SPI_CS_HOLD);
            self.reg(SPI_USER)
                .write_volatile(SPI_DOUTDIN | SPI_USR_MOSI | SPI_USR_MISO | ck_out | cs_timing);

            // Start the transfer (SPI_USR, bit 18 -- not bit 0).
            self.reg(SPI_CMD).write_volatile(SPI_CMD_USR);

            // Wait for completion, bounded: SPI_USR self-clears when the
            // hardware finishes the transaction.
            let mut spins: u32 = 0;
            while self.reg(SPI_CMD).read_volatile() & SPI_CMD_USR != 0 {
                spins += 1;
                if spins > SPI_TIMEOUT_SPINS {
                    return Err(BusError::Timeout);
                }
                core::hint::spin_loop();
            }

            // Read RX data back out, 4 bytes per word.
            for w in 0..nwords {
                let start = w * 4;
                let end = (start + 4).min(len);
                let word = self.reg(SPI_W0 + (w as u32 * 4)).read_volatile();
                unpack_word(word, &mut rx[start..end]);
            }
        }

        Ok(())
    }

    /// Enable the hardware chip-select (CS0) output.
    ///
    /// `init` disables all three CS outputs by default — this bus normally drives
    /// no peripheral CS, and a stray asserted CS corrupts the first byte. A master
    /// talking to a *slave*, though, needs a real CS: the falling edge before the
    /// first clock is what frames the slave's transaction. Call this after `init`,
    /// then route `Signal::SpiCs(instance)` to a pad. The controller asserts CS0
    /// around each transaction automatically, including the configured setup
    /// and hold phases required for a slave to commit its received data.
    ///
    /// # Safety
    /// The host must be initialised.
    pub unsafe fn enable_cs0(&self) {
        reg::clear(self.reg(SPI_PIN), 1); // clear cs0_dis (SPI_PIN bit 0)
        reg::set(self.reg(SPI_USER), SPI_CS_SETUP | SPI_CS_HOLD);
    }

    /// Enable or disable the DMA path for transfers past the FIFO cap. On by
    /// default, per host.
    ///
    /// Disabled, a bulk transfer runs through the FIFO in cap-sized chunks
    /// instead — slower, but it needs no DMA-reachable buffer, so it is the
    /// escape hatch when a caller's buffer cannot be in DMA memory, or when the
    /// DMA channel is wanted for something else.
    pub fn set_dma(&self, enabled: bool) {
        if let Some(s) = addr::spi_instance(self.base).and_then(dma_slot) {
            // SAFETY: a bool store into this host's slot; a bus is single-owner.
            unsafe {
                (*addr_of_mut!(SPI_DMA))[s].enabled = enabled;
            }
        }
    }

    /// Move a transfer larger than the FIFO cap through the FIFO in cap-sized
    /// chunks. Correct for any length; used when DMA is disabled or has no
    /// channel. Each chunk is a full-duplex FIFO transfer.
    fn fifo_chunked(&self, tx: &[u8], rx: &mut [u8], len: usize) -> BusResult<()> {
        let mut off = 0;
        while off < len {
            let end = (off + SPI_MAX_BYTES).min(len);
            self.fifo_exchange(&tx[off..end], &mut rx[off..end])?;
            off = end;
        }
        Ok(())
    }
}

impl PhysicalBus for Esp32Spi {
    fn init(&mut self, config: &BusConfig) -> BusResult<()> {
        match config {
            BusConfig::Spi(hal::bus::SpiConfig { mosi, miso, sck, max_speed, mode }) => {
                let instance = addr::spi_instance(self.base).ok_or(BusError::InvalidConfig)?;

                // Clock and un-reset the peripheral before touching any of
                // its registers -- SPI2/SPI3 are gated off and held in reset
                // at boot, so every access below would otherwise be a no-op.
                let clk_bit = dport::clock_bit(self.base).ok_or(BusError::InvalidConfig)?;
                unsafe { dport::enable(clk_bit) };

                route_pins(instance, *mosi, *miso, *sck)?;

                self.apply_clock(max_speed.hz());

                unsafe {
                    // SPI mode (CPOL, CPHA). CPOL is the clock idle level:
                    // SPI_PIN.ck_idle_edge (bit 29). CPHA is the output edge:
                    // SPI_USER.ck_out_edge (bit 7), set per transfer alongside
                    // the rest of USER. (An earlier revision wrote CPOL/CPHA to
                    // SPI_PIN bits 2/1, which are really cs2_dis/cs1_dis -- that
                    // left the clock config untouched *and* toggled chip-select
                    // enables, whose setup/hold made the first DMA byte marginal.)
                    let (cpol, cpha) = match mode {
                        SpiMode::Mode0 => (0, 0),
                        SpiMode::Mode1 => (0, 1),
                        SpiMode::Mode2 => (1, 0),
                        SpiMode::Mode3 => (1, 1),
                    };
                    self.cpha = cpha != 0;

                    // Disable all three hardware chip-selects (cs0/1/2_dis,
                    // bits [2:0]) and set the clock idle level. This bus does not
                    // drive a peripheral CS; leaving one enabled asserts it around
                    // every transfer with a setup/hold that corrupts the first
                    // byte. esp-idf disables them the same way (PIN = 0x…1f).
                    let mut pin = self.reg(SPI_PIN).read_volatile();
                    pin |= 0b111; // cs0_dis | cs1_dis | cs2_dis
                    if cpol != 0 { pin |= 1 << 29; } else { pin &= !(1 << 29); }
                    self.reg(SPI_PIN).write_volatile(pin);

                    // Timing register, set to esp-idf's computed value for a
                    // GPIO-matrix master at these clocks (SPI_CTRL2 = 0x0002_001f):
                    //   MISO_DELAY_MODE = 2 ([17:16]) — the MISO signal routes
                    //     through the GPIO matrix (~2 cycles of input delay); this
                    //     moves the sample to the matching edge. Without it the
                    //     sampling point is marginal.
                    //   SETUP_TIME = 15 ([3:0]) — with CS_HOLD set in USER, this
                    //     is the CS-assert-to-first-clock gap. 16 cycles lets the
                    //     TX DMA/FIFO prime the first byte before the clock starts;
                    //     the reset default of 1 is too short, and the first byte
                    //     of every transfer then clocks out (and reads back) zero.
                    //   HOLD_TIME = 1 ([7:4]) — reset default, left as esp-idf has.
                    let mut ctrl2 = self.reg(SPI_CTRL2).read_volatile();
                    ctrl2 &= !((0b11 << 16) | 0xF);
                    ctrl2 |= (0b10 << 16) | 0xF;
                    self.reg(SPI_CTRL2).write_volatile(ctrl2);

                    // Enable master mode, disable slave.
                    let slave = self.reg(SPI_SLAVE);
                    reg::clear(slave, 1);
                }

                // Claim a DMA channel for this host, held for the driver's life,
                // so a transfer past the FIFO cap can run over DMA with no
                // arrangement by the caller. A host with no channel free keeps
                // to the FIFO path (chunked past the cap).
                if let Some(s) = dma_slot(instance) {
                    let host = if instance == 2 { Host::Spi2 } else { Host::Spi3 };
                    // SAFETY: init runs once per host before any transfer; this
                    // slot belongs to this host alone.
                    unsafe {
                        let slot = &mut (*addr_of_mut!(SPI_DMA))[s];
                        if slot.channel.is_none() {
                            slot.channel = dma::claim(host).ok();
                        }
                    }
                }
                Ok(())
            }
            _ => Err(BusError::InvalidConfig),
        }
    }
}

impl PhysicalTransfer for Esp32Spi {
    fn exchange(&self, tx: &[u8], rx: &mut [u8]) -> BusResult<()> {
        let len = tx.len().min(rx.len());

        let instance = addr::spi_instance(self.base).ok_or(BusError::InvalidConfig)?;
        let Some(s) = dma_slot(instance) else {
            // No DMA slot for this host (SPI1): FIFO only.
            return self.fifo_chunked(tx, rx, len);
        };

        // SAFETY: a bus is single-owner, and a transfer on one host is
        // synchronous, so nothing else touches this slot while this runs.
        let slot = unsafe { &mut (*addr_of_mut!(SPI_DMA))[s] };

        let reach = dma::DmaReach;
        let reachable = reach.reachable(tx.as_ptr() as u32, len as u32)
            && reach.reachable(rx.as_ptr() as u32, len as u32);
        let ndesc = descriptors_needed(len as u32) as usize;

        // Use DMA for *every* size when it is enabled, a channel is owned, the
        // buffers are DMA-reachable, and the chain fits. Routing small transfers
        // through the FIFO and large ones through DMA would mean a FIFO->DMA
        // transition, which the classic ESP32 SPI does not re-arm cleanly (the
        // DMA receive lands short by the FIFO transfer's byte count). esp-idf
        // avoids it the same way — DMA for all traffic once it is enabled.
        let use_dma =
            len > 0 && slot.enabled && slot.channel.is_some() && reachable && ndesc <= MAX_DESCS;

        if !use_dma {
            // A bulk transfer the caller wants over DMA but cannot be — an
            // unreachable buffer or one too large for the descriptor scratch —
            // is an error, not a silent slow bounce. Everything else takes the
            // FIFO path (cap-sized, or chunked past the cap when DMA is off).
            if slot.enabled && slot.channel.is_some() && len > SPI_MAX_BYTES {
                return Err(if ndesc > MAX_DESCS {
                    BusError::InvalidConfig
                } else {
                    BusError::DmaError
                });
            }
            return self.fifo_chunked(tx, rx, len);
        }

        // SAFETY: descriptors sit in this slot's DMA-reachable scratch; the
        // chains and the caller's buffers stay put across the transfer.
        let bank = slot.next_bank;
        slot.next_bank ^= 1;
        unsafe {
            let tx_head = build_chain(
                &mut slot.tx_descs[bank][..ndesc],
                tx.as_ptr() as u32,
                len as u32,
                Direction::Transmit,
            )
            .map_err(|_| BusError::DmaError)?;
            let rx_head = build_chain(
                &mut slot.rx_descs[bank][..ndesc],
                rx.as_mut_ptr() as u32,
                len as u32,
                Direction::Receive,
            )
            .map_err(|_| BusError::DmaError)?;

            // Descriptor owner/length writes and caller buffer writes must be
            // visible before the link register hands the chain to DMA.
            dma::sync_for_device();
            self.start_dma(tx_head, rx_head, len)?;

            // The peripheral transaction is complete only at TRANS_DONE;
            // descriptor length can reach `len` before the SPI receive state
            // has retired. `transfer_dma` uses that hardware completion bit.
            self.wait_dma_done()?;
            Ok(())
        }
    }

    /// Re-clock a live controller: only SPI_CLOCK changes, so a transfer in
    /// between runs at the new rate with no other disturbance.
    fn set_speed(&self, speed: BusSpeed) -> BusResult<()> {
        let hz = speed.hz();
        if hz == 0 || hz > APB_HZ {
            return Err(BusError::InvalidConfig);
        }
        self.apply_clock(hz);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode a `SPI_CLOCK` value into (pre, n, h, l) as used (register + 1),
    /// or `None` for a `clk_equ_sysclk` word.
    fn decode_clock(reg: u32) -> Option<(u32, u32, u32, u32)> {
        if reg & (1 << 31) != 0 {
            return None;
        }
        let pre = ((reg >> 18) & 0x1FFF) + 1;
        let n = ((reg >> 12) & 0x3F) + 1;
        let h = ((reg >> 6) & 0x3F) + 1;
        let l = (reg & 0x3F) + 1;
        Some((pre, n, h, l))
    }

    #[test]
    fn clock_divider_matches_esp_idf_known_good_values() {
        // The 4 MHz register esp-idf produces off an 80 MHz APB, and the value
        // the issue #91 dump confirms the working path used. Golden.
        assert_eq!(spi_clock_reg(80_000_000, 4_000_000), 0x0001_3253);
        // 40 MHz is a single divide-by-two: pre 1, n 2, h 1, l 2.
        assert_eq!(decode_clock(spi_clock_reg(80_000_000, 40_000_000)), Some((1, 2, 1, 2)));
    }

    #[test]
    fn a_1mhz_clock_is_consistent_not_the_buggy_register() {
        // The bug: n = 80 overflowed its 6-bit field to 15 while h stayed 39,
        // giving CLOCK=0x0000f9cf with h > n — a register the core never
        // finishes, so fifo_exchange() hung.
        let reg = spi_clock_reg(80_000_000, 1_000_000);
        assert_ne!(reg, 0x0000_f9cf, "reproduced the #91 hang register");
        let (pre, n, h, l) = decode_clock(reg).expect("not equ_sysclk at 1 MHz");
        // Effective clock is exactly 1 MHz, and the phase counters are sane.
        assert_eq!(80_000_000 / (pre * n), 1_000_000);
        assert!(h <= n, "high phase {h} exceeds the period {n} — the bug");
        assert_eq!(l, n, "low phase must span the whole low half");
    }

    #[test]
    fn the_divider_reaches_low_frequencies_within_tolerance() {
        // Across the range that previously overflowed the counter, the effective
        // clock stays within one LSB of the divider and the register is always
        // consistent (h <= n). 100 kHz is well below the old 64-count ceiling.
        for &hz in &[8_000_000, 2_000_000, 1_000_000, 500_000, 100_000, 40_000] {
            let (pre, n, h, l) = decode_clock(spi_clock_reg(80_000_000, hz)).unwrap();
            let eff = 80_000_000 / (pre * n);
            assert!(eff.abs_diff(hz) * 100 <= hz * 5, "{hz} Hz off by too much: {eff}");
            assert!(h <= n && l == n, "inconsistent phase counters at {hz} Hz");
        }
    }

    #[test]
    fn above_three_quarters_apb_uses_the_system_clock_directly() {
        // 80 MHz off an 80 MHz APB can only be clk_equ_sysclk (bit 31).
        assert_eq!(spi_clock_reg(80_000_000, 80_000_000), 1 << 31);
        assert!(decode_clock(spi_clock_reg(80_000_000, 80_000_000)).is_none());
    }

    #[test]
    fn register_offsets_match_trm_spi_summary() {
        // Regression guard: the previous revision had CLOCK=0x0C, USER=0x10,
        // USER1=0x14, PIN=0x18, SLAVE=0x1C -- each one register slot early.
        assert_eq!(SPI_CMD, 0x00);
        assert_eq!(SPI_ADDR, 0x04);
        assert_eq!(SPI_CTRL, 0x08);
        assert_eq!(SPI_CTRL1, 0x0C);
        assert_eq!(SPI_RD_STATUS, 0x10);
        assert_eq!(SPI_CTRL2, 0x14);
        assert_eq!(SPI_CLOCK, 0x18);
        assert_eq!(SPI_USER, 0x1C);
        assert_eq!(SPI_USER1, 0x20);
        assert_eq!(SPI_USER2, 0x24);
        assert_eq!(SPI_MOSI_DLEN, 0x28);
        assert_eq!(SPI_MISO_DLEN, 0x2C);
        assert_eq!(SPI_PIN, 0x34);
        assert_eq!(SPI_SLAVE, 0x38);
        assert_eq!(SPI_W0, 0x80);
    }

    #[test]
    fn usr_start_bit_is_18_not_0() {
        // The core of the reported bug: writing/polling bit 0 (SPI_DOUTDIN)
        // instead of bit 18 (SPI_USR) never starts a real transaction and
        // can spin forever waiting for a bit nothing will clear.
        assert_eq!(SPI_CMD_USR, 1 << 18);
        assert_ne!(SPI_CMD_USR, 1);
    }

    #[test]
    fn pack_and_unpack_round_trip_four_bytes_per_word() {
        let bytes = [0xDE, 0xAD, 0xBE, 0xEF];
        let word = pack_word(&bytes);
        // Little-endian: first byte transmitted is the LSB.
        assert_eq!(word, 0xEFBE_ADDE);
        let mut out = [0u8; 4];
        unpack_word(word, &mut out);
        assert_eq!(out, bytes);
    }

    #[test]
    fn pack_handles_partial_final_word() {
        let bytes = [0x01, 0x02];
        let word = pack_word(&bytes);
        assert_eq!(word, 0x0000_0201);
    }

    #[test]
    fn dport_clk_bits_are_distinct_and_match_known_bases() {
        use soc_esp32::addr::{SPI2_BASE, SPI3_BASE};
        assert_eq!(dport::clock_bit(SPI2_BASE), Some(dport::ClockBit::SPI2));
        assert_eq!(dport::clock_bit(SPI3_BASE), Some(dport::ClockBit::SPI3));
        assert_ne!(dport::clock_bit(SPI2_BASE), dport::clock_bit(SPI3_BASE));
        assert_eq!(dport::clock_bit(0xDEAD_BEEF), None);
    }

    #[test]
    fn spi1_is_not_addressable_as_a_general_purpose_controller() {
        // SPI1 drives the boot flash; routing it anywhere bricks the running
        // image.
        use soc_esp32::addr::SPI1_BASE;
        assert_eq!(addr::spi_instance(SPI1_BASE), None);
    }

    #[test]
    fn a_bus_may_not_reuse_one_pad_for_two_signals() {
        assert!(route_pins(3, 23, 23, 18).is_err());
        assert!(route_pins(3, 23, 19, 19).is_err());
    }

    #[test]
    fn mosi_and_sck_cannot_land_on_input_only_pads() {
        // GPIO34-39 have no output driver.
        assert!(route_pins(3, 34, 19, 18).is_err());
        assert!(route_pins(3, 23, 19, 35).is_err());
        // MISO on one is fine -- it is an input.
        let mux = Esp32PinMux::new();
        assert!(mux.can_route(Signal::SpiMiso(3), 34).is_ok());
    }

    #[test]
    fn transfer_length_is_capped_at_the_data_buffer_size() {
        assert_eq!(SPI_MAX_BYTES, 64);
    }
}
