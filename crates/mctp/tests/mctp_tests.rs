use fstart_mctp::binding::{i3c, kcs, pcie_vdm, serial, smbus};
use fstart_mctp::control::{
    ControlCompletion, ControlResponder, SetEidOperation, encode_get_eid_req,
    encode_get_msg_types_req, encode_get_uuid_req, encode_get_version_req, encode_set_eid_req,
};
use fstart_mctp::{
    BASELINE_MTU, Endpoint, Error, MctpChannel, MctpHeader, MessageMeta, MessageType,
    PacketTransport, Packetizer, Reassembler,
};
use std::collections::VecDeque;

#[test]
fn header_roundtrip_and_validation() {
    let hdr = MctpHeader {
        hdr_ver: 1,
        dst_eid: 0x0A,
        src_eid: 0x08,
        som: true,
        eom: false,
        pkt_seq: 3,
        tag_owner: true,
        msg_tag: 5,
    };
    let bytes = hdr.to_bytes();
    assert_eq!(bytes, [0x01, 0x0A, 0x08, 0xBD]);
    assert_eq!(MctpHeader::from_bytes(bytes), Ok(hdr));
    assert_eq!(
        MctpHeader::from_bytes([0x02, 0x0A, 0x08, 0xC0]),
        Err(Error::InvalidHeader)
    );
    // Reserved upper nibble in byte 0 must be ignored on receipt (DSP0236 §5.2).
    assert!(MctpHeader::from_bytes([0x11, 0x0A, 0x08, 0xC0]).is_ok());
}

#[test]
fn fragmentation_reassembly_wraps_sequence_and_handles_interleaving() {
    // 400 bytes over 64B MTU = 7 packets (seq 0,1,2,3,0,1,2)
    let payload_a: Vec<u8> = (0..400).map(|i| (i & 0xFF) as u8).collect();
    let payload_b: Vec<u8> = (0..150).map(|i| ((i * 7) & 0xFF) as u8).collect();

    let mut pa = Packetizer::new(
        MessageMeta {
            dst_eid: 0x10,
            src_eid: 0x08,
            msg_type: MessageType::Pldm as u8,
            integrity_check: false,
            tag_owner: true,
            msg_tag: 1,
        },
        BASELINE_MTU,
        &payload_a,
    )
    .unwrap();
    let mut pb = Packetizer::new(
        MessageMeta {
            dst_eid: 0x10,
            src_eid: 0x09,
            msg_type: MessageType::Spdm as u8,
            integrity_check: false,
            tag_owner: true,
            msg_tag: 2,
        },
        BASELINE_MTU,
        &payload_b,
    )
    .unwrap();

    let mut pkts_a = Vec::new();
    let mut pkts_b = Vec::new();
    let mut scratch = [0u8; 128];
    while let Some(n) = pa.next_packet(&mut scratch).unwrap() {
        pkts_a.push(scratch[..n].to_vec());
    }
    while let Some(n) = pb.next_packet(&mut scratch).unwrap() {
        pkts_b.push(scratch[..n].to_vec());
    }
    assert_eq!(pkts_a.len(), 7);
    assert_eq!(pkts_b.len(), 3);

    let mut reasm = Reassembler::<512, 2>::new();
    let mut out = [0u8; 512];
    let mut got_a = None;
    let mut got_b = None;

    for i in 0..pkts_a.len().max(pkts_b.len()) {
        if let Some(p) = pkts_a.get(i)
            && let Some(meta) = reasm.push(p, 100, &mut out).unwrap()
        {
            got_a = Some((meta, out[..meta.len].to_vec()));
        }
        if let Some(p) = pkts_b.get(i)
            && let Some(meta) = reasm.push(p, 100, &mut out).unwrap()
        {
            got_b = Some((meta, out[..meta.len].to_vec()));
        }
    }

    let (ma, da) = got_a.unwrap();
    assert_eq!(ma.src_eid, 0x08);
    assert_eq!(ma.msg_type, MessageType::Pldm as u8);
    assert_eq!(da, payload_a);

    let (mb, db) = got_b.unwrap();
    assert_eq!(mb.src_eid, 0x09);
    assert_eq!(mb.msg_type, MessageType::Spdm as u8);
    assert_eq!(db, payload_b);
}

