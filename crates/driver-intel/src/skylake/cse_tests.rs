// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use crate::heci::tests::{State, TestTransport, packet, rig};
use fstart_pci::{PciAddress, PciEcam, PciRoot, PciRootInfo};
use std::{cell::RefCell, rc::Rc, vec, vec::Vec};

fn device_at(address: PciAddress) -> (Vec<u32>, EcamDevice) {
    let mut memory = vec![0u32; 1 << 18];
    let pci = PciEcam::new([PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base: memory.as_mut_ptr() as u64,
            bus_start: 0,
            bus_end: 0,
        },
        [],
    )
    .unwrap()])
    .unwrap();
    let d = pci.device(address).unwrap();
    (memory, d)
}
pub(crate) fn device() -> (Vec<u32>, EcamDevice) {
    let (memory, d) = device_at(PciAddress::new(0, 0, 0x16, 0));
    d.write32(0, 0xa13a_8086); // ordinary test RAM, not physical configuration space
    d.write32(0x10, 0xfed1_a004);
    d.write16(4, 6);
    d.write32(0x40, 5);
    d.write32(0x44, 1 << 16 | 8);
    (memory, d)
}
fn system_agent() -> (Vec<u32>, EcamDevice) {
    let (memory, sa) = device_at(PciAddress::new(0, 0, 0, 0));
    sa.write32(0, 0x190f_8086);
    sa.write32(8, 0x0600_0003);
    sa.write32(0x70, 0x7f80_0000);
    sa.write32(0x74, 0);
    // Independent literal for native (~8 + 1) & 0x7ffff, shifted 20,
    // plus ME_STLEN_EN bit 11. This is not an allocated RAM certificate.
    sa.write32(0x78, 0xff80_0800);
    sa.write32(0x7c, 0x7f);
    (memory, sa)
}
pub(crate) fn service(
    d: EcamDevice,
    t: TestTransport,
) -> Cse<
    crate::heci::tests::Host,
    crate::heci::tests::Me,
    crate::heci::tests::Tx,
    crate::heci::tests::Rx,
> {
    Cse {
        device: d,
        bound_base: 0xfed1_a000,
        transport: t,
    }
}
pub(crate) fn reply(s: &Rc<RefCell<State>>, bytes: &[u8]) {
    s.borrow_mut().reply.push_back(packet(bytes, true, 0, 7));
}

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
        let mut response = command.request(0);
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
        let (_memory, d) = device();
        let (_sa_memory, sa) = system_agent();
        let base: u64 = if action == 7 {
            0x1_7f80_0000
        } else {
            0x7f80_0000
        };
        sa.write32(0x74, (base >> 32) as u32);
        if action & 1 != 0 {
            sa.write32(0x78, sa.read32(0x78) | (1 << 10));
        }
        let (s, t, mut delay) = rig(8);
        let mut c = service(d, t);
        reply(&s, &[0xf0, 0x81, 0, 0, action, 0, 0, 0]);
        let dram = unsafe { ValidatedDram::new(uma, base, DramInitStatus::Success).unwrap() };
        assert_eq!(
            c.dram_init_done(&sa, dram, &mut delay),
            DramAction::parse(action)
        );
        assert_eq!(
            s.borrow().tx_history,
            vec![
                0x8008_0007,
                0x0000_01f0,
                if action == 7 {
                    0x1000_17f8
                } else {
                    0x1000_07f8
                }
            ]
        );
        assert_eq!(
            s.borrow().reset_assertions,
            0,
            "DID reset action is only returned"
        );
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
