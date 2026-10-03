// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use std::{collections::VecDeque, vec::Vec};
struct Timer(u64);
impl Clock for Timer {
    fn now_us(&mut self) -> u64 {
        self.0
    }
    fn delay_us(&mut self, _: u32) {
        self.0 += 100000;
    }
}
struct Ports {
    packet: [u8; 256],
    writes: Vec<(u16, u8)>,
    replies: VecDeque<Vec<u8>>,
    requests: Vec<Vec<u8>>,
    corrupt: bool,
    timeout: bool,
    submitted: bool,
    result: u8,
}
impl Ports {
    fn new(replies: &[&[u8]]) -> Self {
        Self {
            packet: [0; 256],
            writes: Vec::new(),
            replies: replies.iter().map(|data| data.to_vec()).collect(),
            requests: Vec::new(),
            corrupt: false,
            timeout: false,
            submitted: false,
            result: 0,
        }
    }
}
impl Io for Ports {
    fn read8(&mut self, port: u16) -> u8 {
        match port {
            STATUS => {
                if self.submitted && self.timeout {
                    6
                } else {
                    0
                }
            }
            DATA => self.result,
            0x920 => b'E',
            0x921 => b'C',
            0x927 => 2,
            0x800..=0x8ff => self.packet[usize::from(port - PACKET)],
            _ => 0xff,
        }
    }
    fn write8(&mut self, port: u16, value: u8) {
        self.writes.push((port, value));
        if (0x800..=0x8ff).contains(&port) {
            self.packet[usize::from(port - PACKET)] = value;
        }
        if port == STATUS {
            assert_eq!(value, 0xda);
            self.submitted = true;
            let length = usize::from(u16::from_le_bytes([self.packet[6], self.packet[7]]));
            self.requests.push(self.packet[..8 + length].to_vec());
            let data = self.replies.pop_front().unwrap_or_default();
            self.packet.fill(0);
            self.packet[0] = 3;
            self.packet[4..6].copy_from_slice(&(data.len() as u16).to_le_bytes());
            self.packet[8..8 + data.len()].copy_from_slice(&data);
            self.packet[1] = checksum(&self.packet[..8 + data.len()]).wrapping_neg();
            if self.corrupt {
                self.packet[1] ^= 1;
            }
        }
    }
}
#[test]
fn cbi_short_integer_zero_and_wire_checksum_trigger_order() {
    let mut ec = Ec::discover(Ports::new(&[&[2], &[0, 0, 0, 0]])).unwrap();
    assert_eq!(ec.cbi_u32(CbiTag::BoardVersion, &mut Timer(0)), Ok(2));
    assert_eq!(ec.cbi_u32(CbiTag::FirmwareConfig, &mut Timer(0)), Ok(0));
    assert!(!ec.is_poisoned());
    for (i, request) in ec.io.requests.iter().enumerate() {
        assert_eq!(
            &request[..8],
            &[3, if i == 0 { 0xd5 } else { 0xcf }, 0x1f, 1, 0, 0, 8, 0]
        );
        assert_eq!(checksum(request), 0);
        assert_eq!(request[8], if i == 0 { 0 } else { 6 });
        assert!(request[9..].iter().all(|&byte| byte == 0));
    }
    for transaction in ec.io.writes.chunks_exact(17) {
        assert_eq!(
            transaction
                .iter()
                .map(|&(port, _)| port)
                .collect::<Vec<_>>(),
            (0x808..=0x80f)
                .chain(0x800..=0x807)
                .chain(core::iter::once(STATUS))
                .collect::<Vec<_>>()
        );
    }
}
#[test]
fn legacy_board_version_has_its_own_command_and_exact_two_byte_response() {
    let mut ec = Ec::discover(Ports::new(&[&[0x34, 0x12]])).unwrap();
    assert_eq!(ec.board_version(&mut Timer(0)), Ok(0x1234));
    assert_eq!(ec.io.requests[0], [3, 0xf7, 6, 0, 0, 0, 0, 0]);
    for data in [&[][..], &[3][..], &[3, 0, 0][..]] {
        let mut ec = Ec::discover(Ports::new(&[data])).unwrap();
        assert_eq!(ec.board_version(&mut Timer(0)), Err(Error::Length));
    }
}
#[test]
fn malformed_or_pending_response_never_publishes_or_retries() {
    let mut ports = Ports::new(&[&[0x55]]);
    ports.corrupt = true;
    let mut ec = Ec::discover(ports).unwrap();
    let mut output = [0xaa; 4];
    assert_eq!(
        ec.command(0x11f, 0, &[], &mut output, &mut Timer(0)),
        Err(Error::Checksum)
    );
    assert_eq!(output, [0xaa; 4]);
    assert!(ec.is_poisoned());
    let writes = ec.io.writes.len();
    assert_eq!(
        ec.command(0x11f, 0, &[], &mut output, &mut Timer(0)),
        Err(Error::Poisoned)
    );
    assert_eq!(ec.io.writes.len(), writes);
    let mut ports = Ports::new(&[&[0x55]]);
    ports.timeout = true;
    let mut ec = Ec::discover(ports).unwrap();
    assert_eq!(
        ec.command(0x11f, 0, &[], &mut output, &mut Timer(0)),
        Err(Error::Timeout)
    );
    assert!(ec.is_poisoned());
    assert_eq!(ec.io.requests.len(), 1);
    let mut ports = Ports::new(&[]);
    ports.result = 8;
    let mut ec = Ec::discover(ports).unwrap();
    assert_eq!(
        ec.command(0x11f, 0, &[], &mut output, &mut Timer(0)),
        Err(Error::Rejected(8))
    );
    assert!(ec.is_poisoned());
}
#[test]
fn lengths_preflight_and_oversized_response_are_bounded() {
    let mut ec = Ec::discover(Ports::new(&[&[1, 2, 3, 4, 5]])).unwrap();
    assert_eq!(
        ec.command(1, 0, &[0; 249], &mut [0; 4], &mut Timer(0)),
        Err(Error::Length)
    );
    assert!(ec.io.writes.is_empty());
    let mut output = [0xaa; 4];
    assert_eq!(
        ec.command(1, 0, &[], &mut output, &mut Timer(0)),
        Err(Error::Length)
    );
    assert_eq!(output, [0xaa; 4]);
    assert!(ec.is_poisoned());
}
#[test]
fn valid_header_result_is_not_ignored_when_legacy_status_claims_success() {
    // Exercise the parser using a port proxy that returns a checksummed error.
    struct Rejected(Ports);
    impl Io for Rejected {
        fn read8(&mut self, port: u16) -> u8 {
            self.0.read8(port)
        }
        fn write8(&mut self, port: u16, value: u8) {
            self.0.write8(port, value);
            if port == STATUS {
                self.0.packet[2] = 4;
                self.0.packet[1] = 0;
                self.0.packet[1] = checksum(&self.0.packet[..8]).wrapping_neg();
            }
        }
    }
    let mut ec = Ec::discover(Rejected(Ports::new(&[&[]]))).unwrap();
    assert_eq!(
        ec.command(1, 0, &[], &mut [], &mut Timer(0)),
        Err(Error::Rejected(4))
    );
    assert!(!ec.is_poisoned());
}
