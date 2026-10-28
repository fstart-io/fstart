// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin Data Fabric initialization. The sequences follow the vendor
//! AmdFabricZpPei/Dxe and the MMIO/IO distribution of AmdSocSp4ZpPei (sp4r2
//! reports DF_ZP and SOC_DISPATCH). Each die has its own DF; what AGESA calls a
//! broadcast access is a direct access repeated on every die. The MMIO/IO
//! distribution is [`super::map::plan`].
use super::{
    Df, DfReg, map,
    regs::{self, CFG_ACCESS_CTRL, CLOCK_GATING, DRAM_HOLE, GLOBAL_CTRL},
    v2::{self, DRAM_BASE, DRAM_LIMIT, SYSTEM_CFG},
};
use crate::cpu::Revision;
use crate::{
    apob::{Apob, mem},
    soc::Soc,
    soc::zeppelin::{NONPCI_MMIO_SIZE, Zeppelin},
};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    Ccm104 [B5 OFFSET(5) NUMBITS(1) [], B28 OFFSET(28) NUMBITS(1) []],
    Ccm108 [B6 OFFSET(6) NUMBITS(1) []],
    Ioms04c [B5 OFFSET(5) NUMBITS(1) []],
    Ioms300 [B8 OFFSET(8) NUMBITS(1) []],
    Cfg0c0 [B17_12 OFFSET(12) NUMBITS(6) []],
    PieCfg [
        /// Erratum 1158 (LINT0/LINT1 while the APIC is software disabled).
        LINT_ERRATUM_1158 OFFSET(8) NUMBITS(1) [],
        DIS_SYNC_FLOOD_PROP OFFSET(19) NUMBITS(1) []
    ],
    ScrubBase [ENABLE OFFSET(0) NUMBITS(1) [], BASE OFFSET(4) NUMBITS(28) []],
    ScrubLimit [RATE OFFSET(0) NUMBITS(4) [], LIMIT OFFSET(4) NUMBITS(28) []],
    ClkGateDc [IOMS OFFSET(0) NUMBITS(1) []]
];

// Function 0
const F2_CS_CFG_68: DfReg = DfReg::new(2, 0x068);
const F2_CS_CFG_6C: DfReg = DfReg::new(2, 0x06c);
const F3_IOMS_04C: DfReg<Ioms04c::Register> = DfReg::new(3, 0x04c);
const F3_CCM_104: DfReg<Ccm104::Register> = DfReg::new(3, 0x104);
const F3_CCM_108: DfReg<Ccm108::Register> = DfReg::new(3, 0x108);
const F3_IOMS_300: DfReg<Ioms300::Register> = DfReg::new(3, 0x300);
const F4_CFG_0C0: DfReg<Cfg0c0::Register> = DfReg::new(4, 0x0c0);
const F4_CCM_0C4: DfReg = DfReg::new(4, 0x0c4);
const F4_CFG_0C8: DfReg = DfReg::new(4, 0x0c8);
const F5_PIE_CFG: DfReg<PieCfg::Register> = DfReg::new(5, 0x104);
const F5_PIE_PWR_300: DfReg = DfReg::new(5, 0x300);
const F5_PIE_PWR_304: DfReg = DfReg::new(5, 0x304);
const F5_PIE_PWR_354: DfReg = DfReg::new(5, 0x354);
const F5_PIE_PWR_358: DfReg = DfReg::new(5, 0x358);
const F6_SCRUB_BASE: DfReg<ScrubBase::Register> = DfReg::new(6, 0x048);
const F6_SCRUB_LIMIT: DfReg<ScrubLimit::Register> = DfReg::new(6, 0x04c);
/// Per UMC instance.
const F6_REDIR_SCRUB: DfReg = DfReg::new(6, 0x058);
const F6_CLK_GATE_DC: DfReg<ClkGateDc::Register> = DfReg::new(6, 0x0dc);

const INST_CCM0: u8 = 2;
const INST_CCM1: u8 = 3;
const INST_IOMS: u8 = 4;
const INST_CS_DRAM_MAP: u8 = 5;

const GIB: u64 = 1 << 30;

/// The vendor code keeps the top 8 MiB of every die's below-4G window (pair
/// 2 * die) as its non-PCI MMIO pool. On die 0 that's 0xfe400000-0xfebfffff.
/// `None` if the die has no such window.
pub fn nonpci_mmio_base(df0: &Df<impl ConfigRegionAccess>, die: u8) -> Option<u32> {
    let route = map::mmio_route(df0, 2 * die);
    let usable = route.enabled()
        && route.destination == Zeppelin::ioms_fabric_id(die)
        && route.end() <= 4 * GIB
        && route.size >= u64::from(NONPCI_MMIO_SIZE);
    usable.then(|| (route.end() - u64::from(NONPCI_MMIO_SIZE)) as u32)
}

