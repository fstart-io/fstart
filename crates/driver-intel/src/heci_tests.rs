// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use std::{cell::RefCell, collections::VecDeque, rc::Rc, vec, vec::Vec};

#[derive(Default)]
pub(crate) struct State {
    pub depth: u8,
    pub host_rp: u8,
    pub host_wp: u8,
    pub me_rp: u8,
    pub host_flags: u32,
    pub me_flags: u32,
    pub host_extra: u32,
    pub host_override: Option<u32>,
    pub me_override: Option<u32>,
    pub writes: Vec<u32>,
    pub tx_history: Vec<u32>,
    pub rx: VecDeque<u32>,
    pub incoming: VecDeque<Vec<u32>>,
    pub reply: VecDeque<Vec<u32>>,
    pub request_remaining: usize,
    pub request_complete: bool,
    pub pending_request: bool,
    pub auto_drain: bool,
    pub reset_works: bool,
    pub reset_assertions: usize,
    pub delays: u32,
    pub ready_after: Option<u32>,
    pub rx_after: Option<u32>,
    pub lose_ready_on_read: bool,
}
impl State {
    pub fn new(depth: u8) -> Self {
        Self {
            depth,
            host_flags: Csr::READY::SET.value,
            me_flags: Csr::READY::SET.value,
            auto_drain: true,
            reset_works: true,
            ..Self::default()
        }
    }
    fn host(&self) -> u32 {
        self.host_override.unwrap_or(
            self.host_flags
                | self.host_extra
                | u32::from(self.host_rp) << 8
                | u32::from(self.host_wp) << 16
                | u32::from(self.depth) << 24,
        )
    }
    fn me(&self) -> u32 {
        self.me_override.unwrap_or(
            self.me_flags
                | u32::from(self.me_rp) << 8
                | u32::from(self.me_rp.wrapping_add(self.rx.len() as u8)) << 16
                | u32::from(self.depth) << 24,
        )
    }
    pub fn load(&mut self, packet: Vec<u32>) {
        self.rx.extend(packet);
    }
}
pub(crate) struct Host(Rc<RefCell<State>>);
pub(crate) struct Me(Rc<RefCell<State>>);
pub(crate) struct Tx(Rc<RefCell<State>>);
pub(crate) struct Rx(Rc<RefCell<State>>);
impl Readable for Host {
    type T = u32;
    type R = Csr::Register;
    fn get(&self) -> u32 {
        self.0.borrow().host()
    }
}
impl Writeable for Host {
    type T = u32;
    type R = Csr::Register;
    fn set(&self, raw: u32) {
        let mut s = self.0.borrow_mut();
        s.writes.push(raw);
        let old_is = s.host_flags & Csr::IS::SET.value;
        s.host_flags = raw & (Csr::IE::SET.value | Csr::READY::SET.value | Csr::RESET::SET.value);
        if raw & Csr::IS::SET.value == 0 {
            s.host_flags |= old_is;
        }
        if raw & Csr::RESET::SET.value != 0 {
            s.reset_assertions += 1;
            if s.reset_works {
                s.host_flags &= !Csr::READY::SET.value;
                s.host_flags |= Csr::IS::SET.value;
                s.me_flags = Csr::READY::SET.value;
                s.host_rp = s.host_wp;
                s.rx.clear();
            }
        } else if raw & Csr::IG::SET.value != 0 {
            if s.auto_drain {
                s.host_rp = s.host_wp;
            }
            if s.pending_request && s.request_remaining == 0 && s.request_complete {
                s.pending_request = false;
                if let Some(packet) = s.reply.pop_front() {
                    s.load(packet);
                }
            } else if s.rx.is_empty()
                && let Some(packet) = s.incoming.pop_front()
            {
                s.load(packet);
            }
        }
    }
}
impl Readable for Me {
    type T = u32;
    type R = Csr::Register;
    fn get(&self) -> u32 {
        self.0.borrow().me()
    }
}
impl Writeable for Tx {
    type T = u32;
    type R = Fifo::Register;
    fn set(&self, raw: u32) {
        let mut s = self.0.borrow_mut();
        assert!(
            s.host_wp.wrapping_sub(s.host_rp) < s.depth,
            "host FIFO overrun"
        );
        s.tx_history.push(raw);
        s.host_wp = s.host_wp.wrapping_add(1);
        if s.request_remaining == 0 {
            s.request_remaining = ((raw >> 16) as usize & 511).div_ceil(4);
            s.request_complete = raw & (1 << 31) != 0;
            s.pending_request = true;
        } else {
            s.request_remaining -= 1;
        }
    }
}
impl Readable for Rx {
    type T = u32;
    type R = Fifo::Register;
    fn get(&self) -> u32 {
        let mut s = self.0.borrow_mut();
        let raw = s.rx.pop_front().expect("stale FIFO read");
        s.me_rp = s.me_rp.wrapping_add(1);
        if s.lose_ready_on_read {
            s.me_flags &= !Csr::READY::SET.value;
        }
        raw
    }
}
pub(crate) struct Delay(pub Rc<RefCell<State>>);
impl DelayNs for Delay {
    fn delay_ns(&mut self, _ns: u32) {
        let mut s = self.0.borrow_mut();
        s.delays += 1;
        if s.ready_after == Some(s.delays) {
            s.me_flags |= Csr::READY::SET.value;
        }
        if s.rx_after == Some(s.delays)
            && let Some(packet) = s.incoming.pop_front()
        {
            s.load(packet);
        }
    }
}
pub(crate) type TestTransport = Transport<Host, Me, Tx, Rx>;
pub(crate) fn rig(depth: u8) -> (Rc<RefCell<State>>, TestTransport, Delay) {
    let s = Rc::new(RefCell::new(State::new(depth)));
    let t = Transport::new(
        Host(s.clone()),
        Me(s.clone()),
        Tx(s.clone()),
        Rx(s.clone()),
        Budgets::new(2, 2, 8).unwrap(),
    );
    (s.clone(), t, Delay(s))
}
pub(crate) fn packet(bytes: &[u8], complete: bool, host: u8, client: u8) -> Vec<u32> {
    let mut out = vec![
        u32::from(client)
            | u32::from(host) << 8
            | (bytes.len() as u32) << 16
            | (u32::from(complete) << 31),
    ];
    for chunk in bytes.chunks(4) {
        let mut word = [0; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        out.push(u32::from_le_bytes(word));
    }
    out
}

#[test]
fn cse_filled_slots_wraps_at_u8_not_depth() {
    assert_eq!(
        Ring::decode(8 << 24 | 1 << 16 | 254 << 8),
        Ok(Ring {
            depth: 8,
            filled: 3
        })
    );
    assert_eq!(Ring::decode(0), Err(Error::Geometry));
    assert_eq!(Ring::decode(2 << 24 | 3 << 16), Err(Error::Geometry));
    assert_eq!(
        Ring::decode(255 << 24 | 255 << 16),
        Ok(Ring {
            depth: 255,
            filled: 255
        })
    );
}

#[test]
fn heci_send_fragmented_partial_zero_padded_and_ring_wrapped() {
    let (s, mut t, mut delay) = rig(3);
    s.borrow_mut().host_rp = 254;
    s.borrow_mut().host_wp = 254;
    t.send(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11], 2, 7, &mut delay)
        .unwrap();
    assert_eq!(
        s.borrow().tx_history,
        vec![0x0008_0207, 0x04030201, 0x08070605, 0x8003_0207, 0x000b0a09]
    );
    assert_eq!(s.borrow().host_wp, 3);
}

