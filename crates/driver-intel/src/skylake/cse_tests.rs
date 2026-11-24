// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[test]
fn me_policy_source_path_priority_and_did_filter() {
    use BiosPath::*;
    let cases = [
        (5, 0, Normal, true, true),
        (5 | 1 << 11, 0, FirmwareUpdate, true, true),
        (5 | 1 << 12, 0, Error, true, false),
        (2, 0, Recovery, true, false),
        (2 | 3 << 12, 0, ErrorWithoutDid, false, false),
        (5 | 5 << 16, 0, SecurityOverrideMei, true, false),
        (5 | 3 << 16, 0, TemporaryDisable, true, false),
        (5 | 4 << 16, 0, SecurityOverrideJumper, true, false),
        (5 | 2 << 16, 0, Debug, false, false),
        (5 | 2 << 16, 1 << 9, Enforcement, false, false),
        (5 | 15 << 16, 1 << 9, Sps, true, true),
        (5 | 1 << 5, 0, Error, true, false),
    ];
    for (h1, h4, path, did, bclk) in cases {
        let status = FirmwareStatus::new(h1, h4).unwrap();
        assert_eq!(status.bios_path(), path);
        assert_eq!(status.allows(Command::DramInitDone), did);
        assert_eq!(status.allows(Command::Bclk), bclk);
    }
    for (h1, h4) in [(0, 0), (u32::MAX, 0), (5, u32::MAX)] {
        assert!(matches!(
            FirmwareStatus::new(h1, h4),
            Err(super::Error::Unavailable)
        ));
    }
}

#[test]
fn me_misc_shadow_valid_zero_is_reported_but_absent_is_not_zero() {
    assert_eq!(UmaRequest::from_shadow(0), Ok(None));
    assert_eq!(UmaRequest::from_shadow(1 << 16).unwrap().unwrap().mib(), 0);
    assert_eq!(
        UmaRequest::from_shadow(1 << 16 | 64)
            .unwrap()
            .unwrap()
            .mib(),
        64
    );
    assert_eq!(
        UmaRequest::from_shadow(1 << 16 | 65),
        Err(Error::InvalidUma)
    );
    assert_eq!(UmaRequest::from_shadow(u32::MAX), Err(Error::Unavailable));
}

#[test]
fn native_cpu_replacement_valid_and_dynamic_fusing_are_distinct() {
    assert_eq!(CpuReplacement::from_hfs2(0), Ok(None));
    assert_eq!(CpuReplacement::from_hfs2(u32::MAX), Err(Error::Unavailable));
    for replaced in [false, true] {
        for reset in [false, true] {
            assert_eq!(
                CpuReplacement::from_hfs2(
                    1 << 8 | u32::from(replaced) << 4 | u32::from(reset) << 7
                ),
                Ok(Some(CpuReplacement {
                    replaced,
                    dynamic_fusing_warm_reset: reset
                }))
            );
        }
    }
}

#[test]
fn mkhi_response_group_command_response_bit_result_and_size_are_validated() {
    for command in [Command::DramInitDone, Command::Bclk] {
        let mut response = [0u8; 8];
        response.copy_from_slice(command.request(0).as_bytes());
        response[1] = 0x81;
        assert_eq!(parse_response(command, &response), Ok([0; 4]));
        for index in 0..4 {
            let mut bad = response;
            bad[index] ^= 1;
            assert!(parse_response(command, &bad).is_err());
        }
        let mut bad = response;
        bad[1] = 1;
        assert_eq!(parse_response(command, &bad), Err(Error::ResponseHeader));
        assert_eq!(
            parse_response(command, &response[..7]),
            Err(Error::ResponseSize {
                expected: 8,
                actual: 7
            })
        );
        let long = [0u8; 9];
        assert_eq!(
            parse_response(command, &long),
            Err(Error::ResponseSize {
                expected: 8,
                actual: 9
            })
        );
    }
}

#[test]
fn native_did_request_exact_source_encoding_and_all_bios_actions() {
    let uma = UmaRequest::from_shadow(1 << 16 | 8).unwrap().unwrap();
    for init in [
        DramInitStatus::Success,
        DramInitStatus::NoMemory,
        DramInitStatus::MemoryNotPreserved,
    ] {
        let dram = unsafe { ValidatedDram::new(uma, 0x1_2300_0000, init).unwrap() };
        assert_eq!(dram.data(), 0x1000_1230 | (init as u32) << 24);
    }
    for action in 0..8 {
        assert!(DramAction::parse(action).is_ok());
    }
    assert_eq!(DramAction::parse(8), Err(Error::InvalidAction(8)));
}

#[test]
fn native_did_validated_allocation_requires_contiguous_naturally_aligned_me_range() {
    // Intel 332688-003EN pp65–66: ME size is power-of-two MiB and
    // MEBASE naturally aligned. A reported request alone is not allocation.
    for (mib, base) in [(3, 0x7f80_0000), (8, 0x7f90_0000), (64, 0x7f80_0000)] {
        let request = UmaRequest::from_shadow(1 << 16 | mib).unwrap().unwrap();
        assert!(matches!(
            unsafe { ValidatedDram::new(request, base, DramInitStatus::Success) },
            Err(Error::InvalidUma)
        ));
    }
    for (mib, base) in [(8, 0x7f80_0000), (64, 0x7c00_0000), (0, 0)] {
        let request = UmaRequest::from_shadow(1 << 16 | mib).unwrap().unwrap();
        assert!(unsafe { ValidatedDram::new(request, base, DramInitStatus::Success) }.is_ok());
    }
    // A truthful failed-memory report allocates no range; it may report an
    // unsupported firmware request without pretending that request became RAM.
    let request = UmaRequest::from_shadow(1 << 16 | 3).unwrap().unwrap();
    assert!(unsafe { ValidatedDram::new(request, 0, DramInitStatus::NoMemory) }.is_ok());
}