fn clock_gating(df: &Df<impl ConfigRegionAccess>, single_die: bool) {
    const INSTANCES: [u8; 12] = [0, 1, 2, 3, 5, 12, 13, 14, 15, 16, 17, 18];
    let gate = df
        .instance(F6_CLK_GATE_DC, INST_IOMS)
        .is_set(ClkGateDc::IOMS);
    if !gate && single_die {
        df.reg(regs::CLOCK_GATING).write(CLOCK_GATING::ENABLE::SET);
        return;
    }
    for instance in INSTANCES {
        df.instance(regs::CLOCK_GATING, instance)
            .write(CLOCK_GATING::ENABLE::SET);
    }
    if !gate {
        df.instance(regs::CLOCK_GATING, INST_IOMS)
            .write(CLOCK_GATING::ENABLE::SET);
    }
}

/// The vendor CS, CCM and IOMS settings; their meaning isn't known.
fn ccm_ioms_init(df: &Df<impl ConfigRegionAccess>, rev_a0: bool) {
    // CS
    df.reg(F2_CS_CFG_68).set(0x0006_5019);
    df.reg(F2_CS_CFG_6C).set(0x1004_0611);
    // CCM
    for instance in [INST_CCM0, INST_CCM1] {
        df.instance(F4_CCM_0C4, instance).set(0x0002_0001);
    }
    df.reg(F4_CFG_0C8).set(0x0102_0000);
    for instance in [INST_CCM0, INST_CCM1] {
        df.instance(F4_CFG_0C0, instance)
            .modify(Cfg0c0::B17_12.val(0));
        df.instance(F3_CCM_104, instance)
            .modify(Ccm104::B28::SET + Ccm104::B5.val(rev_a0.into()));
        df.instance(F3_CCM_108, instance).modify(Ccm108::B6::SET);
    }
    // IOMS
    df.instance(F3_IOMS_04C, INST_IOMS).modify(Ioms04c::B5::SET);
    df.instance(F3_IOMS_300, INST_IOMS).modify(Ioms300::B8::SET);
    df.instance(F4_CFG_0C0, INST_IOMS)
        .modify(Cfg0c0::B17_12.val(if rev_a0 { 0x1e } else { 0 }));
}

fn pie_ras_init(df: &Df<impl ConfigRegionAccess>) {
    // Sync flood propagation stays enabled (vendor default). The erratum
    // 1158 workaround is from the revision guide; the vendor code doesn't
    // set it.
    df.reg(F5_PIE_CFG)
        .modify(PieCfg::DIS_SYNC_FLOOD_PROP::CLEAR + PieCfg::LINT_ERRATUM_1158::SET);
    map::clear_errors(df);
    for instance in 0..12 {
        df.instance(regs::GLOBAL_CTRL, instance)
            .modify(GLOBAL_CTRL::BIT3::CLEAR);
    }
}

/// AmdFabricZpPei, per die.
pub fn early_init(df: &Df<impl ConfigRegionAccess>, dies: u8, revision: Revision) {
    clock_gating(df, dies == 1);
    ccm_ioms_init(df, revision == Revision::ZpA0);
    pie_ras_init(df);
}

/// DRAM behind this die's scrubber, minus the MMIO hole if it is in range.
fn scrub_dram_size(df: &Df<impl ConfigRegionAccess>, die: u8) -> u64 {
    let base = df.reg(F6_SCRUB_BASE).read(ScrubBase::BASE);
    let limit = df.reg(F6_SCRUB_LIMIT).extract();
    if limit.get() == 0 {
        return 0;
    }
    let limit = limit.read(ScrubLimit::LIMIT);
    let mut size = u64::from(limit.wrapping_sub(base).wrapping_add(1)) << 20;
    for i in 0..2 {
        let dram_base = df.instance(v2::dram_base(i), INST_CS_DRAM_MAP).extract();
        let dram_limit = df.instance(v2::dram_limit(i), INST_CS_DRAM_MAP).extract();
        // Socket 0: the destination is the die number.
        if !dram_base.matches_all(DRAM_BASE::VALID::SET + DRAM_BASE::HOLE_EN::SET)
            || dram_limit.read(DRAM_LIMIT::DST_DIE) != u32::from(die)
        {
            continue;
        }
        let hole = df.instance(regs::DRAM_HOLE, INST_CS_DRAM_MAP).extract();
        if hole.is_set(DRAM_HOLE::VALID) {
            size -= 4 * GIB - (u64::from(hole.read(DRAM_HOLE::BASE)) << 24);
        }
    }
    size
}

