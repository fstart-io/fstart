use fstart_mctp::binding::{i3c, kcs, pcie_vdm, serial, smbus};
use fstart_mctp::control::ControlResponder;
use fstart_mctp::{
    BASELINE_MTU, Endpoint, Error as MctpError, MctpChannel, MessageType, PacketTransport,
    TransportKind,
};
use fstart_pldm::base::{
    crc32_ieee, encode_get_pldm_types_req, encode_get_pldm_version_req, encode_get_tid_req,
    encode_set_tid_req,
};
use fstart_pldm::fw_update::{
    FdState, encode_activate_firmware_req, encode_get_status_req, encode_pass_component_req,
    encode_request_update_req, encode_update_component_req,
};
use fstart_pldm::platform::{
    NumericSensor, StateEffecter, encode_get_pdr_repo_info_req, encode_get_sensor_reading_req,
    encode_set_state_effecter_req,
};
use fstart_pldm::{CompletionCode, PldmResponder, PldmType, exchange};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

struct PipeEndpoint {
    kind: TransportKind,
    tx: Rc<RefCell<VecDeque<Vec<u8>>>>,
    rx: Rc<RefCell<VecDeque<Vec<u8>>>>,
}

impl PipeEndpoint {
    fn pair(kind: TransportKind) -> (Self, Self) {
        let a_to_b = Rc::new(RefCell::new(VecDeque::new()));
        let b_to_a = Rc::new(RefCell::new(VecDeque::new()));
        (
            Self {
                kind,
                tx: a_to_b.clone(),
                rx: b_to_a.clone(),
            },
            Self {
                kind,
                tx: b_to_a,
                rx: a_to_b,
            },
        )
    }
}

impl PacketTransport for PipeEndpoint {
    fn mtu(&self) -> usize {
        BASELINE_MTU
    }

    fn send_packet(&mut self, pkt: &[u8]) -> Result<(), MctpError> {
        let mut wire = [0u8; 512];
        let n = match self.kind {
            TransportKind::Serial => serial::encode(pkt, &mut wire)?,
            TransportKind::Smbus => smbus::encode(0x30, 0x10, pkt, &mut wire)?,
            TransportKind::I3c => i3c::encode(0x45, false, pkt, &mut wire)?,
            TransportKind::Kcs => kcs::encode(pkt, &mut wire)?,
            TransportKind::PcieVdm => pcie_vdm::encode(
                pcie_vdm::PcieRouting::RouteById,
                0x0010,
                0x0020,
                pkt,
                &mut wire,
            )?,
        };
        self.tx.borrow_mut().push_back(wire[..n].to_vec());
        Ok(())
    }

