// SPDX-License-Identifier: Apache-2.0

//! ARMv6-M PMSAv6 encoding. RP2040 implements eight regions, minimum 256
//! bytes (RP2040 datasheet §2.4.6), not the 32-byte minimum of ARMv7-M.
//! MMIO is deliberately unsafe: only the privileged scheduler may replace a
//! domain, with local interrupts masked and no concurrent user execution.

use hal::isolation::{Access, Region, TaskMemory};

pub const REGIONS: usize = 8;

/// (RBAR, RASR), with normal shareable non-cacheable memory (TEX=1, S=1).
/// Disabled slots are completely cleared, never inherited from another task.
pub fn encode(region: Region) -> (u32, u32) {
    let permissions = match region.access() {
        Access::ReadOnly | Access::ReadExecute => 6,
        Access::ReadWrite => 3,
    };
    let xn = u32::from(region.access() != Access::ReadExecute) << 28;
    let size = (region.size().ilog2() - 1) << 1;
    let subregions = u32::from(region.guarded()) << 8;
    (
        region.base(),
        xn | (permissions << 24) | (1 << 19) | (1 << 18) | subregions | size | 1,
    )
}

pub fn image(memory: Option<TaskMemory>) -> [(u32, u32); REGIONS] {
    let mut slots = [(0, 0); REGIONS];
    if let Some(memory) = memory {
        slots[0] = encode(memory.code());
        slots[1] = encode(memory.stack());
        if let Some(data) = memory.data() {
            slots[2] = encode(data);
        }
    }
    slots
}

#[cfg(target_arch = "arm")]
pub fn available() -> bool {
    // A missing/unexpected MPU is an error, not an unprotected fallback.
    unsafe { (0xe000_ed90 as *const u32).read_volatile() == 0x800 }
}

/// Replace all regions on this core, retaining the privileged default map.
/// HardFault/NMI bypass protection so the fatal reporter can always run.
///
/// # Safety
/// Handler/privileged mode only, interrupts masked, trusted exclusive grants.
/// The caller must set the matching CONTROL privilege before exception return.
#[cfg(target_arch = "arm")]
pub unsafe fn activate(memory: Option<TaskMemory>) {
    unsafe {
        core::arch::asm!("dsb", options(nostack));
        (0xe000_ed94 as *mut u32).write_volatile(0);
        for (index, (base, attributes)) in image(memory).into_iter().enumerate() {
            (0xe000_ed98 as *mut u32).write_volatile(index as u32);
            (0xe000_ed9c as *mut u32).write_volatile(base);
            (0xe000_eda0 as *mut u32).write_volatile(attributes);
        }
        (0xe000_ed94 as *mut u32).write_volatile(5); // ENABLE | PRIVDEFENA
        core::arch::asm!("dsb", "isb", options(nostack));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rights_size_and_guard_match_pmsav6() {
        let rx = Region::new(0x1000_0000, 4096, Access::ReadExecute).unwrap();
        assert_eq!(encode(rx), (0x1000_0000, 0x060c_0017));
        assert_eq!(
            encode(Region::stack(0x2000_0000, 4096).unwrap()),
            (0x2000_0000, 0x130c_0117)
        );
        let ro = Region::new(0x1000_0000, 256, Access::ReadOnly).unwrap();
        assert_eq!(encode(ro), (0x1000_0000, 0x160c_000f));
    }
    #[test]
    fn unused_regions_and_privileged_switch_clear_all_old_grants() {
        let m = TaskMemory::new(
            Region::new(0x1000_0000, 4096, Access::ReadExecute).unwrap(),
            Region::stack(0x2000_0000, 4096).unwrap(),
            None,
            0x1000_0001,
        )
        .unwrap();
        assert!(image(Some(m))[2..].iter().all(|r| *r == (0, 0)));
        assert_eq!(image(None), [(0, 0); 8]);
    }
}
