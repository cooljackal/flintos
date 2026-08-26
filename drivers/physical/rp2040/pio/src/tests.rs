// SPDX-License-Identifier: Apache-2.0
extern crate std;
use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    vec::Vec,
};
type Audit = Arc<Mutex<Vec<(u32, u32)>>>;

#[derive(Default)]
struct Fake {
    regs: BTreeMap<u32, u32>,
    writes: Vec<(u32, u32)>,
    now: u32,
    step: u32,
    reads: u32,
    audit: Option<Audit>,
}
impl Registers for Fake {
    fn read(&mut self, a: u32) -> u32 {
        self.reads += 1;
        *self.regs.get(&a).unwrap_or(&0)
    }
    fn write(&mut self, a: u32, v: u32) {
        if let Some(audit) = &self.audit {
            audit.lock().unwrap().push((a, v));
        }
        self.writes.push((a, v));
        self.regs.insert(a, v);
    }
    fn now_us(&mut self) -> u32 {
        self.now = self.now.wrapping_add(self.step);
        self.now
    }
}
const CFG: Config = Config {
    frequency_hz: 1_000_000,
    input: false,
    output: false,
};
const NOP: Instruction = Instruction::new(Operation::Nop, 0);
fn engine() -> Engine<Fake> {
    Engine::new(
        Fake::default(),
        Port {
            block: 0,
            input: Some(3),
            output: Some(2),
        },
        None,
    )
}

