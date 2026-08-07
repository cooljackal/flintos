// SPDX-License-Identifier: Apache-2.0

//! Peripheral interrupt routing: which CPU interrupt a peripheral fires on.
//!
//! The Xtensa core has 32 interrupt inputs. The ESP32 has 69 peripheral
//! sources. A crossbar in DPORT decides which of the 32 each of the 69 lands
//! on, and until a source is pointed at an interrupt the CPU is enabled for,
//! that peripheral cannot interrupt anything — its interrupt-enable bits set
//! happily, its status bits assert, and nothing happens.
//!
//! Everything the kernel handled before this existed was internal to the core:
//! Timer0 on CPU interrupt 6 and the software interrupt on 7. Neither goes
//! through the crossbar, which is why a driver could be written, enabled, and
//! silently never serviced.
//!
//! # Where a source points before you route it
//!
//! Reset value is 5'd16 — CPU interrupt 16, an internal timer no peripheral
//! can reach. So the default is not "unconnected" but "parked somewhere
//! harmless", and [`park`] puts a source back rather than clearing to zero.
//! Clearing to zero would aim it at CPU interrupt 0, which is a perfectly
//! usable external interrupt and might belong to something else.
//!
//! # Which interrupts a peripheral may use
//!
//! Two conditions, both load-bearing, both checked by
//! [`can_serve_peripheral`]:
//!
//! 1. The input must be `EXTERN_LEVEL`. Six of the 32 are wired to the core's
//!    own timers, software interrupts, NMI or profiling and are not connected
//!    to the crossbar at all; four more are `EXTERN_EDGE`, which latch and
//!    need explicit acknowledgement the level path does not do.
//! 2. It must be **level 1**. `vectors.S` implements level 1; levels 2 through
//!    5 land in the "unhandled higher-level interrupt" stub. Routing a
//!    peripheral to level 2 produces a fault at the first interrupt instead of
//!    a call to its handler.
//!
//! That leaves 0–5, 8, 9, 12, 13, 17 and 18, minus 6 and 7 which fail the
//! first test anyway. Rejecting the rest here is the point: the failure is
//! otherwise a board that boots and then dies at the first interrupt.
//!
//! # Sources
//!
//! `DPORT_PRO_MAC_INTR_MAP_REG` is source 0 at `DPORT_BASE + 0x104`, and the
//! table is indexed by source number from there. Verified against four
//! independent defines in `dport_reg.h` rather than assumed from one:
//!
//! | Source | Peripheral | Header says | `0x104 + 4n` |
//! |---|---|---|---|
//! | 10 | SLC0 | 0x12C | 0x12C |
//! | 34 | UART | 0x18C | 0x18C |
//! | 47 | RMT | 0x1C0 | 0x1C0 |
//! | 49 | I2C_EXT0 | 0x1C8 | 0x1C8 |
//!
//! Levels and types come from the ESP32's `core-isa.h`; the count from
//! `ETS_MAX_INTR_SOURCE`.

use crate::addr::DPORT_BASE;

/// The PRO CPU's crossbar table. Source 0 lives here.
///
/// There is a matching APP CPU table further along; nothing routes to the
/// second core until it is brought up, so it has no constant yet.
const PRO_MAP_BASE: u32 = DPORT_BASE + 0x104;

/// `ETS_MAX_INTR_SOURCE`. Valid sources are `0..SOURCE_COUNT`.
pub const SOURCE_COUNT: u8 = 69;

/// Where a source points until something routes it.
///
/// CPU interrupt 16 is an internal timer, so a parked source is inert rather
/// than merely unconfigured.
pub const PARKED: u8 = 16;

/// Interrupt level of each of the core's 32 inputs, from `core-isa.h`.
///
/// A table rather than a range check because it is not monotonic: 11 and 15
/// are level 3 sitting between level-1 neighbours, and 14 is the NMI at level
/// 7. Any rule simpler than the table is wrong somewhere.
const LEVEL: [u8; 32] = [
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 3, 1, 1, 7, 3, //  0-15
    5, 1, 1, 2, 2, 2, 3, 3, 4, 4, 5, 3, 4, 3, 4, 5, // 16-31
];

/// The interrupt level `cpu_int` fires at, or `None` if there is no such input.
pub const fn level(cpu_int: u8) -> Option<u8> {
    if cpu_int < 32 {
        Some(LEVEL[cpu_int as usize])
    } else {
        None
    }
}

/// Whether the core input is a plain level-triggered external interrupt.
///
/// Excludes the core's own timers (6, 15, 16), software interrupts (7, 29),
/// profiling (11), the NMI (14), and the four edge-triggered inputs (10, 22,
/// 28, 30) whose latching this kernel does not acknowledge.
pub const fn is_extern_level(cpu_int: u8) -> bool {
    matches!(cpu_int, 0..=5 | 8 | 9 | 12 | 13 | 17..=21 | 23..=27 | 31)
}

/// Whether a peripheral routed here would actually be serviced.
///
/// See the module header: external-level *and* level 1, because those are the
/// interrupts `vectors.S` has a handler for.
pub const fn can_serve_peripheral(cpu_int: u8) -> bool {
    is_extern_level(cpu_int) && matches!(level(cpu_int), Some(1))
}

/// The crossbar register for a peripheral source.
pub const fn map_reg(source: u8) -> Option<u32> {
    if source < SOURCE_COUNT {
        Some(PRO_MAP_BASE + 4 * source as u32)
    } else {
        None
    }
}

