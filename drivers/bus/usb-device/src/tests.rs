// SPDX-License-Identifier: Apache-2.0
extern crate std;
use super::*;
use std::{collections::VecDeque, vec, vec::Vec};

#[derive(Default)]
struct Mock {
    events: VecDeque<Event>,
    writes: Vec<(u8, Vec<u8>)>,
    out: Vec<u8>,
    armed: Vec<u8>,
    stalls: [bool; 32],
    busy: [bool; 32],
    address: u8,
    configured: Vec<u8>,
}
fn index(ep: u8) -> usize {
    usize::from(ep & 15) * 2 + usize::from(ep & 128 != 0)
}
impl DeviceController for Mock {
    fn poll(&mut self) -> Result<Option<Event>, Error> {
        let event = self.events.pop_front();
        if matches!(event, Some(Event::Setup(_) | Event::Reset)) {
            self.stalls[0] = false;
            self.stalls[1] = false;
            self.busy[0] = false;
            self.busy[1] = false;
        }
        if let Some(Event::InComplete(ep) | Event::Out(ep)) = event {
            self.busy[index(ep)] = false;
        }
        if matches!(event, Some(Event::Reset)) {
            self.busy.fill(false);
        }
        Ok(event)
    }
    fn configure(&mut self, ep: u8, _: EndpointType) -> Result<(), Error> {
        self.configured.push(ep);
        Ok(())
    }
    fn close_data_endpoints(&mut self) {
        self.configured.clear();
        self.stalls.fill(false);
        self.busy[2..].fill(false);
    }
    fn set_address(&mut self, address: u8) {
        self.address = address;
    }
    fn write_in(&mut self, ep: u8, data: &[u8]) -> Result<(), Error> {
        assert!(data.len() <= 64);
        assert!(!self.busy[index(ep)], "IN buffer overwritten before ACK");
        self.busy[index(ep)] = true;
        self.writes.push((ep, data.to_vec()));
        Ok(())
    }
    fn arm_out(&mut self, ep: u8) -> Result<(), Error> {
        assert!(!self.busy[index(ep)], "OUT buffer armed twice");
        self.busy[index(ep)] = true;
        self.armed.push(ep);
        Ok(())
    }
    fn read_out(&mut self, _: u8, data: &mut [u8]) -> Result<usize, Error> {
        let len = self.out.len();
        data[..len].copy_from_slice(&self.out);
        self.out.clear();
        Ok(len)
    }
    fn set_stall(&mut self, ep: u8, halt: bool) -> Result<(), Error> {
        self.stalls[index(ep)] = halt;
        self.busy[index(ep)] = false;
        Ok(())
    }
    fn stalled(&self, ep: u8) -> bool {
        self.stalls[index(ep)]
    }
}
fn identity() -> Identity {
    Identity {
        vid: 0x1209,
        pid: 1,
        manufacturer: "FlintOS",
        product: "test",
        serial: None,
        allow_reset: true,
    }
}
fn device() -> Device<Mock> {
    Device::new(Mock::default(), identity())
}
fn setup(d: &mut Device<Mock>, kind: u8, request: u8, value: u16, idx: u16, len: u16) {
    let v = value.to_le_bytes();
    let i = idx.to_le_bytes();
    let l = len.to_le_bytes();
    event(
        d,
        Event::Setup([kind, request, v[0], v[1], i[0], i[1], l[0], l[1]]),
    );
}
fn event(d: &mut Device<Mock>, e: Event) {
    d.controller.events.push_back(e);
    d.service().unwrap();
}
fn ack(d: &mut Device<Mock>) {
    event(d, Event::InComplete(0x80));
}
fn configured() -> Device<Mock> {
    let mut d = device();
    setup(&mut d, 0, 5, 17, 0, 0);
    ack(&mut d);
    setup(&mut d, 0, 9, 1, 0, 0);
    ack(&mut d);
    setup(&mut d, 0x21, 0x22, 1, 0, 0);
    ack(&mut d);
    d.controller.writes.clear();
    d
}

