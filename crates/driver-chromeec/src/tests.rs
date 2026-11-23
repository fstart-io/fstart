// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use fstart_core::services::ManualTimer;
use std::{collections::VecDeque, vec::Vec};
struct Ports {
    packet: [u8; 256],
    writes: Vec<(u16, u8)>,
    replies: VecDeque<Vec<u8>>,
    requests: Vec<Vec<u8>>,
    corrupt: bool,
    timeout: bool,
    submitted: bool,
    result: u8,
    flags: u8,
    stale: bool,
    status_reads: usize,
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
            flags: 2,
            stale: false,
            status_reads: 0,
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
                    self.stale.into()
                }
            }
            DATA => {
                self.status_reads += 1;
                self.stale = false;
                self.result
            }
            0x920 => b'E',
            0x921 => b'C',
            0x927 => self.flags,
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
            if self.flags == 1 {
                let n = usize::from(self.packet[2]);
                let req = &self.packet[..4 + n];
                assert_eq!(req[0], 1);
                assert_eq!(
                    req[3],
                    value
                        .wrapping_add(checksum(&req[..3]))
                        .wrapping_add(checksum(&req[4..]))
                );
                self.requests.push(req.to_vec());
                let data = self.replies.pop_front().unwrap_or_default();
                self.packet[0] = 2;
                self.packet[2] = data.len() as u8;
                self.packet[3] = value
                    .wrapping_add(checksum(&self.packet[..3]))
                    .wrapping_add(checksum(&data))
                    ^ u8::from(self.corrupt);
                self.packet[4..4 + data.len()].copy_from_slice(&data);
                self.submitted = true;
                return;
            }
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
    assert_eq!(ec.cbi_u32(CbiTag::BoardVersion, &ManualTimer::new()), Ok(2));
    assert_eq!(
        ec.cbi_u32(CbiTag::FirmwareConfig, &ManualTimer::new()),
        Ok(0)
    );
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
    for transaction in ec.io.writes.as_chunks::<17>().0 {
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
    assert_eq!(ec.board_version(&ManualTimer::new()), Ok(0x1234));
    assert_eq!(ec.io.requests[0], [3, 0xf7, 6, 0, 0, 0, 0, 0]);
    for data in [&[][..], &[3][..], &[3, 0, 0][..]] {
        let mut ec = Ec::discover(Ports::new(&[data])).unwrap();
        assert_eq!(ec.board_version(&ManualTimer::new()), Err(Error::Length));
    }
}
#[test]
fn malformed_or_pending_response_never_publishes_or_retries() {
    let mut ports = Ports::new(&[&[0x55]]);
    ports.corrupt = true;
    let mut ec = Ec::discover(ports).unwrap();
    let mut output = [0xaa; 4];
    assert_eq!(
        ec.command(0x11f, 0, &[], &mut output, &ManualTimer::new()),
        Err(Error::Checksum)
    );
    assert_eq!(output, [0xaa; 4]);
    assert!(ec.is_poisoned());
    let writes = ec.io.writes.len();
    assert_eq!(
        ec.command(0x11f, 0, &[], &mut output, &ManualTimer::new()),
        Err(Error::Poisoned)
    );
    assert_eq!(ec.io.writes.len(), writes);
    let mut ports = Ports::new(&[&[0x55]]);
    ports.timeout = true;
    let mut ec = Ec::discover(ports).unwrap();
    assert_eq!(
        ec.command(0x11f, 0, &[], &mut output, &ManualTimer::new()),
        Err(Error::Timeout)
    );
    assert!(ec.is_poisoned());
    assert_eq!(ec.io.requests.len(), 1);
    let mut ports = Ports::new(&[]);
    ports.result = 8;
    let mut ec = Ec::discover(ports).unwrap();
    assert_eq!(
        ec.command(0x11f, 0, &[], &mut output, &ManualTimer::new()),
        Err(Error::Rejected(8))
    );
    assert!(ec.is_poisoned());
}
#[test]
fn lengths_preflight_and_oversized_response_are_bounded() {
    let mut ec = Ec::discover(Ports::new(&[&[1, 2, 3, 4, 5]])).unwrap();
    assert_eq!(
        ec.command(1, 0, &[0; 249], &mut [0; 4], &ManualTimer::new()),
        Err(Error::Length)
    );
    assert!(ec.io.writes.is_empty());
    let mut output = [0xaa; 4];
    assert_eq!(
        ec.command(1, 0, &[], &mut output, &ManualTimer::new()),
        Err(Error::Length)
    );
    assert_eq!(output, [0xaa; 4]);
    assert!(ec.is_poisoned());
}
#[test]
fn v2_board_version_mask_and_charge_trigger_last_without_replay() {
    let mut ports = Ports::new(&[&[42, 0], &[], &[], &[77, 0]]);
    ports.flags = 1;
    let mut ec = Ec::discover(ports).unwrap();
    let timer = ManualTimer::new();
    assert_eq!(ec.board_version(&timer), Ok(42));
    ec.set_sci_mask(0x1234, &timer).unwrap();
    ec.short_disable_usb_charging(1, |_| unreachable!())
        .unwrap();
    assert_eq!(
        ec.io.requests,
        [
            &[1, 0, 0, 7][..],
            &[1, 0, 4, 0xd6, 0x34, 0x12, 0, 0],
            &[1, 0, 2, 0x34, 1, 0]
        ]
    );
    ec.io.corrupt = true;
    assert_eq!(ec.board_version(&timer), Err(Error::Checksum));
    assert_eq!(ec.board_version(&timer), Err(Error::Poisoned));
    assert_eq!(
        ec.io
            .writes
            .iter()
            .filter(|(p, _)| *p == STATUS)
            .map(|(_, v)| *v)
            .collect::<Vec<_>>(),
        [6, 0x8b, 0x30, 6]
    );
}