    fn recv_packet(&mut self, out: &mut [u8]) -> Result<usize, MctpError> {
        let wire = self.rx.borrow_mut().pop_front().ok_or(MctpError::Timeout)?;
        match self.kind {
            TransportKind::Serial => serial::decode(&wire, out),
            TransportKind::Smbus => {
                let (_, pkt) = smbus::decode(&wire)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
            TransportKind::I3c => {
                let pkt = i3c::decode(0x45, false, &wire)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
            TransportKind::Kcs => {
                let pkt = kcs::decode(&wire)?.ok_or(MctpError::InvalidFrame)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
            TransportKind::PcieVdm => {
                let (_, pkt) = pcie_vdm::decode(&wire)?;
                out[..pkt.len()].copy_from_slice(pkt);
                Ok(pkt.len())
            }
        }
    }
}

fn serve_one<T: PacketTransport>(
    server_ep: &mut Endpoint<T, 1024, 2>,
    pldm: &mut PldmResponder<8, 4, 1024>,
) {
    let mut req_buf = [0u8; 1024];
    let mut resp_buf = [0u8; 1024];
    let msg = server_ep.recv_handling_control(&mut req_buf).unwrap();
    assert_eq!(msg.msg_type, MessageType::Pldm as u8);
    let rlen = pldm.handle_request(msg.payload, &mut resp_buf).unwrap();
    server_ep
        .send_message(
            msg.src_eid,
            MessageType::Pldm as u8,
            Some(msg.msg_tag),
            &resp_buf[..rlen],
        )
        .unwrap();
}

#[test]
fn pldm_base_and_platform_over_all_five_transports() {
    for kind in [
        TransportKind::Serial,
        TransportKind::Smbus,
        TransportKind::I3c,
        TransportKind::Kcs,
        TransportKind::PcieVdm,
    ] {
        let (t_bmc, t_host) = PipeEndpoint::pair(kind);
        let mut bmc = Endpoint::<_, 1024, 2>::new(
            t_bmc,
            ControlResponder::new(0x08, [1; 16], &[MessageType::Pldm as u8]).with_bus_owner(true),
        );
        let mut host = Endpoint::<_, 1024, 2>::new(
            t_host,
            ControlResponder::new(0x0A, [2; 16], &[MessageType::Pldm as u8]),
        );

        let mut pldm = PldmResponder::<8, 4, 1024>::new(0x01, 0x1234_5678);
        pldm.platform
            .add_sensor(NumericSensor {
                sensor_id: 42,
                reading: 55_000,
            })
            .unwrap();
        pldm.platform
            .add_effecter(StateEffecter {
                effecter_id: 7,
                state: 0,
            })
            .unwrap();

        let mut req = [0u8; 64];
        let mut rx = [0u8; 1024];

        // SetTID -> GetTID
        let n = encode_set_tid_req(1, 0x22, &mut req).unwrap();
        bmc.send_message(0x0A, MessageType::Pldm as u8, None, &req[..n])
            .unwrap();
        serve_one(&mut host, &mut pldm);
        let msg = bmc.recv_message(&mut rx).unwrap();
        assert_eq!(msg.payload[3], CompletionCode::Success as u8);

        let n = encode_get_tid_req(2, &mut req).unwrap();
        bmc.send_message(0x0A, MessageType::Pldm as u8, None, &req[..n])
            .unwrap();
        serve_one(&mut host, &mut pldm);
        let msg = bmc.recv_message(&mut rx).unwrap();
        assert_eq!(msg.payload[4], 0x22);

        // GetPLDMTypes + GetPLDMVersion
        let n = encode_get_pldm_types_req(3, &mut req).unwrap();
        bmc.send_message(0x0A, MessageType::Pldm as u8, None, &req[..n])
            .unwrap();
        serve_one(&mut host, &mut pldm);
        let msg = bmc.recv_message(&mut rx).unwrap();
        assert_eq!(msg.payload[4], (1 << 0) | (1 << 2) | (1 << 5));

        let n = encode_get_pldm_version_req(4, PldmType::FirmwareUpdate, &mut req).unwrap();
        bmc.send_message(0x0A, MessageType::Pldm as u8, None, &req[..n])
            .unwrap();
        serve_one(&mut host, &mut pldm);
        let msg = bmc.recv_message(&mut rx).unwrap();
        let ver = &msg.payload[9..13];
        let crc = u32::from_le_bytes(msg.payload[13..17].try_into().unwrap());
        assert_eq!(crc, crc32_ieee(ver));

        // Platform: GetSensorReading + SetStateEffecterStates + GetPDRRepositoryInfo
        let n = encode_get_sensor_reading_req(5, 42, &mut req).unwrap();
        bmc.send_message(0x0A, MessageType::Pldm as u8, None, &req[..n])
            .unwrap();
        serve_one(&mut host, &mut pldm);
        let msg = bmc.recv_message(&mut rx).unwrap();
        assert_eq!(
            u32::from_le_bytes(msg.payload[10..14].try_into().unwrap()),
            55_000
        );

        let n = encode_set_state_effecter_req(6, 7, 3, &mut req).unwrap();
        bmc.send_message(0x0A, MessageType::Pldm as u8, None, &req[..n])
            .unwrap();
        serve_one(&mut host, &mut pldm);
        let _ = bmc.recv_message(&mut rx).unwrap();
        assert_eq!(pldm.platform.effecter_state(7), Some(3));

        let n = encode_get_pdr_repo_info_req(7, &mut req).unwrap();
        bmc.send_message(0x0A, MessageType::Pldm as u8, None, &req[..n])
            .unwrap();
        serve_one(&mut host, &mut pldm);
        let msg = bmc.recv_message(&mut rx).unwrap();
        assert_eq!(
            u32::from_le_bytes(msg.payload[31..35].try_into().unwrap()),
            2
        );
    }
}

#[test]
fn pldm_firmware_update_full_state_machine() {
    let mut pldm = PldmResponder::<4, 2, 1024>::new(0x01, 0x0000_9E9E);
    let image: Vec<u8> = (0..300).map(|i| (i & 0xFF) as u8).collect();
    let mut req = [0u8; 128];
    let mut resp = [0u8; 128];

    // RequestUpdate -> LearnComponents
    let n = encode_request_update_req(1, 128, 1, 1, 0, b"1.0.0", &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(resp[3], CompletionCode::Success as u8);
    assert_eq!(pldm.fw_update.state(), FdState::LearnComponents);

    // PassComponentTable -> ReadyXfer
    let n = encode_pass_component_req(2, 0x000A, 0x0001, 1, b"1.0.1", &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(pldm.fw_update.state(), FdState::ReadyXfer);

    // UpdateComponent -> Download
    let n =
        encode_update_component_req(3, 0x000A, 0x0001, 1, image.len() as u32, b"1.0.1", &mut req)
            .unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(pldm.fw_update.state(), FdState::Download);

    // Pull firmware chunks via RequestFirmwareData (zero-padding past image end per DSP0267 §11.6)
    let mut inst = 1u8;
    while let Some((rlen, req_bytes)) = pldm.fw_update.next_fw_data_request(inst, &mut req).unwrap()
    {
        assert_eq!(rlen, 11);
        let offset = u32::from_le_bytes(req[3..7].try_into().unwrap()) as usize;
        let mut chunk = vec![0u8; req_bytes];
        if offset < image.len() {
            let copy_len = req_bytes.min(image.len() - offset);
            chunk[..copy_len].copy_from_slice(&image[offset..offset + copy_len]);
        }
        let done = pldm.fw_update.push_fw_data_chunk(&chunk).unwrap();
        inst = inst.wrapping_add(1) & 0x1F;
        if done {
            break;
        }
    }
    assert_eq!(
        pldm.fw_update
            .build_transfer_complete(10, &mut req)
            .unwrap(),
        4
    );
    assert_eq!(pldm.fw_update.state(), FdState::Verify);
    assert_eq!(
        pldm.fw_update
            .build_verify_complete(11, 0, &mut req)
            .unwrap(),
        4
    );
    assert_eq!(pldm.fw_update.state(), FdState::Apply);
    assert_eq!(
        pldm.fw_update.build_apply_complete(12, &mut req).unwrap(),
        6
    );
    assert_eq!(pldm.fw_update.state(), FdState::ReadyXfer);
    assert_eq!(pldm.fw_update.downloaded_image(), &image[..]);

    // ActivateFirmware -> Idle & activated
    let n = encode_activate_firmware_req(4, true, &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(resp[3], CompletionCode::Success as u8);
    assert!(pldm.fw_update.activated());
    assert_eq!(pldm.fw_update.state(), FdState::Idle);

    // GetStatus
    let n = encode_get_status_req(5, &mut req).unwrap();
    let r = pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(r, 14);
    assert_eq!(resp[4], FdState::Idle as u8);
}

#[test]
fn pldm_exchange_helper_matches_instance_id() {
    let (t_a, t_b) = PipeEndpoint::pair(TransportKind::Serial);
    let mut client = Endpoint::<_, 512, 2>::new(
        t_a,
        ControlResponder::new(0x08, [1; 16], &[MessageType::Pldm as u8]),
    );
    let mut server = Endpoint::<_, 512, 2>::new(
        t_b,
        ControlResponder::new(0x0A, [2; 16], &[MessageType::Pldm as u8]),
    );
    let mut pldm = PldmResponder::<4, 2, 512>::new(0x09, 0x1111);
    let mut req = [0u8; 16];
    let n = encode_get_tid_req(7, &mut req).unwrap();

    // Queue request, serve it, then call exchange? Instead test via manual pre-send or direct:
    // Send + serve + recv using exchange on a single-threaded responder-backed transport:
    struct DirectResponder {
        pldm: PldmResponder<4, 2, 512>,
        pending: VecDeque<(u8, u8, Vec<u8>)>,
    }
    impl MctpChannel for DirectResponder {
        fn send_message(
            &mut self,
            dst_eid: u8,
            msg_type: u8,
            _reply_tag: Option<u8>,
            payload: &[u8],
        ) -> Result<u8, MctpError> {
            let mut resp = [0u8; 256];
            let rlen = self.pldm.handle_request(payload, &mut resp).unwrap();
            self.pending
                .push_back((dst_eid, msg_type, resp[..rlen].to_vec()));
            Ok(0)
        }
        fn recv_message<'a>(
            &mut self,
            buf: &'a mut [u8],
        ) -> Result<fstart_mctp::ReceivedMessage<'a>, MctpError> {
            let (src_eid, msg_type, data) = self.pending.pop_front().ok_or(MctpError::Timeout)?;
            buf[..data.len()].copy_from_slice(&data);
            Ok(fstart_mctp::ReceivedMessage {
                src_eid,
                dst_eid: 0x08,
                msg_type,
                integrity_check: false,
                tag_owner: false,
                msg_tag: 0,
                payload: &buf[..data.len()],
            })
        }
    }

    let _ = (&mut client, &mut server, &mut pldm);
    let mut ch = DirectResponder {
        pldm: PldmResponder::new(0x09, 0x1111),
        pending: VecDeque::new(),
    };
    let mut rx = [0u8; 128];
    let (cc, body) = exchange(&mut ch, 0x0A, &req[..n], &mut rx).unwrap();
    assert_eq!(cc, 0);
    assert_eq!(body, &[0x09]);
}