#[test]
fn heci_send_source_nine_bit_decoder_limit_is_507() {
    let (s, mut t, mut delay) = rig(255);
    t.send(&[0x55; 508], 0, 7, &mut delay).unwrap();
    assert_eq!(s.borrow().tx_history[0], 507 << 16 | 7);
    assert_eq!(s.borrow().tx_history[127], 0x00555555);
    assert_eq!(s.borrow().tx_history[128], 0x80010007);
    assert_eq!(s.borrow().tx_history[129], 0x55);
}

#[test]
fn heci_receive_fragmented_concatenation_preserves_canaries() {
    let (s, mut t, mut delay) = rig(4);
    s.borrow_mut().me_rp = 255;
    s.borrow_mut().load(packet(&[1, 2, 3, 4, 5], false, 0, 7));
    s.borrow_mut()
        .incoming
        .push_back(packet(&[6, 7, 8], true, 0, 7));
    let mut out = [0xee; 12];
    assert_eq!(t.receive(&mut out[..8], 0, 7, &mut delay), Ok(8));
    assert_eq!(out, [1, 2, 3, 4, 5, 6, 7, 8, 0xee, 0xee, 0xee, 0xee]);
    assert_eq!(s.borrow().me_rp, 4);
}

#[test]
fn heci_receive_exact_capacity_and_zero_complete() {
    let (s, mut t, mut delay) = rig(3);
    s.borrow_mut().load(packet(&[1, 2, 3, 4], true, 0, 7));
    assert_eq!(t.receive(&mut [0; 4], 0, 7, &mut delay), Ok(4));
    s.borrow_mut().load(packet(&[], true, 0, 7));
    assert_eq!(t.receive(&mut [0; 1], 0, 7, &mut delay), Ok(0));
}

