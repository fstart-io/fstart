// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso NBIO setup (AGESA AmdNbioBaseRV). Hardware order and repeated
//! table writes are intentional. The PCIe power management and the late SMU
//! notifications are [`late`].
use super::{
    ioapic::{IOAPIC_FEATURES_ENABLE, IoapicFeatures},
    iohc::{self, IOAPIC_BAR, NB_BUS_NUM_CNTL, NbBusNumCntl, PSP_BAR, SMU_BAR},
};
use crate::fam17h::tsc::DelayNs;
use crate::picasso::Die;
use crate::smu::v10 as smu;
use crate::smn::{Reg, Smn, SmnReg, copy, rmw, write};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

pub const SMU_MMIO_BASE: u32 = 0xfd00_0000;
pub const PSP_MMIO_BASE: u32 = 0xfd10_0000;
const IOAPIC_BASE: u32 = 0xfec0_1000;

pub mod late;
mod tables;

register_bitfields![u32,
    /// ACP_I2S_PIN_CONFIG.
    AcpPinConfig [CONFIG OFFSET(0) NUMBITS(3) []]
];
const ACP_I2S_PIN_CONFIG: SmnReg<AcpPinConfig::Register> = SmnReg::new(0x0124_1400);
/// Nonzero with graphics.
const GFX_PRESENT: SmnReg = SmnReg::new(0x0004_b600);
/// Copied into the IOMMU and PCIe blocks.
const SB_LOCATION: SmnReg = SmnReg::new(0x13b1_007c);
const SB_LOCATION_COPIES: [SmnReg; 2] = [SmnReg::new(0x1480_0024), SmnReg::new(0x13f0_112c)];
/// [31:30] = 3 on some fused parts.
const FUSE_D5C0: SmnReg = SmnReg::new(0x0005_d5c0);

/// The native table rows: a clear mask of all ones writes the value.
fn record(&(address, clear, set): &(u32, u32, u32)) -> Reg {
    if clear == u32::MAX {
        write(address, set)
    } else {
        rmw(address, clear, set)
    }
}
fn apply(smn: &Smn<impl ConfigRegionAccess>, table: &[(u32, u32, u32)]) {
    crate::smn::apply(smn, table.iter().map(record));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Smu(smu::Error),
    SmuBar(u64),
    /// The low half of a PSP BAR locked elsewhere.
    LockedPspBar(u32),
    MemoryBoundary,
}
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub silicon_id: u32,
    pub graphics: bool,
}

fn smu_bar(smn: &Smn<impl ConfigRegionAccess>) -> Result<(), Error> {
    let (low, high) = (smn.reg(SMU_BAR.low), smn.reg(SMU_BAR.high));
    let bar = (u64::from(high.get()) << 32) | u64::from(low.get());
    if bar & 3 == 0 {
        high.set(0);
        low.set(SMU_MMIO_BASE | 3);
    } else if bar & 0x0000_ffff_fff0_0000 != u64::from(SMU_MMIO_BASE) {
        return Err(Error::SmuBar(bar));
    }
    Ok(())
}

/// Whether the PSP BAR is the one [`psp_bar`] installs. Read-only.
pub fn psp_bar_installed(smn: &Smn<impl ConfigRegionAccess>) -> bool {
    smn.reg(PSP_BAR.low).get() == iohc::psp_bar(PSP_MMIO_BASE).value
        && smn.reg(PSP_BAR.high).get() == 0
}

/// Map the PSP at [`PSP_MMIO_BASE`], enabled and locked.
pub fn psp_bar(smn: &Smn<impl ConfigRegionAccess>) -> Result<(), Error> {
    iohc::map_psp(smn, PSP_MMIO_BASE).map_err(Error::LockedPspBar)
}

/// The IOMMU RAS copies and the late IOHC settings: raw vendor values.
const IOMMU_RAS: [Reg; 2] = [
    copy(0x1570_4330, 0x13f0_0080),
    copy(0x1570_401c, 0x0240_001c),
];

/// BSP-only native NBIO base sequence, no FSP state/HOB dependency.
/// `raven2_cpuid` is specifically the CPUID predicate used by these tables;
/// fused reduced-I/O PCO is NOT interchangeable with that predicate.
pub fn early_init(
    smn: &Smn<impl ConfigRegionAccess>,
    raven2_cpuid: bool,
    am4: bool,
    delay: &mut impl DelayNs,
) -> Result<Capabilities, Error> {
    let silicon_id =
        smu::request(smn, smu::GET_SILICON_ID, [0; 6], delay).map_err(Error::Smu)?[0];
    let graphics = silicon_id & (1 << 16) == 0 && smn.reg(GFX_PRESENT).get() != 0;
    apply(smn, tables::BASE);
    if !graphics {
        apply(smn, tables::NO_GFX);
    }
    let sb = smn.reg(SB_LOCATION).get();
    for copy in SB_LOCATION_COPIES {
        smn.reg(copy).set(sb);
    }
    apply(smn, tables::IOMMU);
    if raven2_cpuid {
        apply(smn, tables::RAVEN2);
    }
    // ACP owns its audio pins later; start with all pins unconfigured.
    smn.reg(ACP_I2S_PIN_CONFIG)
        .modify(AcpPinConfig::CONFIG.val(7));
    crate::smn::apply(
        smn,
        IOMMU_RAS
            .into_iter()
            .chain([rmw(0x13b2_0400, 0x0017_1700, 0x0011_1100)]),
    );
    apply(smn, tables::IOMMU_LATE);
    crate::smn::apply(smn, [write(0x1570_00f8, 0x800)]);
    apply(smn, tables::IOHC_LATE);
    smu_bar(smn)?;
    crate::smn::apply(smn, [rmw(0x0141_1000, 0, 1 << 19)]);
    if smn.reg(FUSE_D5C0).get() >> 30 == 3 || raven2_cpuid {
        crate::smn::apply(smn, [rmw(0x0141_1000, 0, 1 << 18)]);
    }
    if am4 {
        crate::smn::apply(smn, [rmw(0x0141_0000, 0, 1 << 20)]);
    }
    Ok(Capabilities {
        silicon_id,
        graphics,
    })
}

/// Preserve the PSP's DRAM topology and expose high MMIO through IOHC.
/// TOP_MEM/TOP_MEM2 must be read from AMD MSRs, NOT derived from a guessed
/// DIMM capacity or the number of populated channels.
pub fn dram_routes(
    die: &Die<impl ConfigRegionAccess + Copy>,
    top_mem: u64,
    top_mem2: Option<u64>,
) -> Result<(), Error> {
    if top_mem == 0 || top_mem >= 1 << 32 || top_mem & 0x7fffff != 0 {
        return Err(Error::MemoryBoundary);
    }
    // Picasso's physical address space ends below 1 TiB.
    let top_mem2 = top_mem2.map(|top| top & 0x0000_00ff_ffc0_0000);
    iohc::dram_map(die, top_mem as u32, top_mem2);
    Ok(())
}

pub fn ioapic_decode(smn: &Smn<impl ConfigRegionAccess>) {
    IOAPIC_BAR.map(smn, IOAPIC_BASE);
    smn.reg(IOAPIC_FEATURES_ENABLE)
        .modify(IoapicFeatures::ID_EXT::SET + IoapicFeatures::FCH::SET);
    smn.reg(NB_BUS_NUM_CNTL)
        .write(NbBusNumCntl::LAT_MODE::SET + NbBusNumCntl::BUS.val(0));
}
