// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;

use super::*;
use crate::skylake::registers::testing::{Image, image};
use crate::sunrise_point::pcr::testing::ecam;
use fstart_pci::PciAddress;
use std::vec::Vec;

pub(super) const CONFIG: EcamConfig = EcamConfig {
    base: 0xe000_0000,
    buses: 256,
};

fn host(sa: &EcamDevice) {
    sa.write32(0, 0x191f_8086);
    sa.write32(8, 0x0600_0001);
    sa.write8(0x0e, 0);
    sa.write32(PCIEXBAR, CONFIG.encoded_pciexbar().unwrap());
}
pub(super) fn pci_snapshot(sa: &EcamDevice) -> Vec<u32> {
    (0..0x1000)
        .step_by(4)
        .map(|offset| sa.read32(offset))
        .collect()
}
pub(super) fn mch_snapshot(image: &Image) -> Vec<u32> {
    image.0.iter().map(|cell| unsafe { *cell.get() }).collect()
}

#[test]
fn bootblock_systemagent_pciexbar_source_encodings_and_overlap_preflight() {
    for (buses, bits) in [(64, 5), (128, 3), (256, 1)] {
        let config = EcamConfig { buses, ..CONFIG };
        assert_eq!(config.encoded_pciexbar(), Ok(0xe000_0000 | bits));
        assert_eq!(plan_pciexbar(config, 0, 0xdead_beef), Ok(true));
        assert_eq!(plan_pciexbar(config, 0xe000_0000 | bits, 0), Ok(false));
        assert_eq!(
            plan_pciexbar(config, 0xe000_0000 | bits, 1),
            Err(Error::ConflictingDecode)
        );
    }
    for config in [
        EcamConfig { base: 0, ..CONFIG },
        EcamConfig {
            base: 0xe000_1000,
            ..CONFIG
        },
        EcamConfig {
            base: 1 << 32,
            ..CONFIG
        },
        EcamConfig {
            base: u64::MAX,
            ..CONFIG
        },
        EcamConfig {
            buses: 32,
            ..CONFIG
        },
        EcamConfig {
            buses: 512,
            ..CONFIG
        },
    ] {
        assert_eq!(config.encoded_pciexbar(), Err(Error::InvalidGeometry));
        // These paths reject before any privileged port I/O.
        assert_eq!(
            unsafe { bootstrap_ecam(config) },
            Err(Error::InvalidGeometry)
        );
    }
    for buses in [64, 128, 256] {
        let config = EcamConfig {
            base: match buses {
                256 => 0xf000_0000,
                128 => 0xf800_0000,
                _ => 0xfc00_0000,
            },
            buses,
        };
        assert_eq!(config.encoded_pciexbar(), Err(Error::Overlap));
        assert_eq!(unsafe { bootstrap_ecam(config) }, Err(Error::Overlap));
    }
    assert_eq!(
        plan_pciexbar(CONFIG, 0xe000_0010, 0),
        Err(Error::UnexplainedBarBits)
    );
    assert_eq!(
        plan_pciexbar(CONFIG, 0xd000_0001, 0),
        Err(Error::ConflictingDecode)
    );
    assert_eq!(
        plan_pciexbar(CONFIG, 0xe000_0003, 0),
        Err(Error::ConflictingDecode)
    );
}

#[test]
fn source_bar_words_geometry_and_conservative_unexplained_bits() {
    for (base, size) in [
        (MCHBAR_BASE as u32, 0x8000),
        (DMI_BASE, 0x1000),
        (EP_BASE, 0x1000),
        (EDRAM_BASE, 0x4000),
        (GDXC_BASE, 0x1000),
    ] {
        assert_eq!(plan_decode(0, u32::MAX, base, size), Ok(true));
        assert_eq!(plan_decode(base | 1, 0, base, size), Ok(false));
        assert_eq!(
            plan_decode(base | 1, 1, base, size),
            Err(Error::ConflictingDecode)
        );
        assert_eq!(
            plan_decode(base | 2, 0, base, size),
            Err(Error::UnexplainedBarBits)
        );
        assert_eq!(
            plan_decode((base + size) | 1, 0, base, size),
            Err(Error::ConflictingDecode)
        );
    }
    for (base, size) in [
        (0, 0x1000),
        (EDRAM_BASE + 0x1000, 0x4000),
        (GDXC_BASE, 0),
        (GDXC_BASE, 3),
    ] {
        assert_eq!(plan_decode(0, 0, base, size), Err(Error::InvalidGeometry));
    }
    assert_eq!(range_end(0xffff_f000, 0x1000), Ok(1 << 32));
    assert_eq!(range_end(0xffff_f000, 0x2000), Err(Error::InvalidGeometry));
}

#[test]
fn pam0_and_tseg_locks_reject_even_matching_values_in_both_cold_phases() {
    for (offset, value) in [(0x80, 0x31), (0xb8, 1), (0xb8, 0xabc0_0001)] {
        let (pci, _storage) = ecam();
        let sa = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
        host(&sa);
        sa.write32(offset, value);
        let before = pci_snapshot(&sa);
        assert_eq!(
            enable_pci_decodes_cold(&sa, CONFIG, MemoryBootMode::Cold),
            Err(Error::Locked)
        );
        assert_eq!(pci_snapshot(&sa), before);
        sa.write32(offset, 0);
        enable_pci_decodes_cold(&sa, CONFIG, MemoryBootMode::Cold).unwrap();
        sa.write32(offset, value);
        let before = pci_snapshot(&sa);
        let memory = image();
        let mch_before = mch_snapshot(&memory);
        assert_eq!(
            prepare_auxiliary_pam_tseg_cold(&sa, memory.registers(), CONFIG, MemoryBootMode::Cold),
            Err(Error::Locked)
        );
        assert_eq!(pci_snapshot(&sa), before);
        assert_eq!(mch_snapshot(&memory), mch_before);
    }
}
