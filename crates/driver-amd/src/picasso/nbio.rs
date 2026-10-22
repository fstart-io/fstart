// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Picasso NBIO setup.
//! Hardware order and repeated table writes are intentional.
use super::{nbio_tables as tables, registers::Smn, smu};

pub const SMU_MMIO_BASE: u32 = 0xfd00_0000;
pub const PSP_MMIO_BASE: u32 = 0xfd10_0000;
const SMU_BAR: u32 = 0x13b1_02e8;
const PSP_BAR: u32 = 0x13b1_02e0;
const IOAPIC_BAR: u32 = 0x13b1_02f0;

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

fn apply(smn: &mut impl Smn, table: &[(u32, u32, u32)]) {
    for &(address, clear, set) in table {
        if clear == u32::MAX {
            smn.write(address, set);
        } else {
            smn.update(address, clear, set);
        }
    }
}

fn smu_bar(smn: &mut impl Smn) -> Result<(), Error> {
    let bar = (u64::from(smn.read(SMU_BAR + 4)) << 32) | u64::from(smn.read(SMU_BAR));
    if bar & 3 == 0 {
        smn.write(SMU_BAR + 4, 0);
        smn.write(SMU_BAR, SMU_MMIO_BASE | 3);
    } else if bar & 0x0000_ffff_fff0_0000 != u64::from(SMU_MMIO_BASE) {
        return Err(Error::SmuBar(bar));
    }
    Ok(())
}

pub fn psp_bar(smn: &mut impl Smn) -> Result<(), Error> {
    let low = smn.read(PSP_BAR);
    let high = smn.read(PSP_BAR + 4);
    let bar = (u64::from(high) << 32) | u64::from(low);
    if low & 0x100 != 0 {
        if high == 0 && low & 0xfff0_0101 == PSP_MMIO_BASE | 0x101 {
            return Ok(());
        }
        return Err(Error::LockedPspBar(bar));
    }
    smn.write(PSP_BAR, PSP_MMIO_BASE | 0x101);
    smn.write(PSP_BAR + 4, 0);
    Ok(())
}

/// BSP-only native NBIO base sequence, no FSP state/HOB dependency.
/// `raven2_cpuid` is specifically the CPUID predicate used by these tables;
/// fused reduced-I/O PCO is NOT interchangeable with that predicate.
pub fn early_init(
    smn: &mut impl Smn,
    raven2_cpuid: bool,
    am4: bool,
    delay_us: &mut impl FnMut(u32),
) -> Result<Capabilities, Error> {
    let silicon_id =
        smu::request(smn, smu::GET_SILICON_ID, [0; 6], delay_us).map_err(Error::Smu)?[0];
    let graphics = silicon_id & (1 << 16) == 0 && smn.read(0x0004_b600) != 0;
    apply(smn, tables::BASE);
    if !graphics {
        apply(smn, tables::NO_GFX);
    }
    let sb = smn.read(0x13b1_007c);
    smn.write(0x1480_0024, sb);
    smn.write(0x13f0_112c, sb);
    apply(smn, tables::IOMMU);
    if raven2_cpuid {
        apply(smn, tables::RAVEN2);
    }
    // ACP owns its audio pins later; start with all pins unconfigured.
    smn.update(0x0124_1400, 7, 7);
    let value = smn.read(0x13f0_0080);
    smn.write(0x1570_4330, value);
    let value = smn.read(0x0240_001c);
    smn.write(0x1570_401c, value);
    smn.update(0x13b2_0400, 0x00171700, 0x00111100);
    apply(smn, tables::IOMMU_LATE);
    smn.write(0x1570_00f8, 0x800);
    apply(smn, tables::IOHC_LATE);
    smu_bar(smn)?;
    smn.update(0x0141_1000, 0, 1 << 19);
    if smn.read(0x0005_d5c0) >> 30 == 3 || raven2_cpuid {
        smn.update(0x0141_1000, 0, 1 << 18);
    }
    if am4 {
        smn.update(0x0141_0000, 0, 1 << 20);
    }
    Ok(Capabilities {
        silicon_id,
        graphics,
    })
}

/// Preserve the PSP's DRAM topology and expose high MMIO through IOHC.
/// TOP_MEM/TOP_MEM2 must be read from AMD MSRs, NOT derived from a guessed
/// DIMM capacity or the number of populated channels.
pub fn dram_routes(smn: &mut impl Smn, top_mem: u64, top_mem2: Option<u64>) -> Result<(), Error> {
    if top_mem == 0 || top_mem >= 1 << 32 || top_mem & 0x7fffff != 0 {
        return Err(Error::MemoryBoundary);
    }
    let tom = (smn.read(0x13b0_0090) & 0x007fffff) | (top_mem as u32 & 0xff800000);
    let mut low = smn.read(0x13b1_0064);
    let mut high = smn.read(0x13b1_0068);
    if let Some(top) = top_mem2.filter(|&top| top != 0) {
        let top = top & 0x0000_00ff_ffc0_0000;
        low = (low & 0x007ffffe) | (top as u32 & 0xff800000) | 1;
        high = (high & !0xff) | ((top >> 32) as u32 & 0xff);
    }
    smn.write(0x13b0_0090, tom);
    smn.write(0x13b1_0064, low);
    smn.write(0x13b1_0068, high);
    smn.write(0x13b1_0138, 0x83ffffff);
    smn.write(0x13b1_005c, 0x01000000);
    smn.write(0x13b1_0060, 0x7fffffff);
    smn.update(0x13b0_004c, 0, 1 << 23);
    Ok(())
}

pub fn ioapic_decode(smn: &mut impl Smn) {
    smn.write(IOAPIC_BAR + 4, 0);
    smn.write(IOAPIC_BAR, 0xfec01000);
    smn.write(IOAPIC_BAR, 0xfec01001);
    smn.update(0x1430_0000, 0, 0x14);
    smn.write(0x13b1_0044, 0x100);
}
