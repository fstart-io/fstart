// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use std::{vec, vec::Vec};
struct Io {
    controls: [u8; 29],
    states: [u8; 29],
    writes: Vec<(u8, u8)>,
    state_reads: Vec<u8>,
}
impl Io {
    fn new() -> Self {
        Self {
            controls: [0xc6; 29],
            states: [3; 29],
            writes: Vec::new(),
            state_reads: Vec::new(),
        }
    }
}
impl AoacAccess for Io {
    fn control(&mut self, id: u8) -> u8 {
        self.controls[usize::from(id)]
    }
    fn set_control(&mut self, id: u8, value: u8) {
        self.writes.push((id, value));
        self.controls[usize::from(id)] = value;
    }
    fn state(&mut self, id: u8) -> u8 {
        self.state_reads.push(id);
        self.states[usize::from(id)]
    }
}
#[test]
fn aoac_all_devices_started_before_wait_with_whole_preflight() {
    let mut io = Io::new();
    power_devices(&mut io, &[17, 7, 8, 9, 27], 0, || panic!("already ready")).unwrap();
    assert_eq!(
        io.writes,
        vec![(17, 0xcd), (7, 0xcd), (8, 0xcd), (9, 0xcd), (27, 0xcd)]
    );
    assert_eq!(io.state_reads, [17, 7, 8, 9, 27]);
    let mut io = Io::new();
    assert_eq!(
        power_devices(&mut io, &[17, 6], 1, || {}),
        Err(Error::InvalidDevice)
    );
    assert!(io.writes.is_empty());
    io.controls[7] = 0xff;
    assert_eq!(
        power_devices(&mut io, &[17, 7], 1, || {}),
        Err(Error::Unavailable(7))
    );
    assert!(io.writes.is_empty());
}
#[test]
fn aoac_all_ones_is_not_ready_and_failed_programming_never_continues_waiting() {
    let mut io = Io::new();
    io.states[17] = 0xff;
    assert_eq!(
        power_devices(&mut io, &[17, 7], 1, || panic!("unavailable")),
        Err(Error::Unavailable(17))
    );
    assert_eq!(io.writes.len(), 2);
    assert_eq!(io.state_reads, [17]);
    let mut io = Io::new();
    io.states[17] = 0;
    let mut delays = 0;
    assert_eq!(
        power_devices(&mut io, &[17, 7], 2, || delays += 1),
        Err(Error::PowerTimeout(17))
    );
    assert_eq!(delays, 2);
    assert_eq!(io.state_reads, [17, 17, 17]);
}