/// Why a route was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteError {
    /// No such peripheral source on this chip.
    NoSuchSource,
    /// The core input exists but nothing would service it — see
    /// [`can_serve_peripheral`].
    Unusable,
}

/// Point `source` at `cpu_int`.
///
/// The caller still has to enable `cpu_int` on the core and enable the
/// peripheral's own interrupt bits; this only decides where it lands.
///
/// # Safety
/// Writes a DPORT crossbar register. Two peripherals may share a CPU interrupt
/// — the handler then has to work out which fired — but routing a source that
/// another driver owns will steal its interrupts.
pub unsafe fn route(source: u8, cpu_int: u8) -> Result<(), RouteError> {
    let reg = map_reg(source).ok_or(RouteError::NoSuchSource)?;
    if !can_serve_peripheral(cpu_int) {
        return Err(RouteError::Unusable);
    }
    crate::dport::write(reg, cpu_int as u32);
    Ok(())
}

/// Put `source` back where reset left it, so it can no longer interrupt.
///
/// # Safety
/// Writes a DPORT crossbar register.
pub unsafe fn park(source: u8) -> Result<(), RouteError> {
    let reg = map_reg(source).ok_or(RouteError::NoSuchSource)?;
    crate::dport::write(reg, PARKED as u32);
    Ok(())
}

/// Read back where a source currently points.
///
/// # Safety
/// Reads a DPORT register. No side effects.
pub unsafe fn routed_to(source: u8) -> Option<u8> {
    let reg = map_reg(source)?;
    Some((crate::dport::read(reg) & 0x1F) as u8)
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_is_indexed_by_source_number() {
        // Four defines quoted from dport_reg.h. One would prove the base; four
        // spread across the table prove the stride and that nothing is
        // reordered partway through, which is the assumption actually being
        // made by indexing it.
        assert_eq!(map_reg(0), Some(0x3FF0_0104), "DPORT_PRO_MAC_INTR_MAP_REG");
        assert_eq!(map_reg(10), Some(0x3FF0_012C), "SLC0");
        assert_eq!(map_reg(34), Some(0x3FF0_018C), "UART");
        assert_eq!(map_reg(47), Some(0x3FF0_01C0), "RMT");
        assert_eq!(map_reg(49), Some(0x3FF0_01C8), "I2C_EXT0");
    }

    #[test]
    fn a_source_off_the_end_has_no_register() {
        // ETS_MAX_INTR_SOURCE is 69, so 68 is the last one. Computing an
        // address for 69 would land on whatever DPORT keeps after the table.
        assert!(map_reg(68).is_some());
        assert_eq!(map_reg(69), None);
        assert_eq!(map_reg(255), None);
    }

    #[test]
    fn the_kernels_own_interrupts_are_refused() {
        // Timer0 and the software interrupt are how the scheduler runs.
        // Routing a peripheral onto either would have it dispatched as a tick.
        assert!(!can_serve_peripheral(6), "Timer0");
        assert!(!can_serve_peripheral(7), "software");
    }

    #[test]
    fn interrupts_above_level_one_are_refused() {
        // vectors.S handles level 1; 2-5 reach the "unhandled" stub. This is
        // the check that turns a fault at the first interrupt into an error at
        // the call that caused it.
        for cpu_int in [19, 20, 21, 22, 23, 24, 25, 26, 27, 31] {
            assert!(
                !can_serve_peripheral(cpu_int),
                "CPU interrupt {cpu_int} is level {:?} and has no handler",
                level(cpu_int)
            );
        }
    }

    #[test]
    fn edge_and_core_internal_interrupts_are_refused() {
        for cpu_int in [10, 22, 28, 30] {
            assert!(!can_serve_peripheral(cpu_int), "{cpu_int} is edge-triggered");
        }
        for cpu_int in [11, 14, 15, 16, 29] {
            assert!(!can_serve_peripheral(cpu_int), "{cpu_int} is core-internal");
        }
    }

    #[test]
    fn the_usable_set_is_exactly_the_level_one_external_inputs() {
        // Pinned as a set rather than spot-checked, so widening the rule has to
        // be a deliberate edit to this list.
        const USABLE: [u8; 12] = [0, 1, 2, 3, 4, 5, 8, 9, 12, 13, 17, 18];
        for cpu_int in 0..32u8 {
            let want = USABLE.contains(&cpu_int);
            assert_eq!(
                can_serve_peripheral(cpu_int),
                want,
                "CPU interrupt {cpu_int}: level {:?}, extern_level {}",
                level(cpu_int),
                is_extern_level(cpu_int)
            );
        }
    }

    #[test]
    fn levels_come_from_the_table_not_a_rule() {
        // The non-monotonic entries: any range-based shortcut gets these wrong.
        assert_eq!(level(11), Some(3), "profiling, between level-1 neighbours");
        assert_eq!(level(14), Some(7), "NMI");
        assert_eq!(level(16), Some(5));
        assert_eq!(level(13), Some(1));
        assert_eq!(level(32), None);
    }

    #[test]
    fn parking_is_where_reset_leaves_a_source_not_zero() {
        // dport_reg.h: "default: 5'd16". Clearing to 0 would aim the source at
        // CPU interrupt 0, which is usable and might belong to another driver.
        assert_eq!(PARKED, 16);
        assert!(!can_serve_peripheral(PARKED), "parked must be inert");
    }
}
