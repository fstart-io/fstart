// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h models 00h-0Fh (Zeppelin) in the SP4/SP4r2 packages (EPYC
//! Embedded 3000).
//!
//! The PSP trains DRAM with the vendor ABL and reports the result in the
//! APOB; everything x86 does after that is native: Data Fabric, NBIO, SMU,
//! DXIO (PCIe), CCX and the Taishan FCH. Every die has its own DF, NBIO, SMU
//! and FCH; SP4r2 has one die, SP4 two.
pub mod i2c;
pub mod identity;
#[cfg(test)]
pub(crate) mod test_pci;

use crate::soc::Soc;
pub use identity::{Package, Revision};

/// GPIO bank 0: 145 pins, but the vendor board tables also use 149-152. The Zeppelin
/// IOMUX assignments aren't known, so pads name their functions by number.
pub const GPIO_BANK: crate::fch::gpio::Bank = crate::fch::gpio::Bank {
    pins: 153,
    reserved: &[],
    gpio_function: |_| None,
    events: crate::fch::gpio::FAMILY17H_EVENTS,
};

/// The Zeppelin SoC.
pub enum Zeppelin {}
impl Soc for Zeppelin {
    /// Two dies on SP4, one on SP4r2.
    const MAX_DIES: usize = 2;
    const SMU: crate::smu::Mailbox = crate::smu::v9::MAILBOX;
    const DXIO_SERVICE: u32 = crate::smu::v9::DXIO_SERVICE;
    fn ioms_fabric_id(die: u8) -> u8 {
        (die << 5) | 4
    }
}
impl crate::ccx::microcode::Family for Zeppelin {
    /// ZP-A0 is model 0, ZP-B0/B1/B2 are model 1. The vendor ROM's BIOS
    /// directory has a type 0x66 patch per equivalence ID 8000/8010/8011/8012.
    const SIGNATURES: &'static [u32] = &[0x0080_0f00, 0x0080_0f10];
    const MAX_PATCHES: usize = 4;
}
pub const MAX_DIES: usize = Zeppelin::MAX_DIES;
pub type Die<P> = crate::df::Die<Zeppelin, P>;

/// Non-PCI MMIO pool of a die, the top 8 MiB of its below-4G DF window: the
/// PSP BAR at +0, the FCH ACPI MMIO of dies >= 1 at +1 MiB, the IOMMU (512
/// KiB) at +2 MiB, the NBIO IOAPIC at +7 MiB.
pub const NONPCI_MMIO_SIZE: u32 = 8 << 20;
pub const NONPCI_PSP: u32 = 0;
pub const NONPCI_IOMMU: u32 = 2 << 20;
pub const NONPCI_IOAPIC: u32 = 7 << 20;

/// PCIe ECAM: 128 buses, as the vendor firmware.
pub const ECAM: u64 = 0xf000_0000;
pub const ECAM_BUSES: u16 = 128;
/// The PSP copies the APOB (BIOS directory entry 0x61) here.
pub const APOB_BASE: u64 = 0x3000_0000;
/// The vendor firmware's APOB_NV region is 128 KiB, so the APOB isn't larger.
pub const APOB_SIZE: usize = 0x2_0000;

/// FCH ACPI fixed I/O, the same layout as the vendor firmware.
pub mod acpi_io {
    pub const BASE: u16 = 0x800;
    pub const PM1_EVT: u16 = BASE;
    pub const PM1_CNT: u16 = BASE + 0x04;
    pub const PM_TMR: u16 = BASE + 0x08;
    pub const CSTATE_CONTROL: u16 = BASE + 0x10;
    pub const GPE0: u16 = BASE + 0x20;
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::Zeppelin;
    use crate::ccx::microcode::{Bundle, Error, PATCH_BYTES};
    use std::vec::Vec;

    #[test]
    fn selects_the_vendor_patch_by_equivalence_id() {
        // Header prefixes of the MJ11 vendor ROM's four 0x66 entries.
        let patches: Vec<u8> = [
            (0x0800_1250, 0x8012),
            (0x0800_1129, 0x8011),
            (0x0800_100c, 0x8010),
            (0x0800_002a, 0x8000),
        ]
        .into_iter()
        .flat_map(|(id, revision): (u32, u16)| {
            let mut patch = [0; PATCH_BYTES];
            patch[4..8].copy_from_slice(&id.to_le_bytes());
            patch[24..26].copy_from_slice(&revision.to_le_bytes());
            patch
        })
        .collect();
        let bundle = Bundle::<Zeppelin>::parse(&patches).unwrap();
        // EPYC 3151: ZP-B2.
        assert_eq!(bundle.select(0x0080_0f12).unwrap().revision(), 0x0800_1250);
        assert_eq!(bundle.select(0x0080_0f00).unwrap().revision(), 0x0800_002a);
        assert_eq!(bundle.select(0x0080_0f13).err(), Some(Error::CpuIdentity));
        assert_eq!(bundle.select(0x0081_0f10).err(), Some(Error::CpuIdentity));
    }
}
