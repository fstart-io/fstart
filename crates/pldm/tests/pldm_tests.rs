use fstart_pldm::base::{
    crc32_ieee, encode_get_pldm_types_req, encode_get_pldm_version_req, encode_get_tid_req,
    encode_set_tid_req,
};
use fstart_pldm::fw_update::{
    FdState, encode_activate_firmware_req, encode_get_fw_params_req, encode_get_status_req,
    encode_pass_component_req, encode_request_update_req, encode_update_component_req,
};
use fstart_pldm::platform::{
    NumericSensor, StateEffecter, encode_get_pdr_repo_info_req, encode_get_sensor_reading_req,
    encode_set_state_effecter_req,
};
use fstart_pldm::{CompletionCode, PldmResponder, PldmType};

#[test]
fn pldm_base_and_platform_sans_io() {
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
    let mut resp = [0u8; 64];

    let n = encode_set_tid_req(1, 0x22, &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(resp[3], CompletionCode::Success as u8);

    let n = encode_get_tid_req(2, &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(resp[4], 0x22);

    let n = encode_get_pldm_types_req(3, &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(resp[4], (1 << 0) | (1 << 2) | (1 << 5));

    let n = encode_get_pldm_version_req(4, PldmType::FirmwareUpdate, &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    let ver = &resp[9..13];
    let crc = u32::from_le_bytes(resp[13..17].try_into().unwrap());
    assert_eq!(crc, crc32_ieee(ver));

    let n = encode_get_sensor_reading_req(5, 42, &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(u32::from_le_bytes(resp[10..14].try_into().unwrap()), 55_000);

    let n = encode_set_state_effecter_req(6, 7, 3, &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(pldm.platform.effecter_state(7), Some(3));

    let n = encode_get_pdr_repo_info_req(7, &mut req).unwrap();
    let rlen = pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(rlen, 44);
    assert_eq!(u32::from_le_bytes(resp[31..35].try_into().unwrap()), 2);
}

#[test]
fn pldm_firmware_update_full_state_machine() {
    let mut pldm = PldmResponder::<4, 2, 1024>::new(0x01, 0x0000_9E9E);
    let image: Vec<u8> = (0..300).map(|i| (i & 0xFF) as u8).collect();
    let mut req = [0u8; 128];
    let mut resp = [0u8; 128];

    // GetFirmwareParameters
    let n = encode_get_fw_params_req(0, &mut req).unwrap();
    let r = pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(r, 14);
    assert_eq!(resp[3], CompletionCode::Success as u8);

    // RequestUpdate -> LearnComponents
    let n = encode_request_update_req(1, 128, 1, 1, 0, b"1.0.0", &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(resp[3], CompletionCode::Success as u8);
    assert_eq!(pldm.fw_update.state(), FdState::LearnComponents);

    // PassComponentTable -> ReadyXfer
    let n = encode_pass_component_req(2, 0x000A, 0x0001, 1, b"1.0.1", &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(pldm.fw_update.state(), FdState::ReadyXfer);

    // Premature ActivateFirmware without any applied component must fail with IncompleteUpdate
    let n = encode_activate_firmware_req(9, true, &mut req).unwrap();
    pldm.handle_request(&req[..n], &mut resp).unwrap();
    assert_eq!(resp[3], CompletionCode::IncompleteUpdate as u8);

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
    // Failed verify must remain in Verify (cannot advance to Apply)
    assert_eq!(
        pldm.fw_update
            .build_verify_complete(11, 0x01, &mut req)
            .unwrap(),
        4
    );
    assert_eq!(pldm.fw_update.state(), FdState::Verify);

    assert_eq!(
        pldm.fw_update
            .build_verify_complete(12, 0x00, &mut req)
            .unwrap(),
        4
    );
    assert_eq!(pldm.fw_update.state(), FdState::Apply);
    assert_eq!(
        pldm.fw_update
            .build_apply_complete(13, 0x00, &mut req)
            .unwrap(),
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
