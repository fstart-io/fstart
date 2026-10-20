// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso NBIO setup.
//! Hardware order and repeated table writes are intentional.
use crate::fam17h::tsc::DelayNs;
use super::{
    nbio_tables as tables,
    registers::{Reg, Smn, SmnReg, copy, rmw, write},
    smu,
};
use crate::fam17h::nbio::{
    Bar, IOAPIC_BASE_ADDR_HI, IOAPIC_BASE_ADDR_LO, IOAPIC_FEATURES_ENABLE, IoapicFeatures,
    IohcMmioBar, NB_BUS_NUM_CNTL, NB_LOWER_TOP_OF_DRAM2, NB_MMIOBASE, NB_MMIOLIMIT, NB_PCI_CTRL,
    NB_TOP_OF_DRAM_SLOT1_SMN, NB_TOP_OF_DRAM3, NB_UPPER_TOP_OF_DRAM2, NbBusNumCntl, NbPciCtrl,
    PSP_BASE_ADDR_HI, PSP_BASE_ADDR_LO, SMU_BASE_ADDR_HI, SMU_BASE_ADDR_LO, TopOfDram,
};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

pub const SMU_MMIO_BASE: u32 = 0xfd00_0000;
pub const PSP_MMIO_BASE: u32 = 0xfd10_0000;
const IOAPIC_BASE: u32 = 0xfec0_1000;

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
    super::registers::apply(smn, table.iter().map(record));
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Smu(smu::Error),
    SmuBar(u64),
    LockedPspBar(u64),
    MemoryBoundary,
}
#[derive(Debug, Clone, Copy)]
pub struct Capabilities {
    pub silicon_id: u32,
    pub graphics: bool,
}

fn smu_bar(smn: &Smn<impl ConfigRegionAccess>) -> Result<(), Error> {
    let (low, high) = (smn.reg(SMU_BASE_ADDR_LO), smn.reg(SMU_BASE_ADDR_HI));
    let bar = (u64::from(high.get()) << 32) | u64::from(low.get());
    if bar & 3 == 0 {
        high.set(0);
        low.set(SMU_MMIO_BASE | 3);
    } else if bar & 0x0000_ffff_fff0_0000 != u64::from(SMU_MMIO_BASE) {
        return Err(Error::SmuBar(bar));
    }
    Ok(())
}

fn installed_psp_bar() -> tock_registers::fields::FieldValue<u32, Bar::Register> {
    Bar::BASE.val(PSP_MMIO_BASE >> 20) + Bar::LOCK::SET + Bar::ENABLE::SET
}

/// Whether the PSP BAR is the one [`psp_bar`] installs. Read-only.
pub fn psp_bar_installed(smn: &Smn<impl ConfigRegionAccess>) -> bool {
    smn.reg(PSP_BASE_ADDR_LO).get() == installed_psp_bar().value
        && smn.reg(PSP_BASE_ADDR_HI).get() == 0
}

pub fn psp_bar(smn: &Smn<impl ConfigRegionAccess>) -> Result<(), Error> {
    let (low, high) = (smn.reg(PSP_BASE_ADDR_LO), smn.reg(PSP_BASE_ADDR_HI));
    let current = low.extract();
    let upper = high.get();
    let bar = (u64::from(upper) << 32) | u64::from(current.get());
    let want = installed_psp_bar();
    if current.is_set(Bar::LOCK) {
        if upper == 0 && current.matches_all(want) {
            return Ok(());
        }
        return Err(Error::LockedPspBar(bar));
    }
    low.write(want);
    high.set(0);
    Ok(())
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
    super::registers::apply(
        smn,
        IOMMU_RAS
            .into_iter()
            .chain([rmw(0x13b2_0400, 0x0017_1700, 0x0011_1100)]),
    );
    apply(smn, tables::IOMMU_LATE);
    super::registers::apply(smn, [write(0x1570_00f8, 0x800)]);
    apply(smn, tables::IOHC_LATE);
    smu_bar(smn)?;
    super::registers::apply(smn, [rmw(0x0141_1000, 0, 1 << 19)]);
    if smn.reg(FUSE_D5C0).get() >> 30 == 3 || raven2_cpuid {
        super::registers::apply(smn, [rmw(0x0141_1000, 0, 1 << 18)]);
    }
    if am4 {
        super::registers::apply(smn, [rmw(0x0141_0000, 0, 1 << 20)]);
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
    smn: &Smn<impl ConfigRegionAccess>,
    top_mem: u64,
    top_mem2: Option<u64>,
) -> Result<(), Error> {
    if top_mem == 0 || top_mem >= 1 << 32 || top_mem & 0x7fffff != 0 {
        return Err(Error::MemoryBoundary);
    }
    let mut tom = smn.reg(NB_TOP_OF_DRAM_SLOT1_SMN).extract();
    tom.modify(TopOfDram::TOM.val((top_mem >> 23) as u32));
    let mut low = smn.reg(NB_LOWER_TOP_OF_DRAM2).get();
    let mut high = smn.reg(NB_UPPER_TOP_OF_DRAM2).get();
    if let Some(top) = top_mem2.filter(|&top| top != 0) {
        let top = top & 0x0000_00ff_ffc0_0000;
        low = (low & 0x007ffffe) | (top as u32 & 0xff800000) | 1;
        high = (high & !0xff) | ((top >> 32) as u32 & 0xff);
    }
    smn.reg(NB_TOP_OF_DRAM_SLOT1_SMN).set(tom.get());
    smn.reg(NB_LOWER_TOP_OF_DRAM2).set(low);
    smn.reg(NB_UPPER_TOP_OF_DRAM2).set(high);
    smn.reg(NB_TOP_OF_DRAM3).set(0x83ffffff);
    smn.reg(NB_MMIOBASE).set(0x01000000);
    smn.reg(NB_MMIOLIMIT).set(0x7fffffff);
    smn.reg(NB_PCI_CTRL).modify(NbPciCtrl::MMIO_ENABLE::SET);
    Ok(())
}

pub fn ioapic_decode(smn: &Smn<impl ConfigRegionAccess>) {
    let address = IohcMmioBar::ADDRESS.val(IOAPIC_BASE >> 8);
    smn.reg(IOAPIC_BASE_ADDR_HI).set(0);
    smn.reg(IOAPIC_BASE_ADDR_LO).write(address);
    smn.reg(IOAPIC_BASE_ADDR_LO)
        .write(address + IohcMmioBar::ENABLE::SET);
    smn.reg(IOAPIC_FEATURES_ENABLE)
        .modify(IoapicFeatures::ID_EXT::SET + IoapicFeatures::FCH::SET);
    smn.reg(NB_BUS_NUM_CNTL)
        .write(NbBusNumCntl::LAT_MODE::SET + NbBusNumCntl::BUS.val(0));
}
