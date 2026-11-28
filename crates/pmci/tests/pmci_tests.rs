use fstart_mctp::binding::{i3c, kcs, pcie_vdm, serial, smbus};
use fstart_mctp::control::ControlResponder;
use fstart_mctp::{
    BASELINE_MTU, Endpoint, Error as MctpError, MctpChannel, MessageType, PacketTransport,
    TransportKind,
};
use fstart_pldm::PldmResponder;
use fstart_pldm::base::encode_get_tid_req;
use fstart_pldm::platform::{NumericSensor, encode_get_sensor_reading_req};
use fstart_pmci::{SecuredMctpChannel, pldm_exchange};
use fstart_spdm::crypto::DeterministicRng;
use fstart_spdm::{SessionTransport, SpdmKeyPair, SpdmRequester, SpdmResponder};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

struct PipeTransport {
    kind: TransportKind,
    tx: Rc<RefCell<VecDeque<Vec<u8>>>>,
    rx: Rc<RefCell<VecDeque<Vec<u8>>>>,
}

impl PipeTransport {
    fn pair(kind: TransportKind) -> (Self, Self) {
        let a2b = Rc::new(RefCell::new(VecDeque::new()));
        let b2a = Rc::new(RefCell::new(VecDeque::new()));
        (
            Self {
                kind,
                tx: a2b.clone(),
                rx: b2a.clone(),
            },
            Self {
                kind,
                tx: b2a,
                rx: a2b,
            },
        )
    }
}