#[test]
fn allocation_handles_full_memory_without_shift_overflow() {
    assert_eq!(allocation(0, 32), Ok((0, u32::MAX)));
    assert_eq!(allocation(u32::MAX, 1), Err(Error::NoSpace));
    assert_eq!(allocation(0, 0), Err(Error::Invalid));
    assert_eq!(allocation(0, 33), Err(Error::Invalid));
    assert_eq!(allocation(0xf000_0000, 4), Ok((24, 0x0f00_0000)));
    assert_eq!(allocation(0xaaaa_aaaa, 2), Err(Error::NoSpace));
}
#[test]
fn allocation_relocates_every_jump_and_refuses_escape() {
    assert_eq!(
        encode(Instruction::new(Operation::Jump(1), 31), 28, 4, CFG),
        Ok(0x1f1d)
    );
    assert_eq!(
        encode(Instruction::new(Operation::JumpDecrement(2), 0), 28, 4, CFG),
        Ok(0x5e)
    );
    assert_eq!(
        encode(Instruction::new(Operation::Jump(4), 0), 28, 4, CFG),
        Err(Error::Invalid)
    );
    assert_eq!(
        encode(Instruction::new(Operation::Jump(1), 0), 31, 4, CFG),
        Err(Error::Invalid)
    );
    assert_eq!(
        encode(Instruction::new(Operation::Nop, 32), 0, 1, CFG),
        Err(Error::Invalid)
    );
}
#[test]
fn invalid_instruction_does_not_write_or_reserve_anything() {
    let mut e = engine();
    for op in [
        Operation::InputBit,
        Operation::OutputBit,
        Operation::SetOutput(true),
        Operation::WaitInput(false),
        Operation::SetCounter(32),
        Operation::Jump(1),
    ] {
        assert_eq!(
            e.configure(0, &[Instruction::new(op, 0)], CFG),
            Err(Error::Invalid)
        );
        assert_eq!(e.memory, 0);
        assert!(e.io.writes.is_empty());
    }
}
#[test]
fn lowering_is_the_vendor_encoding_not_application_native_words() {
    let cfg = Config {
        input: true,
        output: true,
        ..CFG
    };
    for (op, bits) in [
        (Operation::Pull, 0x80a0),
        (Operation::Push, 0x8020),
        (Operation::OutputBit, 0x6001),
        (Operation::InputBit, 0x4001),
        (Operation::WaitInput(true), 0x20a0),
        (Operation::SetOutput(true), 0xe001),
        (Operation::SetCounter(31), 0xe03f),
        (Operation::Nop, 0xa042),
    ] {
        assert_eq!(encode(Instruction::new(op, 0), 0, 32, cfg), Ok(bits));
    }
}
#[test]
fn divisor_bounds_and_rounding_do_not_exceed_requested_rate() {
    assert_eq!(divider(12_000_000, 1_000_000), Ok(12 << 16));
    assert_eq!(divider(125_000_000, 1_000_000), Ok(125 << 16));
    assert_eq!(divider(65_536, 1), Ok(0));
    for (cpu, rate) in [
        (0, 1),
        (12_000_000, 0),
        (12_000_000, 12_000_001),
        (12_000_000, 1),
    ] {
        assert_eq!(divider(cpu, rate), Err(Error::Invalid));
    }
    let encoded = divider(12_000_000, 7_000_000).unwrap();
    assert!(12_000_000u64 * 256 <= u64::from(encoded >> 8) * 7_000_000);
}
#[test]
fn machines_and_program_memory_have_exclusive_lifetimes() {
    let mut e = engine();
    for m in 0..4 {
        e.configure(m, &[NOP; 8], CFG).unwrap();
    }
    assert_eq!(e.memory, u32::MAX);
    assert_eq!(e.configure(0, &[NOP], CFG), Err(Error::Busy));
    assert_eq!(e.configure(4, &[NOP], CFG), Err(Error::Invalid));
    e.cancel(2).unwrap();
    assert!(e.machines[0].is_some());
    assert!(e.machines[3].is_some());
    e.configure(2, &[NOP; 8], CFG).unwrap();
    e.reset();
    assert_eq!(e.memory, 0);
    assert!(e.machines.iter().all(Option::is_none));
    e.configure(0, &[NOP; 32], CFG).unwrap();
    assert_eq!(e.configure(1, &[NOP], CFG), Err(Error::NoSpace));
}
#[test]
fn configured_machines_cannot_share_the_routed_pins() {
    let mut e = engine();
    let out = Config {
        output: true,
        ..CFG
    };
    let input = Config { input: true, ..CFG };
    e.configure(0, &[NOP], out).unwrap();
    assert_eq!(e.configure(1, &[NOP], out), Err(Error::Busy));
    e.configure(1, &[NOP], input).unwrap();
    assert_eq!(e.configure(2, &[NOP], input), Err(Error::Busy));
    e.cancel(0).unwrap();
    e.configure(2, &[NOP], out).unwrap();
    e.port.input = None;
    assert_eq!(e.configure(3, &[NOP], input), Err(Error::Invalid));
}
#[test]
fn physical_port_validation_rejects_aliases_and_unbonded_pins() {
    assert_eq!(
        Port {
            block: 0,
            input: Some(3),
            output: Some(2)
        }
        .mask(),
        Ok(12)
    );
    for p in [
        Port {
            block: 2,
            input: None,
            output: None,
        },
        Port {
            block: 0,
            input: Some(30),
            output: None,
        },
        Port {
            block: 0,
            input: Some(3),
            output: Some(3),
        },
    ] {
        assert_eq!(p.mask(), Err(Error::Invalid));
    }
}
#[test]
fn fifo_full_or_empty_never_performs_an_unsafe_fifo_access() {
    let mut e = engine();
    e.configure(3, &[NOP], CFG).unwrap();
    e.io.regs
        .insert(e.port.base() + FSTAT, (1 << 19) | (1 << 11));
    e.io.writes.clear();
    assert_eq!(e.try_write(3, 42), Err(Error::WouldBlock));
    assert_eq!(e.try_read(3), Err(Error::WouldBlock));
    assert!(e.io.writes.is_empty());
    e.io.regs.insert(e.port.base() + FSTAT, 0);
    e.io.regs.insert(e.port.base() + 0x2c, 0xdead_beef);
    e.try_write(3, 42).unwrap();
    assert_eq!(e.try_read(3), Ok(0xdead_beef));
    assert_eq!(e.io.writes.last(), Some(&(e.port.base() + 0x1c, 42)));
    assert_eq!(e.try_read(4), Err(Error::Invalid));
    assert_eq!(e.try_write(0, 0), Err(Error::NotConfigured));
}
#[test]
fn timeout_flushes_disables_and_releases_for_reconfiguration() {
    let mut e = engine();
    e.configure(
        0,
        &[NOP],
        Config {
            output: true,
            ..CFG
        },
    )
    .unwrap();
    e.io.regs
        .insert(e.port.base() + FSTAT, (1 << 16) | (1 << 8));
    e.io.step = 1;
    assert_eq!(e.exchange(0, 42, 10), Err(Error::Timeout));
    assert_eq!(e.memory, 0);
    assert_eq!(e.start(0), Err(Error::NotConfigured));
    let b = e.port.base();
    assert!(e.io.writes.contains(&(b + 0x3000, 1)));
    assert!(e.io.writes.contains(&(b + SM + 8, SHIFT | (1 << 31))));
    assert!(e.io.writes.contains(&(b + SM + 0x10, 0xe080))); // tri-state
    e.configure(0, &[NOP], CFG).unwrap();
    e.io.regs.insert(b + FSTAT, 0);
    e.io.regs.insert(b + 0x20, 42);
    assert_eq!(e.exchange(0, 42, 10), Ok(42));
}
#[test]
fn stopped_and_wrapping_timers_are_bounded() {
    for (start, step) in [(0, 0), (u32::MAX - 2, 1)] {
        let mut e = engine();
        e.configure(0, &[NOP], CFG).unwrap();
        e.io.regs.insert(e.port.base() + FSTAT, 1 << 8);
        e.io.now = start;
        e.io.step = step;
        assert_eq!(e.exchange(0, 1, 10), Err(Error::Timeout));
        assert!(e.io.reads <= POLL_LIMIT + 10);
    }
}
#[test]
fn invalid_timeout_keeps_the_machine_and_performs_no_io() {
    let mut e = engine();
    e.configure(0, &[NOP], CFG).unwrap();
    e.io.writes.clear();
    for timeout in [0, 1_000_001] {
        assert_eq!(e.exchange(0, 1, timeout), Err(Error::Invalid));
    }
    assert!(e.io.writes.is_empty());
    assert!(e.machines[0].is_some());
}
#[test]
fn initialization_has_a_poll_limit_and_does_not_touch_unowned_pins_on_failure() {
    let mut e = engine();
    assert_eq!(e.initialize(), Err(Error::Hardware));
    assert_eq!(e.io.reads, POLL_LIMIT);
    assert_eq!(e.io.writes.len(), 2);
    assert!(!e.initialized);
}
#[test]
fn initialization_disables_interrupts_and_configures_only_owned_pins() {
    let mut e = engine();
    e.io.regs.insert(soc_rp2040::RESETS_BASE + 8, u32::MAX);
    e.initialize().unwrap();
    assert!(e.initialized);
    for offset in [0x12c, 0x130, 0x138, 0x13c] {
        assert!(e.io.writes.contains(&(e.port.base() + offset, 0)));
    }
    assert_eq!(e.io.regs[&(soc_rp2040::IO_BANK0_BASE + 4 + 2 * 8)], 6);
    assert_eq!(e.io.regs[&(soc_rp2040::IO_BANK0_BASE + 4 + 3 * 8)], 6);
}
#[test]
fn host_open_never_accesses_mmio() {
    assert!(matches!(
        Rp2040Pio::open(Port {
            block: 0,
            input: Some(3),
            output: Some(2)
        }),
        Err(Error::Unsupported)
    ));
}