/// The scrub rate for one pass over `size` bytes in `hours`: the largest table
/// index whose threshold (inferred: ns per 64-byte line) doesn't exceed
/// hours * 3.96e12 / (size / 64).
pub fn scrub_rate(size: u64, hours: u64) -> u8 {
    const THRESHOLDS: [u64; 16] = [
        640, 1280, 2560, 5120, 10200, 20500, 41000, 81900, 163800, 327700, 655400, 1310000,
        2620000, 5240000, 10490000, 20970000,
    ];
    if size >> 6 == 0 {
        return 0;
    }
    let q = hours * 3_960_000_000_000 / (size >> 6);
    THRESHOLDS
        .iter()
        .take_while(|&&t| t <= q)
        .count()
        .saturating_sub(1) as u8
}

/// The DRAM scrubber of the vendor AmdFabricZpPei with its defaults: scrub
/// redirection on (PCD 0x8f) and one pass every 24 hours (PCD 0x8e). The ABL
/// sets the scrub base and limit.
pub fn scrub_init(df: &Df<impl ConfigRegionAccess>, die: u8, apob: &Apob) -> Option<(u8, u64)> {
    let size = scrub_dram_size(df, die);
    if size == 0 {
        return None;
    }
    // Inferred meaning: the two UMC channels are populated.
    let config = mem::mem_config(apob, 0, die).ok()?;
    let [Ok(a), Ok(b)] = [0, 1].map(|channel| config.channel_byte(channel)) else {
        return None;
    };
    if a == 0 && b == 0 {
        return None;
    }
    for (instance, populated) in [a, b].into_iter().enumerate() {
        df.instance(F6_REDIR_SCRUB, instance as u8)
            .set(u32::from(populated != 0));
    }
    let rate = scrub_rate(size, 24);
    df.reg(F6_SCRUB_LIMIT)
        .modify(ScrubLimit::RATE.val(rate.into()));
    df.reg(F6_SCRUB_BASE).modify(ScrubBase::ENABLE::SET);
    Some((rate, size))
}

/// AmdFabricZpDxe: PIE power management, per die.
/// The PIE power values are the vendor's; their meaning isn't known.
pub fn late_init(
    df: &Df<impl ConfigRegionAccess>,
    dies: u8,
    sockets: u8,
    revision: Revision,
    apob: &Apob,
) {
    let multi_die = dies / sockets > 1;
    let apob_flag =
        apob.version() >= 9 && mem::pie_power(apob).is_ok_and(|pie| pie.flag() == Ok(true));
    let bit = |cond: bool, mask: u32| if cond { mask } else { 0 };

    // Bit 5 stays clear with C6 enabled (vendor PCD 0x13 = 1).
    let pwr_300 = df.reg(F5_PIE_PWR_300);
    let mut v = pwr_300.get();
    v &= !((1 << 5) | (1 << 12) | (1 << 14) | (1 << 15));
    v |= bit(apob_flag, 1 << 5) | (1 << 13);
    let early_rev = matches!(revision, Revision::ZpA0 | Revision::ZpB0 | Revision::ZpB1);
    v |= bit(!(dies > 1 && early_rev), 1 << 12);
    v |= bit(multi_die, 1 << 14) | bit(sockets > 1, 1 << 15);
    pwr_300.set(v);

    let pwr_358 = df.reg(F5_PIE_PWR_358);
    pwr_358.set((pwr_358.get() & 0x0f00_0000) | 0x80e0_00e0 | bit(multi_die, 0x0000_e000));

    let pwr_304 = df.reg(F5_PIE_PWR_304);
    pwr_304.set(
        (pwr_304.get() & !0x420)
            | 0x3c0
            | bit(sockets == 1 || apob_flag, 1 << 10)
            | bit(apob_flag, 1 << 5),
    );

    if dies > 1 {
        let pwr_354 = df.reg(F5_PIE_PWR_354);
        pwr_354
            .set((pwr_354.get() & 0xffff_0000) | bit(sockets > 1, 0x7100) | bit(multi_die, 0x46));
    }
}

/// Sockets in the system, from DF1x200.
pub fn sockets(df0: &Df<impl ConfigRegionAccess>) -> u8 {
    df0.reg(v2::SYSTEM_CFG).read(SYSTEM_CFG::OTHER_SOCK) as u8 + 1
}

/// ReadyToBoot in the vendor code: "Disable extended configuration access".
pub fn disable_ext_cfg(df: &Df<impl ConfigRegionAccess>) {
    df.reg(regs::CFG_ACCESS_CTRL)
        .modify(CFG_ACCESS_CTRL::EXT_CFG::CLEAR);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn scrub_rate_for_16g_in_a_day() {
        // 16 GiB / 64 = 2^28 lines; 24 * 3.96e12 / 2^28 = 354_067 -> index 9.
        assert_eq!(scrub_rate(16 << 30, 24), 9);
        assert_eq!(scrub_rate(0, 24), 0);
    }
}
