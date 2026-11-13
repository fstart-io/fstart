// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h models 00h-0Fh (Zeppelin) in the SP4/SP4r2 packages (EPYC
//! Embedded 3000).
//!
//! The PSP trains DRAM with the vendor ABL and reports the result in the
//! APOB; everything x86 does after that is native: Data Fabric, NBIO, SMU,
//! DXIO (PCIe), CCX and the Taishan FCH. Every die has its own DF, NBIO, SMU
//! and FCH; SP4r2 has one die, SP4 two.
mod init;
pub mod silicon;
pub use init::{FchUp, Up};

use super::Soc;
use fstart_driver_amd::{
    dxio::Descriptor,
    fch::{
        early::PowerFailure,
        gpio::Bank,
        taishan::{Devices, irq::Route},
    },
};
use fstart_driver_i2c::designware::{Speed, Timing};

/// The Zeppelin SoC: [`fstart_driver_amd::ccx::zp`], [`fstart_driver_amd::df::zp`],
/// [`fstart_driver_amd::nbio::zeppelin`], [`fstart_driver_amd::dxio::zeppelin`], [`fstart_driver_amd::smu::v9`]
/// and the Taishan FCH [`fstart_driver_amd::fch::taishan`].
pub enum Zeppelin {}
impl Soc for Zeppelin {
    /// Two dies on SP4, one on SP4r2.
    const MAX_DIES: usize = 2;
    const SMU: fstart_driver_amd::smu::Mailbox = fstart_driver_amd::smu::v9::MAILBOX;
    const DXIO_SERVICE: u32 = fstart_driver_amd::smu::v9::msg::DXIO;
    /// GPIO bank 0: 145 pins, but the vendor board tables also use 149-152. The
    /// Zeppelin IOMUX assignments aren't known, so pads name their functions
    /// by number.
    const GPIO_BANK: &'static Bank = fstart_driver_amd::fch::taishan::GPIO_BANK;
    const I2C: &'static [usize] = fstart_driver_amd::fch::taishan::I2C;
    fn ioms_fabric_id(die: u8) -> u8 {
        (die << 5) | 4
    }
}
impl fstart_driver_amd::ccx::microcode::Family for Zeppelin {
    /// ZP-A0 is model 0, ZP-B0/B1/B2 are model 1. The vendor ROM's BIOS
    /// directory has a type 0x66 patch per equivalence ID 8000/8010/8011/8012.
    const SIGNATURES: &'static [u32] = &[0x0080_0f00, 0x0080_0f10];
    const MAX_PATCHES: usize = 4;
}
pub const MAX_DIES: usize = Zeppelin::MAX_DIES;
pub type Die<P> = fstart_driver_amd::df::Die<Zeppelin, P>;

/// A board's Zeppelin configuration, by IP block.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Subsystem ID of the FCH, NBIO and PCIe functions.
    pub subsystem_id: u32,
    pub ccx: CcxConfig,
    pub fch: FchConfig,
    pub dxio: DxioConfig,
}

/// The cores.
#[derive(Debug, Clone, Copy)]
pub struct CcxConfig {
    /// SYSCFG.SMEE on every thread, as the vendor firmware sets it: the OS
    /// may use SME (`mem_encrypt=on`) and SEV. Off by default: the PSP's SEV
    /// firmware keeps its state in flash through the SMM handler's P2C SPI
    /// services (PSP_ZP §3.5), which fstart doesn't implement, so Linux's
    /// SEV setup times out (5 s) and disables the PSP driver.
    pub memory_encryption: bool,
}
impl CcxConfig {
    pub const DEFAULT: Self = Self {
        memory_encryption: false,
    };
}
impl Default for CcxConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The Taishan FCH.
#[derive(Debug, Clone, Copy)]
pub struct FchConfig {
    /// AC power-loss policy.
    pub power_failure: PowerFailure,
    /// FCH devices in use, per die.
    pub devices: [Devices; MAX_DIES],
    /// The PCI interrupt router (0xc00/0xc01) table.
    pub irq_routes: &'static [Route],
    /// GEVENT pins (bit n = GEVENTn) routed to the board's SMI handler
    /// (`fstart_driver_amd::fch::smi::PendingSmi::gevents`).
    pub smi_gevents: u32,
    /// Spread spectrum on the CG1 PLL, which clocks the PCIe reference
    /// clocks. On by default, as the vendor Setup default; a board whose
    /// PCIe devices don't tolerate the spread turns it off.
    pub spread_spectrum: bool,
}
impl FchConfig {
    pub const DEFAULT: Self = Self {
        power_failure: PowerFailure::Previous,
        devices: [Devices {
            xhci: true,
            sata: true,
        }; MAX_DIES],
        irq_routes: &[],
        smi_gevents: 0,
        spread_spectrum: true,
    };
}
impl Default for FchConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The DXIO engines (PCIe, SATA lanes) of each die.
#[derive(Debug, Clone, Copy, Default)]
pub struct DxioConfig {
    pub descriptors: [&'static [Descriptor]; MAX_DIES],
}

/// Non-PCI MMIO pool of a die, the top 8 MiB of its below-4G DF window: the
/// PSP BAR at +0, the FCH ACPI MMIO of dies >= 1 at +1 MiB, the IOMMU (512
/// KiB) at +2 MiB, the NBIO IOAPIC at +7 MiB.
pub const NONPCI_MMIO_SIZE: u32 = fstart_driver_amd::df::zp::NONPCI_MMIO_SIZE;
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

/// The vendor code doesn't power the I2C controllers up through AOAC or
/// touch the pads. It programs standard speed, these SCL counts (0x285 +
/// 0x357 cycles per bit at 100 kHz: a 150 MHz controller clock) and
/// IC_FS_SPKLEN 7.
pub const I2C_TIMING: Timing = Timing {
    speed: Speed::Standard,
    scl_high: 0x285,
    scl_low: 0x357,
    spike_length: Some(7),
    sda_hold: None,
};

/// FCH ACPI fixed I/O, the same layout as the vendor firmware.
pub use fstart_driver_amd::fch::taishan::acpi_io;

#[cfg(test)]
mod tests {
    extern crate std;
    use super::Zeppelin;
    use fstart_driver_amd::ccx::microcode::{Bundle, Error};
    const PATCH_BYTES: usize = 0xc80;
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
