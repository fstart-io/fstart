// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h models 00h-0Fh (Zeppelin) in the SP4/SP4r2 packages (EPYC
//! Embedded 3000).
//!
//! The PSP trains DRAM with the vendor ABL and reports the result in the
//! APOB; everything x86 does after that is native: Data Fabric, NBIO, SMU,
//! DXIO (PCIe), CCX and the Taishan FCH. Every die has its own DF, NBIO, SMU
//! and FCH; SP4r2 has one die, SP4 two.
pub mod ccx;
mod ccx_tables;
pub mod dxio;
pub mod fabric;
pub mod fch;
pub mod i2c;
pub mod identity;
pub mod iommu;
pub mod microcode;
pub mod nbio;
mod nbio_tables;
pub mod psp;
pub mod smu;
#[cfg(test)]
mod test_pci;

use crate::fam17h::topology::Soc;
pub use identity::{Package, Revision};

/// GPIO bank 0: 145 pins, but the vendor board tables also use 149-152. The Zeppelin
/// IOMUX assignments aren't known, so pads name their functions by number.
pub const GPIO_BANK: crate::gpio::Bank = crate::gpio::Bank {
    pins: 153,
    reserved: &[],
    gpio_function: |_| None,
    events: crate::gpio::FAMILY17H_EVENTS,
};

/// The Zeppelin SoC.
pub enum Zeppelin {}
impl Soc for Zeppelin {
    /// Two dies on SP4, one on SP4r2.
    const MAX_DIES: usize = 2;
    const SMU: crate::fam17h::smu::Mailbox = smu::MAILBOX;
    fn ioms_fabric_id(die: u8) -> u8 {
        (die << 5) | 4
    }
}
pub const MAX_DIES: usize = Zeppelin::MAX_DIES;
pub type Die<P> = crate::fam17h::topology::Die<Zeppelin, P>;

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