impl PacketTransport for PipeTransport {
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

#[test]
fn pldm_over_spdm_secured_mctp_all_five_transports() {
    for kind in [
        TransportKind::Serial,
        TransportKind::Smbus,
        TransportKind::I3c,
        TransportKind::Kcs,
        TransportKind::PcieVdm,
    ] {
        let (t_req, t_rsp) = PipeTransport::pair(kind);
        let mut ep_req = Endpoint::<_, 1024, 2>::new(
            t_req,
            ControlResponder::new(0x08, [1; 16], &[0x01, 0x05, 0x06]),
        );
        let mut ep_rsp = Endpoint::<_, 1024, 2>::new(
            t_rsp,
            ControlResponder::new(0x0A, [2; 16], &[0x01, 0x05, 0x06]),
        );

        let keypair = SpdmKeyPair::from_secret_bytes(&[0x77; 32]).unwrap();
        let pub_xy = *keypair.public_xy();
        let mut spdm_rsp =
            SpdmResponder::<512, 2>::new(SessionTransport::Mctp, keypair, b"cert").unwrap();
        let mut spdm_req = SpdmRequester::<512>::new(SessionTransport::Mctp, pub_xy);
        let mut rng_a = DeterministicRng::new(1);
        let mut rng_b = DeterministicRng::new(2);

        let mut tx = [0u8; 512];
        let mut rx = [0u8; 1024];
        let mut rsp_buf = [0u8; 512];

        let mut xchg = |ep_a: &mut Endpoint<PipeTransport, 1024, 2>,
                        ep_b: &mut Endpoint<PipeTransport, 1024, 2>,
                        sr: &mut SpdmResponder<512, 2>,
                        payload: &[u8]|
         -> Vec<u8> {
            ep_a.send_message(0x0A, MessageType::Spdm as u8, None, payload)
                .unwrap();
            let m = ep_b.recv_message(&mut rx).unwrap();
            let rlen = sr
                .handle_request(m.payload, &mut rsp_buf, &mut rng_b)
                .unwrap();
            ep_b.send_message(
                m.src_eid,
                MessageType::Spdm as u8,
                Some(m.msg_tag),
                &rsp_buf[..rlen],
            )
            .unwrap();
            ep_a.recv_message(&mut rx).unwrap().payload.to_vec()
        };

        let n = spdm_req.build_get_version(&mut tx).unwrap();
        let r = xchg(&mut ep_req, &mut ep_rsp, &mut spdm_rsp, &tx[..n]);
        spdm_req.handle_version(&r).unwrap();

        let n = spdm_req.build_get_capabilities(&mut tx).unwrap();
        let r = xchg(&mut ep_req, &mut ep_rsp, &mut spdm_rsp, &tx[..n]);
        spdm_req.handle_capabilities(&r).unwrap();

        let n = spdm_req.build_negotiate_algorithms(&mut tx).unwrap();
        let r = xchg(&mut ep_req, &mut ep_rsp, &mut spdm_rsp, &tx[..n]);
        spdm_req.handle_algorithms(&r).unwrap();

        let n = spdm_req.build_get_digests(&mut tx).unwrap();
        let r = xchg(&mut ep_req, &mut ep_rsp, &mut spdm_rsp, &tx[..n]);
        spdm_req.handle_digests(&r).unwrap();

        let n = spdm_req.build_key_exchange(&mut rng_a, &mut tx).unwrap();
        let r = xchg(&mut ep_req, &mut ep_rsp, &mut spdm_rsp, &tx[..n]);
        spdm_req.handle_key_exchange_rsp(&tx[..n], &r).unwrap();

        let n = spdm_req.build_secured_finish(&mut tx).unwrap();
        ep_req
            .send_message(0x0A, MessageType::SecuredMsg as u8, None, &tx[..n])
            .unwrap();
        let m = ep_rsp.recv_message(&mut rx).unwrap();
        let rlen = spdm_rsp
            .handle_secured_spdm(m.payload, &mut rsp_buf)
            .unwrap();
        ep_rsp
            .send_message(
                m.src_eid,
                MessageType::SecuredMsg as u8,
                Some(m.msg_tag),
                &rsp_buf[..rlen],
            )
            .unwrap();
        let fin_rsp = ep_req.recv_message(&mut rx).unwrap();
        spdm_req.handle_secured_finish_rsp(fin_rsp.payload).unwrap();

        let mut pldm = PldmResponder::<4, 2, 512>::new(0x33, 0xCAFE_BABE);
        pldm.platform
            .add_sensor(NumericSensor {
                sensor_id: 1,
                reading: 42_500,
            })
            .unwrap();

        let mut sec_req = SecuredMctpChannel {
            inner: &mut ep_req,
            session: spdm_req.session.as_mut().unwrap(),
        };
        let mut sec_rsp = SecuredMctpChannel {
            inner: &mut ep_rsp,
            session: spdm_rsp.session.as_mut().unwrap(),
        };

        let n = encode_get_tid_req(1, &mut tx).unwrap();
        sec_req
            .send_message(0x0A, MessageType::Pldm as u8, None, &tx[..n])
            .unwrap();
        let m = sec_rsp.recv_message(&mut rx).unwrap();
        let rlen = pldm.handle_request(m.payload, &mut rsp_buf).unwrap();
        sec_rsp
            .send_message(
                m.src_eid,
                MessageType::Pldm as u8,
                Some(m.msg_tag),
                &rsp_buf[..rlen],
            )
            .unwrap();
        let reply = sec_req.recv_message(&mut rx).unwrap();
        assert_eq!(reply.payload[4], 0x33, "{kind:?}");

        let n = encode_get_sensor_reading_req(2, 1, &mut tx).unwrap();
        sec_req
            .send_message(0x0A, MessageType::Pldm as u8, None, &tx[..n])
            .unwrap();
        let m = sec_rsp.recv_message(&mut rx).unwrap();
        let rlen = pldm.handle_request(m.payload, &mut rsp_buf).unwrap();
        sec_rsp
            .send_message(
                m.src_eid,
                MessageType::Pldm as u8,
                Some(m.msg_tag),
                &rsp_buf[..rlen],
            )
            .unwrap();
        let reply = sec_req.recv_message(&mut rx).unwrap();
        assert_eq!(
            u32::from_le_bytes(reply.payload[10..14].try_into().unwrap()),
            42_500,
            "{kind:?}"
        );
    }
}

#[test]
fn pldm_exchange_over_mctp_channel() {
    struct DirectResponder {
        pldm: PldmResponder<4, 2, 512>,
        pending: VecDeque<(u8, u8, u8, Vec<u8>)>,
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
                .push_back((dst_eid, msg_type, 3, resp[..rlen].to_vec()));
            Ok(3)
        }
        fn recv_message<'a>(
            &mut self,
            buf: &'a mut [u8],
        ) -> Result<fstart_mctp::ReceivedMessage<'a>, MctpError> {
            let (src_eid, msg_type, msg_tag, data) =
                self.pending.pop_front().ok_or(MctpError::Timeout)?;
            buf[..data.len()].copy_from_slice(&data);
            Ok(fstart_mctp::ReceivedMessage {
                src_eid,
                dst_eid: 0x08,
                msg_type,
                integrity_check: false,
                tag_owner: false,
                msg_tag,
                payload: &buf[..data.len()],
            })
        }
    }

    let mut ch = DirectResponder {
        pldm: PldmResponder::new(0x09, 0x1111),
        pending: VecDeque::new(),
    };
    let mut req = [0u8; 16];
    let n = encode_get_tid_req(7, &mut req).unwrap();
    let mut rx = [0u8; 128];
    let (cc, body) = pldm_exchange(&mut ch, 0x0A, &req[..n], &mut rx).unwrap();
    assert_eq!(cc, 0);
    assert_eq!(body, &[0x09]);
}