#[test]
fn drop_disables_the_machine_and_restores_pin_state_before_releasing_the_lease() {
    let mut e = engine();
    e.io.regs.insert(soc_rp2040::RESETS_BASE + 8, u32::MAX);
    let mux = soc_rp2040::IO_BANK0_BASE + 4 + 2 * 8;
    let pad = soc_rp2040::PADS_BANK0_BASE + 4 + 2 * 4;
    e.io.regs.insert(mux, 5);
    e.io.regs.insert(pad, 0x56);
    let audit = Arc::new(Mutex::new(Vec::new()));
    e.io.audit = Some(audit.clone());
    e.initialize().unwrap();
    e.configure(
        0,
        &[NOP],
        Config {
            output: true,
            ..CFG
        },
    )
    .unwrap();
    let start = audit.lock().unwrap().len();
    drop(e);
    let writes = audit.lock().unwrap();
    let cleanup = &writes[start..];
    let disabled = cleanup.iter().position(|w| *w == (0x5020_3000, 1)).unwrap();
    let tristate = cleanup
        .iter()
        .position(|w| *w == (0x5020_00d8, 0xe080))
        .unwrap();
    let restored = cleanup.iter().position(|w| *w == (mux, 5)).unwrap();
    assert!(disabled < tristate && tristate < restored);
    assert!(cleanup.contains(&(pad, 0x56)));
}
