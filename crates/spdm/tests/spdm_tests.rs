use fstart_spdm::crypto::{DeterministicRng, sha256};
use fstart_spdm::{
    MeasurementBlock, SessionTransport, SpdmCode, SpdmKeyPair, SpdmRequester, SpdmResponder,
};

fn run_full_spdm_session(transport: SessionTransport) {
    let secret = [0x42u8; 32];
    let keypair = SpdmKeyPair::from_secret_bytes(&secret).unwrap();
    let pub_xy = *keypair.public_xy();
    let fake_cert = b"fstart-test-x509-leaf-certificate-payload";

    let mut rsp = SpdmResponder::<512, 4>::new(transport, keypair, fake_cert).unwrap();
    rsp.add_measurement(MeasurementBlock {
        index: 1,
        value_type: 0x00,
        digest: sha256(b"bootblock-v1"),
    })
    .unwrap();
    rsp.add_measurement(MeasurementBlock {
        index: 2,
        value_type: 0x01,
        digest: sha256(b"mainstage-v1"),
    })
    .unwrap();

    let mut req = SpdmRequester::<512>::new(transport, pub_xy);
    let mut rng_req = DeterministicRng::new(0x1111);
    let mut rng_rsp = DeterministicRng::new(0x2222);

    let mut tx = [0u8; 512];
    let mut rx = [0u8; 512];

    let n = req.build_get_version(&mut tx).unwrap();
    let r = rsp.handle_request(&tx[..n], &mut rx, &mut rng_rsp).unwrap();
    assert_eq!(req.handle_version(&rx[..r]).unwrap(), 0x12);

    let n = req.build_get_capabilities(&mut tx).unwrap();
    let r = rsp.handle_request(&tx[..n], &mut rx, &mut rng_rsp).unwrap();
    assert_ne!(req.handle_capabilities(&rx[..r]).unwrap(), 0);

    let n = req.build_negotiate_algorithms(&mut tx).unwrap();
    let r = rsp.handle_request(&tx[..n], &mut rx, &mut rng_rsp).unwrap();
    req.handle_algorithms(&rx[..r]).unwrap();

    let n = req.build_get_digests(&mut tx).unwrap();
    let r = rsp.handle_request(&tx[..n], &mut rx, &mut rng_rsp).unwrap();
    let _digest = req.handle_digests(&rx[..r]).unwrap();

    let mut offset = 0u16;
    loop {
        let n = req.build_get_certificate(offset, 32, &mut tx).unwrap();
        let r = rsp.handle_request(&tx[..n], &mut rx, &mut rng_rsp).unwrap();
        let rem = req.handle_certificate(&rx[..r]).unwrap();
        offset += 32;
        if rem == 0 {
            break;
        }
    }

    let n = req.build_challenge(&mut rng_req, &mut tx).unwrap();
    let r = rsp.handle_request(&tx[..n], &mut rx, &mut rng_rsp).unwrap();
    req.verify_challenge_auth(&tx[..n], &rx[..r]).unwrap();

    let n = req.build_get_measurements(&mut rng_req, &mut tx).unwrap();
    let r = rsp.handle_request(&tx[..n], &mut rx, &mut rng_rsp).unwrap();
    assert_eq!(req.verify_measurements(&tx[..n], &rx[..r]).unwrap(), 2);

    let n = req.build_key_exchange(&mut rng_req, &mut tx).unwrap();
    let r = rsp.handle_request(&tx[..n], &mut rx, &mut rng_rsp).unwrap();
    let sid = req.handle_key_exchange_rsp(&tx[..n], &rx[..r]).unwrap();
    assert_ne!(sid, 0);

    let n = req.build_secured_finish(&mut tx).unwrap();
    let r = rsp.handle_secured_spdm(&tx[..n], &mut rx).unwrap();
    req.handle_secured_finish_rsp(&rx[..r]).unwrap();

    let n = req
        .build_secured_cmd(SpdmCode::Heartbeat, 0, 0, &mut tx)
        .unwrap();
    let r = rsp.handle_secured_spdm(&tx[..n], &mut rx).unwrap();
    req.handle_secured_ack(SpdmCode::HeartbeatAck, &rx[..r])
        .unwrap();

    let n = req
        .build_secured_cmd(SpdmCode::KeyUpdate, 2, 0x55, &mut tx)
        .unwrap();
    let r = rsp.handle_secured_spdm(&tx[..n], &mut rx).unwrap();
    assert_eq!(
        req.handle_secured_ack(SpdmCode::KeyUpdateAck, &rx[..r])
            .unwrap(),
        (2, 0x55)
    );

    let n = req
        .build_secured_cmd(SpdmCode::Heartbeat, 0, 0, &mut tx)
        .unwrap();
    let r = rsp.handle_secured_spdm(&tx[..n], &mut rx).unwrap();
    req.handle_secured_ack(SpdmCode::HeartbeatAck, &rx[..r])
        .unwrap();

    let n = req
        .build_secured_cmd(SpdmCode::EndSession, 0, 0, &mut tx)
        .unwrap();
    let r = rsp.handle_secured_spdm(&tx[..n], &mut rx).unwrap();
    req.handle_secured_ack(SpdmCode::EndSessionAck, &rx[..r])
        .unwrap();
    assert!(req.session.is_none());
    assert!(rsp.session.is_none());
}

#[test]
fn spdm_full_lifecycle_mctp_and_doe() {
    run_full_spdm_session(SessionTransport::Mctp);
    run_full_spdm_session(SessionTransport::Doe);
}
