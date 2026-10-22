// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::gpio::*;
extern crate std;
use std::vec::Vec;
struct Mock {
    control: u32,
    writes: Vec<u32>,
    master: u32,
    routed: Option<(u8, Event)>,
    muxes: usize,
}
impl Access for Mock {
    fn read_control(&mut self, _: u8) -> u32 {
        self.control
    }
    fn write_control(&mut self, _: u8, value: u32) {
        self.writes.push(value);
        let pending = self.control & STATUS_MASK & !value;
        self.control = value & !STATUS_MASK | pending;
    }
    fn mux(&mut self, _: u8, _: u8) {
        self.muxes += 1;
    }
    fn master(&mut self, clear: u32, set: u32) {
        self.master = (self.master & !clear) | set;
    }
    fn route(&mut self, event: u8, config: Event) {
        self.routed = Some((event, config));
    }
}
fn mock() -> Mock {
    Mock {
        control: STATUS_MASK | (1 << 17) | (1 << 22),
        writes: Vec::new(),
        master: u32::MAX,
        routed: None,
        muxes: 0,
    }
}
#[test]
fn override_preserves_strength_clears_status_and_routes_sci() {
    let mut io = mock();
    configure(
        &mut io,
        &[Pad::output(9, true)],
        &[Pad::sci(9, Pull::Up, Trigger::LevelLow)],
    )
    .unwrap();
    assert_eq!(io.writes.len(), 2);
    assert_eq!(io.control & (1 << 17), 1 << 17); // drive/reserved bits untouched
    assert_eq!(io.control & STATUS_MASK, 0);
    assert_eq!(io.control & (3 << 22), 0); // output removed
    assert_eq!(io.control & (1 << 8), 1 << 8); // PAD_SCI uses LEVEL_HIGH at pad
    assert!(matches!(
        io.routed,
        Some((22, Event::Sci(Trigger::LevelLow)))
    ));
    assert_eq!(io.master & (1 << 28), 0);
    assert_eq!(io.master & (1 << 30), 1 << 30);
}
#[test]
fn validate_before_first_hardware_write() {
    let mut io = mock();
    assert_eq!(
        configure(&mut io, &[Pad::output(9, true), Pad::output(9, false)], &[]),
        Err(Error::DuplicatePad)
    );
    assert_eq!(io.muxes, 0);
    assert_eq!(io.writes.len(), 0);
}
#[test]
fn both_edge_sci_is_rejected_before_writes() {
    let mut io = mock();
    assert_eq!(
        configure(&mut io, &[Pad::sci(9, Pull::Up, Trigger::BothEdges)], &[]),
        Err(Error::BothEdgesSci)
    );
    assert_eq!(io.master, u32::MAX);
    assert_eq!(io.muxes, 0);
    assert!(io.writes.is_empty());
    assert!(io.routed.is_none());
}
#[test]
fn tsc_pstate_divisors() {
    use crate::picasso::tsc::pstate_mhz;
    assert_eq!(pstate_mhz((1 << 63) | 100 | (8 << 8)), Some(2500));
    assert_eq!(pstate_mhz((1 << 63) | 100 | (10 << 8)), Some(2000));
    assert_eq!(pstate_mhz((1 << 63) | 100), Some(4000));
    assert_eq!(pstate_mhz(100 | (8 << 8)), None);
}