#[test]
fn unified_masks_require_feature_and_command_and_program_lazy_s5_then_s3() {
    for (flag, version, expected) in [
        (0, 1, EventMasks::Legacy),
        (1, 0, EventMasks::Legacy),
        (1, 1, EventMasks::Unified),
    ] {
        let mut ec = Ec::discover(Ports::new(&[
            &[0, 0, 0, 0, flag, 0, 0, 0],
            &[version, 0, 0, 0],
            &[],
            &[],
        ]))
        .unwrap();
        let timer = ManualTimer::new();
        let mode = EventMasks::detect(&mut ec, &timer).unwrap();
        assert_eq!(mode, expected);
        assert_eq!(mode.lazy(&mut ec, 6, 0x1006, &timer), [Ok(()); 2]);
        if mode == EventMasks::Unified {
            assert_eq!(&ec.io.requests[1][2..], &[8, 0, 1, 0, 2, 0, 0xa4, 0]);
            assert_eq!(
                &ec.io.requests[2][8..],
                &[1, 8, 0, 0, 6, 0, 0, 0, 0, 0, 0, 0]
            );
            assert_eq!(
                &ec.io.requests[3][8..],
                &[1, 7, 0, 0, 6, 16, 0, 0, 0, 0, 0, 0]
            );
        } else {
            assert_eq!(ec.io.requests.len(), 1 + usize::from(flag));
        }
    }
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
        ec.command(1, 0, &[], &mut [], &ManualTimer::new()),
        Err(Error::Rejected(4))
    );
    assert!(!ec.is_poisoned());
}

#[test]
fn smm_writes_are_bounded_and_never_wait_out_an_existing_transaction() {
    let mut ec = Ec::discover(Ports::new(&[])).unwrap();
    ec.short_set_mask(LegacyMask::Smi, 1, |_| unreachable!())
        .unwrap();
    ec.short_disable_usb_charging(1, |_| unreachable!())
        .unwrap();
    assert_eq!(&ec.io.requests[0][2..], &[0x8a, 0, 0, 0, 4, 0, 1, 0, 0, 0]);
    assert_eq!(&ec.io.requests[1][2..], &[0x30, 0, 0, 0, 2, 0, 1, 0]);
    ec.io.timeout = true;
    assert_eq!(
        ec.short_set_mask(LegacyMask::Wake, 2, |_| unreachable!()),
        Err(Error::Unavailable)
    );
    assert_eq!(ec.io.requests.len(), 2);
    ec.io.submitted = false;
    let mut us = 0;
    assert_eq!(
        ec.short_set_mask(LegacyMask::Wake, 2, |delay| us += delay),
        Err(Error::Timeout)
    );
    assert_eq!(us, 1000);
    assert!(ec.is_poisoned());
    assert_eq!(ec.io.status_reads, 2);
    ec.io.timeout = false;
    ec.io.stale = true; // Late output from the third (timed-out) command.
    ec.finish_owned(|_| unreachable!()).unwrap();
    ec.finish_owned(|_| unreachable!()).unwrap();
    assert_eq!((ec.io.status_reads, ec.io.requests.len()), (3, 3));
    assert!(ec.is_poisoned()); // discard a late result, never retry the mutation
}