#[test]
fn heci_receive_errors_are_not_stale_fifo_success() {
    let cases = [
        (vec![0x02010007], Error::Header),
        (packet(&[1], true, 1, 7), Error::Address),
        (packet(&[1], true, 0, 8), Error::Address),
        (packet(&[], false, 0, 7), Error::Incomplete),
        (
            vec![0x80050007],
            Error::Capacity {
                needed: 5,
                capacity: 4,
            },
        ),
        (vec![0x80040007], Error::Timeout),
    ];
    for (words, error) in cases {
        let (s, mut t, mut delay) = rig(4);
        s.borrow_mut().load(words);
        assert_eq!(t.receive(&mut [0; 4], 0, 7, &mut delay), Err(error));
        assert_eq!(
            t.receive(&mut [0; 4], 0, 7, &mut delay),
            Err(Error::NeedsReset)
        );
    }
}

#[test]
fn heci_receive_capacity_exhausted_without_complete_and_empty_final_fail() {
    let (s, mut t, mut delay) = rig(4);
    s.borrow_mut().load(packet(&[1], false, 0, 7));
    assert_eq!(
        t.receive(&mut [0; 1], 0, 7, &mut delay),
        Err(Error::Incomplete)
    );
    let (s, mut t, mut delay) = rig(4);
    s.borrow_mut().load(packet(&[1], false, 0, 7));
    s.borrow_mut().incoming.push_back(packet(&[], true, 0, 7));
    assert_eq!(
        t.receive(&mut [0; 4], 0, 7, &mut delay),
        Err(Error::Incomplete)
    );
}

#[test]
fn heci_receive_fragment_budget_bounded() {
    let (s, mut t, mut delay) = rig(3);
    s.borrow_mut().load(packet(&[1], false, 0, 7));
    for _ in 0..9 {
        s.borrow_mut().incoming.push_back(packet(&[1], false, 0, 7));
    }
    assert_eq!(
        t.receive(&mut [0; 16], 0, 7, &mut delay),
        Err(Error::Incomplete)
    );
    assert_eq!(s.borrow().me_rp, 16);
}

#[test]
fn heci_receive_loss_of_me_ready_never_publishes() {
    let (s, mut t, mut delay) = rig(4);
    s.borrow_mut().load(packet(&[1], true, 0, 7));
    s.borrow_mut().lose_ready_on_read = true;
    assert_eq!(
        t.receive(&mut [0; 4], 0, 7, &mut delay),
        Err(Error::NotReady)
    );
}