#[test]
fn reassembler_rejects_bad_sequence_and_expires_stale_slots() {
    let payload = [0xAA; 150];
    let mut p = Packetizer::new(
        MessageMeta {
            dst_eid: 0x10,
            src_eid: 0x08,
            msg_type: 1,
            integrity_check: false,
            tag_owner: true,
            msg_tag: 0,
        },
        BASELINE_MTU,
        &payload,
    )
    .unwrap();
    let mut pkts = Vec::new();
    let mut scratch = [0u8; 128];
    while let Some(n) = p.next_packet(&mut scratch).unwrap() {
        pkts.push(scratch[..n].to_vec());
    }
    assert_eq!(pkts.len(), 3);

    let mut reasm = Reassembler::<256, 1>::new().with_timeout_ms(100);
    let mut out = [0u8; 256];
    // Feed pkt 0 then skip pkt 1 -> BadSequence
    assert_eq!(reasm.push(&pkts[0], 10, &mut out), Ok(None));
    assert_eq!(reasm.push(&pkts[2], 20, &mut out), Err(Error::BadSequence));

    // Feed pkt 0 at t=10, then pkt 1 at t=200 (expired) -> BadSequence
    assert_eq!(reasm.push(&pkts[0], 10, &mut out), Ok(None));
    assert_eq!(reasm.push(&pkts[1], 200, &mut out), Err(Error::BadSequence));
}

#[test]
fn serial_binding_escapes_special_bytes_and_detects_corruption() {
    // Include 0x7E (flag) and 0x7D (escape) in payload.
    let pkt = [0x01, 0x0A, 0x08, 0xC8, 0x01, 0x7E, 0x7D, 0x5E, 0x5D, 0x00];
    let mut frame = [0u8; 64];
    let flen = serial::encode(&pkt, &mut frame).unwrap();
    assert_eq!(frame[0], serial::FRAME_FLAG);
    assert_eq!(frame[flen - 1], serial::FRAME_FLAG);

    let mut decoded = [0u8; 64];
    let dlen = serial::decode(&frame[..flen], &mut decoded).unwrap();
    assert_eq!(&decoded[..dlen], &pkt);

    // Corrupt a body byte -> BadChecksum
    let mut bad = frame[..flen].to_vec();
    bad[4] ^= 0x01;
    assert_eq!(serial::decode(&bad, &mut decoded), Err(Error::BadChecksum));
}

#[test]
fn smbus_binding_roundtrip_and_pec() {
    let pkt = [0x01, 0x10, 0x08, 0xC8, 0x00, 0x80, 0x02];
    let mut frame = [0u8; 64];
    let flen = smbus::encode(0x30, 0x10, &pkt, &mut frame).unwrap();
    assert_eq!(frame[0], 0x60);
    assert_eq!(frame[1], smbus::COMMAND_CODE);
    assert_eq!(frame[2], (1 + pkt.len()) as u8);
    assert_eq!(frame[3], 0x21); // (0x10 << 1) | 1

    let (meta, decoded) = smbus::decode(&frame[..flen]).unwrap();
    assert_eq!(meta.dst_addr, 0x30);
    assert_eq!(meta.src_addr, 0x10);
    assert_eq!(decoded, &pkt);

    // Corrupt PEC
    frame[flen - 1] ^= 0xFF;
    assert_eq!(smbus::decode(&frame[..flen]), Err(Error::BadChecksum));
}

#[test]
fn i3c_binding_write_and_read_pec() {
    let pkt = [0x01, 0x12, 0x08, 0xC8, 0x05, 0x12, 0x84, 0x00, 0x00];
    let mut frame = [0u8; 64];

    for is_read in [false, true] {
        let flen = i3c::encode(0x45, is_read, &pkt, &mut frame).unwrap();
        assert_eq!(i3c::decode(0x45, is_read, &frame[..flen]), Ok(&pkt[..]));
        // Wrong direction bit must fail PEC check.
        assert_eq!(
            i3c::decode(0x45, !is_read, &frame[..flen]),
            Err(Error::BadChecksum)
        );
    }
}