#[test]
fn pending_output_is_neither_read_nor_overwritten_in_smm() {
    struct Pending;
    impl Io for Pending {
        fn read8(&mut self, port: u16) -> u8 {
            assert!(matches!(port, STATUS | 0x66), "read another owner's byte");
            TO_HOST
        }
        fn write8(&mut self, _: u16, _: u8) {
            panic!("wrote over pending output")
        }
    }
    let mut ec = Ec {
        io: Pending,
        host_pending: false,
        v3: true,
        query_pending: false,
        poisoned: false,
    };
    assert_eq!(ec.query_event(|_| unreachable!()), Err(Error::Unavailable));
    assert_eq!(
        ec.short_set_mask(LegacyMask::Smi, 0, |_| unreachable!()),
        Err(Error::Unavailable)
    );
    ec.finish_owned(|_| unreachable!()).unwrap();
    let mut ports = Ports::new(&[&[3, 0]]);
    ports.stale = true;
    let mut bsp = Ec::discover(ports).unwrap();
    assert_eq!(bsp.board_version(&ManualTimer::new()), Ok(3));
}

#[test]
fn acpi_query_requires_idle_then_output_not_just_an_empty_input_buffer() {
    struct AcpiPorts {
        status: VecDeque<u8>,
        commands: Vec<u8>,
    }
    impl Io for AcpiPorts {
        fn read8(&mut self, port: u16) -> u8 {
            match port {
                0x66 => self.status.pop_front().unwrap(),
                0x62 => HostEvent::LidClosed as u8,
                other => panic!("unexpected read {other:x}"),
            }
        }
        fn write8(&mut self, port: u16, value: u8) {
            assert_eq!(port, 0x66);
            self.commands.push(value);
        }
    }
    let mut ec = Ec {
        v3: true,
        query_pending: false,
        host_pending: false,
        io: AcpiPorts {
            status: [0, 4, 0, 1].into(),
            commands: Vec::new(),
        },
        poisoned: false,
    };
    let mut us = 0;
    assert_eq!(ec.query_event(|delay| us += delay), Ok(1));
    assert_eq!(us, 20);
    assert_eq!(ec.io.commands, [0x84]);
    ec.io.status.push_back(0xff);
    assert_eq!(ec.query_event(|_| unreachable!()), Err(Error::Unavailable));
    assert_eq!(ec.io.commands.len(), 1);
    // A late output after QUERY must be consumed once, without issuing a
    // second QUERY or treating an unrelated OBF as our own on next entry.
    ec.io.status.extend(core::iter::repeat_n(0, 101));
    assert_eq!(ec.query_event(|_| {}), Err(Error::Timeout));
    ec.io.status.extend([4, 1]);
    assert_eq!(ec.finish_owned(|_| {}), Ok(()));
    assert!(!ec.query_pending());
    assert!(ec.io.status.is_empty());
    assert_eq!(ec.io.commands, [0x84, 0x84]);
}

#[test]
fn mkbp_drain_accepts_unavailable_or_unsupported_and_bounds_a_stuck_queue() {
    for status in [1, 9] {
        let mut p = Ports::new(&[]);
        p.result = status;
        let mut ec = Ec::discover(p).unwrap();
        ec.drain_mkbp(|_| unreachable!()).unwrap();
        assert_eq!(&ec.io.requests[0][2..4], &[0x67, 0]);
    }
    let mut ec = Ec::discover(Ports::new(&[&[1u8][..]; 34])).unwrap();
    assert_eq!(ec.drain_mkbp(|_| unreachable!()), Err(Error::Timeout));
    assert_eq!(ec.io.requests.len(), 33);
}

#[test]
fn event_masks_are_their_own_commands_with_a_32_bit_mask() {
    let mut ec = Ec::discover(Ports::new(&[&[], &[0; 4]])).unwrap();
    let timer = ManualTimer::new();
    let mask = HostEvent::LidClosed.mask() | HostEvent::ModeChange.mask();
    assert_eq!(mask, 1 | 1 << 28);
    ec.set_sci_mask(mask, &timer).unwrap();
    ec.set_wake_mask(0, &timer).unwrap();
    let requests = &ec.io.requests;
    assert_eq!(&requests[0][2..4], &0x8bu16.to_le_bytes());
    assert_eq!(&requests[0][6..], &[4, 0, 1, 0, 0, 0x10]);
    assert_eq!(&requests[1][2..4], &0x8eu16.to_le_bytes());
    assert_eq!(&requests[1][8..], &[0; 4]);
}