#[test]
fn heci_split_receive_preflight_failure_requires_explicit_reset() {
    for error in [Error::NotReady, Error::ResetRequested, Error::Geometry] {
        let (s, mut t, mut delay) = rig(4);
        t.send(&[1], 0, 7, &mut delay).unwrap();
        let transmitted = s.borrow().tx_history.clone();
        match error {
            Error::NotReady => s.borrow_mut().me_flags = 0,
            Error::ResetRequested => s.borrow_mut().me_flags |= Csr::RESET::SET.value,
            _ => s.borrow_mut().me_override = Some(Csr::READY::SET.value),
        }
        assert_eq!(t.receive(&mut [0; 4], 0, 7, &mut delay), Err(error));
        s.borrow_mut().me_flags = Csr::READY::SET.value;
        s.borrow_mut().me_override = None;
        assert_eq!(t.ready(), Err(Error::NeedsReset));
        assert_eq!(t.send(&[2], 0, 7, &mut delay), Err(Error::NeedsReset));
        assert_eq!(s.borrow().tx_history, transmitted);
        t.reset(&mut delay).unwrap();
        t.send(&[2], 0, 7, &mut delay).unwrap();
    }
}

#[test]
fn heci_initialize_waits_final_sample_and_preserves_w1c_and_neighbours() {
    let (s, mut t, mut delay) = rig(4);
    {
        let mut s = s.borrow_mut();
        s.host_flags = Csr::IS::SET.value | 1;
        s.host_extra = 1 << 6;
        s.me_flags = 0;
        s.ready_after = Some(2);
    }
    t.initialize(&mut delay).unwrap();
    assert_eq!(s.borrow().delays, 2);
    assert_eq!(s.borrow().writes.len(), 1);
    assert_eq!(s.borrow().writes[0] & 0x7f, 1 | 4 | 8 | 64);
    assert_ne!(
        s.borrow().host_flags & 2,
        0,
        "bring-up must not clear pending IS"
    );
    assert_eq!(s.borrow().reset_assertions, 0);
}

#[test]
fn heci_native_reset_assert_does_not_echo_w1c_but_release_acknowledges() {
    let (s, mut t, mut delay) = rig(4);
    s.borrow_mut().host_flags |= 2 | 1;
    s.borrow_mut().host_extra = 64;
    t.reset(&mut delay).unwrap();
    let s = s.borrow();
    assert_eq!(s.writes.len(), 2);
    assert_eq!(s.writes[0] & 0x7f, 1 | 4 | 8 | 16 | 64);
    assert_eq!(s.writes[1] & 0x7f, 1 | 2 | 4 | 8 | 64);
    assert_eq!(s.host_flags & 2, 0);
}

#[test]
fn heci_initialize_resets_only_when_me_requests_and_failed_reset_poisoned() {
    let (s, mut t, mut delay) = rig(4);
    s.borrow_mut().me_flags |= 16;
    t.initialize(&mut delay).unwrap();
    assert_eq!(s.borrow().reset_assertions, 1);
    let (s, mut t, mut delay) = rig(4);
    s.borrow_mut().reset_works = false;
    assert_eq!(t.reset(&mut delay), Err(Error::Timeout));
    assert_eq!(s.borrow().delays, 2);
    assert_eq!(t.send(&[1], 0, 7, &mut delay), Err(Error::NeedsReset));
}

#[test]
fn heci_send_receive_rejects_stale_before_any_write() {
    let (s, mut t, mut delay) = rig(4);
    s.borrow_mut().load(packet(&[1], true, 0, 7));
    assert_eq!(
        t.send_receive(&[1], &mut [0; 4], 0, 7, &mut delay),
        Err(Error::Stale)
    );
    assert!(s.borrow().writes.is_empty());
    assert!(s.borrow().tx_history.is_empty());
}

#[test]
fn heci_receive_waits_final_io_sample_and_rejects_impossible_fragment() {
    let (s, mut t, mut delay) = rig(4);
    s.borrow_mut()
        .incoming
        .push_back(packet(&[1, 2, 3], true, 0, 7));
    s.borrow_mut().rx_after = Some(2);
    let mut out = [0; 3];
    assert_eq!(t.receive(&mut out, 0, 7, &mut delay), Ok(3));
    assert_eq!(out, [1, 2, 3]);
    assert_eq!(s.borrow().delays, 2);
    let (s, mut t, mut delay) = rig(2);
    s.borrow_mut().load(vec![0x80080007]);
    assert_eq!(t.receive(&mut [0; 8], 0, 7, &mut delay), Err(Error::Header));
}