#[test]
fn kcs_binding_roundtrip_and_empty_response() {
    let pkt = [0x01, 0x0A, 0x08, 0xC8, 0x01, 0x80, 0x00, 0x02];
    let mut frame = [0u8; 64];
    let flen = kcs::encode(&pkt, &mut frame).unwrap();
    assert_eq!(frame[0], kcs::BINDING_ID);
    assert_eq!(frame[1], kcs::REVISION);
    assert_eq!(frame[2], pkt.len() as u8);
    assert_eq!(kcs::decode(&frame[..flen]), Ok(Some(&pkt[..])));

    frame[flen - 1] ^= 0x01;
    assert_eq!(kcs::decode(&frame[..flen]), Err(Error::BadChecksum));

    // DSP0254 §8 no-packet read response: [0xB0, 0x01, 0x00, 0xFF]
    let mut empty = [0u8; 8];
    let elen = kcs::encode_empty_response(&mut empty).unwrap();
    assert_eq!(&empty[..elen], &[0xB0, 0x01, 0x00, 0xFF]);
    assert_eq!(kcs::decode(&empty[..elen]), Ok(None));
}

#[test]
fn pcie_vdm_binding_all_pad_lengths_and_routings() {
    let routings = [
        pcie_vdm::PcieRouting::RouteToRc,
        pcie_vdm::PcieRouting::RouteById,
        pcie_vdm::PcieRouting::BroadcastFromRc,
    ];
    for routing in routings {
        for payload_len in 1..=8 {
            let mut pkt = vec![0x01, 0x20, 0x10, 0xC8];
            for i in 0..payload_len {
                pkt.push((0xA0 + i) as u8);
            }
            let mut tlp = [0u8; 64];
            let tlen = pcie_vdm::encode(routing, 0x0018, 0x0100, &pkt, &mut tlp).unwrap();
            assert_eq!(tlen % 4, 0);
            let (meta, decoded) = pcie_vdm::decode(&tlp[..tlen]).unwrap();
            assert_eq!(meta.routing, routing);
            assert_eq!(meta.requester_bdf, 0x0018);
            assert_eq!(
                meta.target_bdf,
                if routing == pcie_vdm::PcieRouting::RouteById {
                    0x0100
                } else {
                    0
                }
            );
            assert_eq!(decoded, &pkt[..]);
        }
    }
}

#[test]
fn control_protocol_full_discovery_flow() {
    let uuid = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    let mut responder = ControlResponder::new(
        0x00,
        uuid,
        &[
            MessageType::Pldm as u8,
            MessageType::Spdm as u8,
            MessageType::SecuredMsg as u8,
        ],
    );
    let mut req = [0u8; 32];
    let mut resp = [0u8; 32];

    // Reject illegal EID 0x00 and 0xFF
    let n = encode_set_eid_req(1, SetEidOperation::SetEid, 0x00, &mut req).unwrap();
    let r = responder.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(resp[2], ControlCompletion::InvalidData as u8);
    assert_eq!(r, 3);

    // Assign EID 0x0A
    let n = encode_set_eid_req(2, SetEidOperation::SetEid, 0x0A, &mut req).unwrap();
    let r = responder.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(r, 6);
    assert_eq!(resp[2], ControlCompletion::Success as u8);
    assert_eq!(resp[3], 0x00); // accepted
    assert_eq!(resp[4], 0x0A);
    assert_eq!(responder.eid(), 0x0A);

    // Get EID
    let n = encode_get_eid_req(3, &mut req).unwrap();
    let r = responder.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(r, 6);
    assert_eq!(resp[3], 0x0A);

    // Get UUID
    let n = encode_get_uuid_req(4, &mut req).unwrap();
    let r = responder.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(r, 19);
    assert_eq!(&resp[3..19], &uuid);

    // Get Version Support (base returns all 4 mandatory DSP0236 1.3.1 entries)
    let n = encode_get_version_req(5, 0xFF, &mut req).unwrap();
    let r = responder.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(r, 20);
    assert_eq!(resp[2], ControlCompletion::Success as u8);
    assert_eq!(resp[3], 4);

    // Get Message Type Support
    let n = encode_get_msg_types_req(6, &mut req).unwrap();
    let r = responder.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(r, 7);
    assert_eq!(resp[3], 3);
    assert_eq!(&resp[4..7], &[0x01, 0x05, 0x06]);
}