#[test]
fn address_and_configuration_change_only_after_status_ack() {
    let mut d = device();
    setup(&mut d, 0, 5, 17, 0, 0);
    assert_eq!(d.controller.address, 0);
    ack(&mut d);
    assert_eq!(d.controller.address, 17);
    setup(&mut d, 0, 9, 1, 0, 0);
    assert!(!d.status().configured);
    ack(&mut d);
    assert_eq!(d.controller.configured, vec![0x81, 2, 0x82]);
    assert!(d.rx_armed);
}
#[test]
fn all_descriptor_request_lengths_are_bounded() {
    for requested in [1, 8, 18, 63, 64, 65, 84, 255, 256, 65535] {
        let mut d = device();
        setup(&mut d, 0x80, 6, 0x200, 0, requested);
        while matches!(d.control, Control::In { .. }) {
            ack(&mut d);
        }
        let bytes: Vec<_> = d
            .controller
            .writes
            .iter()
            .flat_map(|(_, v)| v.iter().copied())
            .collect();
        assert_eq!(
            bytes,
            &descriptors::CONFIG[..usize::from(requested).min(84)]
        );
        assert!(matches!(d.control, Control::StatusOut));
        event(&mut d, Event::Out(0));
        assert!(matches!(d.control, Control::Idle));
    }
}
#[test]
fn exact_packet_short_descriptor_needs_zero_length_termination() {
    let mut d = device();
    d.reply[..64].fill(7);
    d.send_reply(64, 255).unwrap();
    ack(&mut d);
    assert_eq!(
        d.controller
            .writes
            .iter()
            .map(|(_, v)| v.len())
            .collect::<Vec<_>>(),
        vec![64, 0]
    );
    ack(&mut d);
    assert!(matches!(d.control, Control::StatusOut));
}
#[test]
fn exact_requested_length_does_not_add_a_zero_packet() {
    let mut d = device();
    d.send_reply(64, 64).unwrap();
    ack(&mut d);
    assert_eq!(d.controller.writes.len(), 1);
}
#[test]
fn new_setup_cancels_pending_address_and_recovers_from_stall() {
    let mut d = device();
    setup(&mut d, 0, 5, 99, 0, 0);
    setup(&mut d, 0x80, 6, 0x100, 0, 18);
    ack(&mut d);
    assert_eq!(d.address, 0);
    setup(&mut d, 0x80, 6, 0xff00, 0, 64);
    assert!(d.controller.stalled(0x80));
    setup(&mut d, 0x80, 6, 0x100, 0, 18);
    assert!(!d.controller.stalled(0x80));
}
#[test]
fn invalid_control_fields_stall_instead_of_mutating_state() {
    for (kind, req, value, idx, len) in [
        (0, 5, 128, 0, 0),
        (0, 5, 2, 1, 0),
        (0, 5, 2, 0, 1),
        (0, 9, 2, 0, 0),
        (0x80, 6, 0x101, 0, 18),
        (0x80, 6, 0x100, 1, 18),
        (0x80, 6, 0x300, 9, 4),
        (0x21, 0x20, 0, 0, 8),
        (0x21, 0x22, 4, 0, 0),
        (0x21, 0x22, 1, 1, 0),
        (0x82, 0, 0, 0x192, 2),
        (2, 3, 0, 0x80, 0),
        (0x41, 1, 1, 2, 0),
        (0x41, 1, 0, 1, 0),
        (0xc0, 1, 1, 7, 166),
        (0xff, 0xff, 0xffff, 0xffff, 0xffff),
    ] {
        let mut d = configured();
        setup(&mut d, kind, req, value, idx, len);
        assert!(d.controller.stalled(0x80), "{kind:x}/{req:x}");
        assert!(d.reset.is_none());
    }
}
#[test]
fn vendor_reset_waits_for_ack_and_obeys_policy() {
    for (request, target) in [(1, ResetTarget::Bootloader), (2, ResetTarget::Application)] {
        let mut d = configured();
        setup(&mut d, 0x41, request, 0, 2, 0);
        assert!(d.reset.is_none());
        ack(&mut d);
        assert_eq!(d.reset, Some(target));
        let mut d = configured();
        d.identity.allow_reset = false;
        setup(&mut d, 0x41, request, 0, 2, 0);
        assert!(d.reset.is_none());
        assert!(d.controller.stalled(0x80));
    }
}
#[test]
fn twelve_hundred_baud_reset_requires_valid_data_and_status_ack() {
    let mut d = configured();
    setup(&mut d, 0x21, 0x20, 0, 0, 7);
    d.controller.out = vec![0xb0, 4, 0, 0, 0, 0, 8];
    event(&mut d, Event::Out(0));
    assert!(d.reset.is_none());
    ack(&mut d);
    assert_eq!(d.reset, Some(ResetTarget::Bootloader));
    let mut d = configured();
    setup(&mut d, 0x21, 0x20, 0, 0, 7);
    d.controller.out = vec![0xb0, 4, 0];
    event(&mut d, Event::Out(0));
    assert!(d.controller.stalled(0));
    assert!(d.reset.is_none());
}
#[test]
fn disabled_reset_allows_twelve_hundred_baud_as_normal_line_coding() {
    let mut d = configured();
    d.identity.allow_reset = false;
    setup(&mut d, 0x21, 0x20, 0, 0, 7);
    d.controller.out = vec![0xb0, 4, 0, 0, 0, 0, 8];
    event(&mut d, Event::Out(0));
    ack(&mut d);
    assert!(d.reset.is_none());
    assert_eq!(d.line_coding[0], 0xb0);
}
#[test]
fn receive_backpressure_never_overwrites_unread_bytes() {
    let mut d = configured();
    for byte in 0..8 {
        d.controller.out = vec![byte; 64];
        event(&mut d, Event::Out(2));
    }
    assert_eq!(d.rx.len, 512);
    assert!(!d.rx_armed);
    let mut out = [0; 63];
    d.rx.pop(&mut out);
    d.service().unwrap();
    assert!(!d.rx_armed);
    d.rx.pop(&mut [0]);
    d.service().unwrap();
    assert!(d.rx_armed);
    for byte in 1..8 {
        let mut packet = [0; 64];
        assert_eq!(d.rx.pop(&mut packet), 64);
        assert_eq!(packet, [byte; 64]);
    }
}
#[test]
fn transmit_packet_boundary_and_zero_packet_are_exact() {
    let mut d = configured();
    assert_eq!(d.tx.push(&[9; 512]), 512);
    assert_eq!(d.tx.push(&[8]), 0);
    d.service().unwrap();
    for _ in 0..8 {
        event(&mut d, Event::InComplete(0x82));
    }
    let sizes: Vec<_> = d
        .controller
        .writes
        .iter()
        .filter(|(ep, _)| *ep == 0x82)
        .map(|(_, v)| v.len())
        .collect();
    assert_eq!(sizes, vec![64, 64, 64, 64, 64, 64, 64, 64, 0]);
    assert_eq!(d.stats.transmitted, 512);
}
#[test]
fn reset_discards_old_data_and_pending_reset_request() {
    let mut d = configured();
    d.tx.push(&[7; 5]);
    d.rx.push(&[6; 3]);
    d.reset = Some(ResetTarget::Bootloader);
    event(&mut d, Event::Reset);
    assert!(!d.status().configured);
    assert_eq!(d.rx.len + d.tx.len, 0);
    assert!(d.reset.is_none());
    assert_eq!(d.stats.resets, 1);
}
#[test]
fn suspend_stops_transmit_resume_preserves_bytes() {
    let mut d = configured();
    event(&mut d, Event::Suspend);
    d.tx.push(&[5; 3]);
    d.service().unwrap();
    assert_eq!(d.tx.len, 3);
    event(&mut d, Event::Resume);
    assert_eq!(d.tx.len, 0);
    assert_eq!(d.controller.writes.last().unwrap().1, vec![5; 3]);
}
#[test]
fn endpoint_halt_clear_and_get_status_agree() {
    let mut d = configured();
    setup(&mut d, 2, 3, 0, 0x82, 0);
    ack(&mut d);
    assert!(d.controller.stalled(0x82));
    setup(&mut d, 0x82, 0, 0, 0x82, 2);
    assert_eq!(d.controller.writes.last().unwrap().1, vec![1, 0]);
    setup(&mut d, 2, 1, 0, 0x82, 0);
    ack(&mut d);
    assert!(!d.controller.stalled(0x82));
}
#[test]
fn unicode_strings_and_missing_serial_are_checked() {
    let mut d = device();
    d.identity.product = "USB 🦀";
    setup(&mut d, 0x80, 6, 0x302, 0x409, 255);
    let packet = &d.controller.writes.last().unwrap().1;
    assert_eq!(packet.len(), 14);
    assert_eq!(packet[0], 14);
    setup(&mut d, 0x80, 6, 0x303, 0x409, 255);
    assert!(d.controller.stalled(0x80));
}
#[test]
fn ring_wrap_preserves_order() {
    let mut ring = Ring::new();
    for _ in 0..50 {
        assert_eq!(ring.push(&[3; 500]), 500);
        assert_eq!(ring.pop(&mut [0; 499]), 499);
        assert_eq!(ring.push(&[4; 10]), 10);
        let mut out = [0; 11];
        assert_eq!(ring.pop(&mut out), 11);
        assert_eq!(out[0], 3);
        assert_eq!(&out[1..], &[4; 10]);
    }
}

#[test]
fn service_has_a_fixed_event_budget() {
    let mut d = configured();
    d.controller.events.extend([Event::Suspend; 40]);
    d.service().unwrap();
    assert_eq!(d.controller.events.len(), 24);
    d.service().unwrap();
    assert_eq!(d.controller.events.len(), 8);
}

#[test]
fn deterministic_setup_noise_always_allows_a_new_valid_request() {
    let mut d = configured();
    let mut random = 0x1720_0001u32;
    for _ in 0..10_000 {
        let mut bytes = [0; 8];
        for byte in &mut bytes {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            *byte = random as u8;
        }
        event(&mut d, Event::Setup(bytes));
        setup(&mut d, 0x80, 6, 0x100, 0, 18);
        assert!(!d.controller.stalled(0x80));
        assert_eq!(d.controller.writes.last().unwrap().1.len(), 18);
        ack(&mut d);
        event(&mut d, Event::Out(0));
        assert!(matches!(d.control, Control::Idle));
    }
}