/// In-memory transport that actually runs one of the 5 wire binding encoders/decoders
/// on every packet, proving end-to-end Endpoint operation across all 5 bindings.
struct BoundLoopback {
    kind: fstart_mctp::TransportKind,
    queue: VecDeque<Vec<u8>>,
}

impl PacketTransport for BoundLoopback {
    fn mtu(&self) -> usize {
        BASELINE_MTU
    }

    fn send_packet(&mut self, pkt: &[u8]) -> Result<(), Error> {
        let mut wire = [0u8; 512];
        let n = match self.kind {
            fstart_mctp::TransportKind::Serial => serial::encode(pkt, &mut wire)?,
            fstart_mctp::TransportKind::Smbus => smbus::encode(0x30, 0x10, pkt, &mut wire)?,
            fstart_mctp::TransportKind::I3c => i3c::encode(0x45, false, pkt, &mut wire)?,
            fstart_mctp::TransportKind::Kcs => kcs::encode(pkt, &mut wire)?,
            fstart_mctp::TransportKind::PcieVdm => pcie_vdm::encode(
                pcie_vdm::PcieRouting::RouteById,
                0x0010,
                0x0020,
                pkt,
                &mut wire,
            )?,
        };
        self.queue.push_back(wire[..n].to_vec());
        Ok(())
    }

    fn recv_packet(&mut self, out: &mut [u8]) -> Result<usize, Error> {
        let wire = self.queue.pop_front().ok_or(Error::Timeout)?;
        match self.kind {
            fstart_mctp::TransportKind::Serial => serial::decode(&wire, out),
            fstart_mctp::TransportKind::Smbus => {
                let (_, pkt) = smbus::decode(&wire)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
            fstart_mctp::TransportKind::I3c => {
                let pkt = i3c::decode(0x45, false, &wire)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
            fstart_mctp::TransportKind::Kcs => {
                let pkt = kcs::decode(&wire)?.ok_or(Error::InvalidFrame)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
            fstart_mctp::TransportKind::PcieVdm => {
                let (_, pkt) = pcie_vdm::decode(&wire)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
        }
    }
}

#[test]
fn endpoint_multi_packet_roundtrip_over_all_five_bindings() {
    let payload: Vec<u8> = (0..220).map(|i| (i ^ 0x5A) as u8).collect();
    for kind in [
        fstart_mctp::TransportKind::Serial,
        fstart_mctp::TransportKind::Smbus,
        fstart_mctp::TransportKind::I3c,
        fstart_mctp::TransportKind::Kcs,
        fstart_mctp::TransportKind::PcieVdm,
    ] {
        let transport = BoundLoopback {
            kind,
            queue: VecDeque::new(),
        };
        let ctrl = ControlResponder::new(0x0A, [0x11; 16], &[MessageType::Pldm as u8]);
        let mut ep = Endpoint::<_, 512, 2>::new(transport, ctrl);
        let tag = ep
            .send_message(0x0A, MessageType::Pldm as u8, None, &payload)
            .unwrap();
        let mut rx_buf = [0u8; 512];
        let msg = ep.recv_message(&mut rx_buf).unwrap();
        assert_eq!(msg.src_eid, 0x0A, "{kind:?}");
        assert_eq!(msg.dst_eid, 0x0A, "{kind:?}");
        assert_eq!(msg.msg_type, MessageType::Pldm as u8, "{kind:?}");
        assert_eq!(msg.msg_tag, tag, "{kind:?}");
        assert!(msg.tag_owner);
        assert_eq!(msg.payload, &payload[..], "{kind:?}");
    }
}
