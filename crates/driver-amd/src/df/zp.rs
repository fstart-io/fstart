// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Data Fabric init of the ZP die (Zeppelin, Naples): the vendor
//! AmdFabricZpPei/Dxe and the MMIO/IO distribution of AmdSocSp4ZpPei (sp4r2
//! reports DF_ZP and SOC_DISPATCH; the SP3 SOC_DF §3.6 "= SP4"). Each die
//! has its own DF node; what AGESA calls a broadcast access is a direct
//! access repeated on every die. The single-socket MMIO/IO distribution is
//! the [`super::plan`] ZP split ([`route_resources`]).
//!
//! Per die, in order: [`early_init`] (clock gating, CS, CCM, IOMS, PIE
//! RAS), [`scrub_init`], then at DXE entry [`late_init`] (PIE power
//! management); ReadyToBoot is [`disable_ext_cfg`].
use super::{
    Df, DfReg, Die, Version, map,
    plan::{self, Input, Plan, PlanError, Policy},
    program,
    regs::{self, CFG_ACCESS_CTRL, CLOCK_GATING, DRAM_HOLE, GLOBAL_CTRL},
    v2::{self, DRAM_BASE, DRAM_LIMIT, Df2, SYSTEM_CFG},
};
use crate::cpu::{Package, Revision};
use crate::{
    apob::{Apob, mem},
    soc::Soc,
};
use fstart_pci::ConfigRegionAccess;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    Ccm104 [
        B5 OFFSET(5) NUMBITS(1) [],
        B28 OFFSET(28) NUMBITS(1) []
    ],
    Ccm108 [B6 OFFSET(6) NUMBITS(1) []],
    Ioms04c [B5 OFFSET(5) NUMBITS(1) []],
    Ioms300 [B8 OFFSET(8) NUMBITS(1) []],
    Cfg0c0 [B17_12 OFFSET(12) NUMBITS(6) []],
    PieCfg [
        /// Erratum 1158 (LINT0/LINT1 while the APIC is software disabled).
        LINT_ERRATUM_1158 OFFSET(8) NUMBITS(1) [],
        DIS_SYNC_FLOOD_PROP OFFSET(19) NUMBITS(1) []
    ],
    ScrubBase [
        ENABLE OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(4) NUMBITS(28) []
    ],
    ScrubLimit [
        RATE OFFSET(0) NUMBITS(4) [],
        LIMIT OFFSET(4) NUMBITS(28) []
    ],
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

/// The fabric policy of AmdFabricZp (vendor PCDs); `Default` is the
/// vendors' default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FabricPolicy {
    /// Scrub period in hours, 0: the scrubber stays off (PCD 0x8e / SP3
    /// 0x65: 24).
    pub scrub_hours: u8,
    /// Redirect scrubbing to the populated channels (PCD 0x8f / SP3 0x66).
    pub scrub_redirect: bool,
    /// GLOBAL_CTRL\[5:4\] (SP3 PCD 0x67; `None`: keep).
    pub ras_a: Option<u8>,
    /// GLOBAL_CTRL's 3-bit RAS fields (SP3 PCD 0x68; `None`: keep).
    pub ras_b: Option<u8>,
    /// Sync flood on a fatal error stays off (IDS option 0x43: CBS
    /// default).
    pub sync_flood_disable: bool,
    /// Erratum 1158 (LINT0/LINT1 while the APIC is software disabled), from
    /// the revision guide; the vendor code doesn't set it.
    pub lint_erratum_1158: bool,
    /// "Global C-state Control" auto/enabled (SP3 PCD 0x10, SP4 0x13): the
    /// PIE keeps power-management bit 5 clear.
    pub global_cstate: bool,
}

impl Default for FabricPolicy {
    fn default() -> Self {
        Self {
            scrub_hours: 24,
            scrub_redirect: true,
            ras_a: None,
            ras_b: None,
            sync_flood_disable: false,
            lint_erratum_1158: true,
            global_cstate: true,
        }
    }
}

/// The vendor's non-PCI MMIO pool at the top of each die's window below
/// 4 GiB (AmdSocSp4ZpPei).
pub const NONPCI_MMIO_SIZE: u32 = 8 << 20;

/// Split MMIO and I/O over `dies` with the AGESA ZP equal split
/// (AmdSocSp4ZpPei) and route them on every die, base, limit and control
/// per rule.
pub fn route_resources<S: Soc, P: ConfigRegionAccess + Copy>(
    dies: &[Die<S, P>],
    (ecam, ecam_buses): (u64, u16),
    tom: u64,
    tom2: u64,
) -> Result<Plan, PlanError> {
    let root_bridges = super::root_bridges(dies, (ecam_buses - 1) as u8);
    let input = Input {
        tom,
        tom2,
        ecam_base: ecam,
        ecam_size: u64::from(ecam_buses) << 20,
        // The ZP split has a fixed above-4G limit.
        phys_addr_bits: 48,
        root_bridges: &root_bridges,
        mmio_pairs: Df2::MMIO_PAIRS,
    };
    let plan = plan::plan(&input, &Policy::df2())?;
    let nodes = nodes(dies);
    program::mmio::<Df2, _>(&nodes, &plan.mmio);
    program::io::<Df2, _>(&nodes, &plan.io);
    Ok(plan)
}

/// Route the legacy VGA ranges to die 0's IOMS on every die.
pub fn route_vga<S: Soc, P: ConfigRegionAccess + Copy>(dies: &[Die<S, P>]) {
    if let Some(first) = dies.first() {
        program::vga::<Df2, _>(&nodes(dies), first.ioms_fabric_id().into());
    }
}

fn nodes<S: Soc, P: ConfigRegionAccess + Copy>(
    dies: &[Die<S, P>],
) -> heapless::Vec<Df<P>, { super::MAX_NODES as usize }> {
    dies.iter().map(Die::df).collect()
}

/// The vendor code keeps the top 8 MiB of every die's below-4G window (pair
/// 2 * die) as its non-PCI MMIO pool. On die 0 that's 0xfe400000-0xfebfffff.
/// `None` if the die has no such window.
pub fn nonpci_mmio_base<S: Soc>(df0: &Df<impl ConfigRegionAccess>, die: u8) -> Option<u32> {
    let route = map::mmio_route(df0, 2 * die);
    let usable = route.enabled()
        && route.destination == S::ioms_fabric_id(die)
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

fn pie_ras_init(df: &Df<impl ConfigRegionAccess>, policy: &FabricPolicy) {
    df.reg(F5_PIE_CFG).modify(
        PieCfg::DIS_SYNC_FLOOD_PROP.val(policy.sync_flood_disable.into())
            + PieCfg::LINT_ERRATUM_1158.val(policy.lint_erratum_1158.into()),
    );
    map::clear_errors(df);
    for instance in 0..12 {
        let mut ras = GLOBAL_CTRL::BIT3::CLEAR;
        if let Some(a) = policy.ras_a.filter(|&a| a < 4) {
            ras += GLOBAL_CTRL::RAS_A.val(a.into());
        }
        if let Some(b) = policy.ras_b.filter(|&b| b < 8).map(u32::from) {
            ras += GLOBAL_CTRL::RAS_B0.val(b)
                + GLOBAL_CTRL::RAS_B1.val(b)
                + GLOBAL_CTRL::RAS_B2.val(b);
        }
        df.instance(regs::GLOBAL_CTRL, instance).modify(ras);
    }
}

/// AmdFabricZpPei, per die: `dies` is the system's.
pub fn early_init(
    df: &Df<impl ConfigRegionAccess>,
    dies: u8,
    revision: Revision,
    policy: &FabricPolicy,
) {
    clock_gating(df, dies == 1);
    ccm_ioms_init(df, revision == Revision::ZpA0);
    pie_ras_init(df, policy);
}

/// DRAM behind this die's scrubber, minus the MMIO hole if DRAM rule 0 or 1
/// (the PIE's copy) targets the die with the hole on (DF_ZP §3.8).
fn scrub_dram_size(df: &Df<impl ConfigRegionAccess>, socket: u8, die: u8) -> u64 {
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
        // The destination is the socket's die.
        if !dram_base.matches_all(DRAM_BASE::VALID::SET + DRAM_BASE::HOLE_EN::SET)
            || dram_limit.read(DRAM_LIMIT::DST_DIE) != u32::from(socket << 2 | die)
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

/// DRAM `[base, end)` and the die it belongs to, for SRAT.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DieMemory {
    pub base: u64,
    pub end: u64,
    pub die: u8,
}

/// The DRAM of every die as the vendor AmdFabricZpDxe reports it for SRAT
/// (DF_ZP §3.13, ACPI_ZP §3.4): the valid pairs of the system DRAM map
/// (IOMS instance, 16 pairs) of socket 0 die 0, split into
/// [0, 640 KiB), from 1 MiB up to the DRAM hole and the rest from 4 GiB.
/// `None` if a pair interleaves dies (they share one domain then, and
/// there is no NUMA to describe).
pub fn die_memory(df0: &Df<impl ConfigRegionAccess>) -> Option<heapless::Vec<DieMemory, 48>> {
    let hole = df0.instance(regs::DRAM_HOLE, v2::IOMS_INSTANCE).extract();
    let hole = hole
        .is_set(DRAM_HOLE::VALID)
        .then(|| u64::from(hole.read(DRAM_HOLE::BASE)) << 24);
    let mut ranges = heapless::Vec::new();
    for i in 0..v2::DRAM_RULES {
        let base = df0.instance(v2::dram_base(i), v2::IOMS_INSTANCE).extract();
        let limit = df0.instance(v2::dram_limit(i), v2::IOMS_INSTANCE).extract();
        if !base.is_set(DRAM_BASE::VALID) {
            continue;
        }
        if limit.read(DRAM_LIMIT::ILV_DIE) != 0 || limit.read(DRAM_LIMIT::ILV_SOCK) != 0 {
            return None;
        }
        let die = limit.read(DRAM_LIMIT::DST_DIE) as u8;
        let start = u64::from(base.read(DRAM_BASE::BASE)) << 28;
        let end = (u64::from(limit.read(DRAM_LIMIT::LIMIT)) + 1) << 28;
        let mut push = |base: u64, end: u64| {
            if end > base {
                ranges.push(DieMemory { base, end, die }).ok()
            } else {
                Some(())
            }
        };
        let mut from = start;
        if from == 0 {
            push(0, 0xa_0000)?;
            from = 1 << 20;
        }
        match hole.filter(|&hole| base.is_set(DRAM_BASE::HOLE_EN) && from < hole && end > 4 * GIB) {
            // The limit is a system address: the DRAM the hole covers is
            // remapped above 4 GiB, within it.
            Some(hole) => {
                push(from, hole)?;
                push(4 * GIB, end)?;
            }
            None => push(from, end)?,
        }
    }
    Some(ranges)
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

/// The populated UMC channels of a die, bit per channel, as the APOB's
/// memory configuration has them (inferred: a non-zero channel byte).
pub fn populated_channels(apob: &Apob, socket: u8, die: u8) -> u8 {
    mem::mem_config(apob, socket, die)
        .map(|config| {
            (0..2).fold(0, |present, channel| {
                present | u8::from(config.channel_byte(channel).is_ok_and(|b| b != 0)) << channel
            })
        })
        .unwrap_or(0)
}

/// The DRAM scrubber of the vendor AmdFabricZpPei over the `present`
/// channels ([`populated_channels`]); the ABL sets the scrub base and limit.
/// Returns the rate and the size, if the die has DRAM to scrub.
pub fn scrub_init(
    df: &Df<impl ConfigRegionAccess>,
    (socket, die): (u8, u8),
    present: u8,
    policy: &FabricPolicy,
) -> Option<(u8, u64)> {
    let size = scrub_dram_size(df, socket, die);
    if size == 0 || present & 3 == 0 {
        return None;
    }
    if policy.scrub_redirect {
        for instance in 0..2 {
            df.instance(F6_REDIR_SCRUB, instance)
                .set(u32::from(present >> instance & 1));
        }
    }
    if policy.scrub_hours == 0 {
        return None;
    }
    let rate = scrub_rate(size, policy.scrub_hours.into());
    df.reg(F6_SCRUB_LIMIT)
        .modify(ScrubLimit::RATE.val(rate.into()));
    df.reg(F6_SCRUB_BASE).modify(ScrubBase::ENABLE::SET);
    Some((rate, size))
}

/// The APOB's PIE power flag AmdFabricZpDxe tests from APOB version 9 on.
pub fn pie_power_flag(apob: &Apob) -> bool {
    apob.version() >= 9 && mem::pie_power(apob).is_ok_and(|pie| pie.flag() == Ok(true))
}

/// AmdFabricZpDxe: PIE power management, per die; `dies` and `sockets`
/// are the system's, `apob_flag` is [`pie_power_flag`]. The PIE power values
/// are the vendor's; their meaning isn't known.
pub fn late_init(
    df: &Df<impl ConfigRegionAccess>,
    (dies, sockets): (u8, u8),
    (revision, package): (Revision, Package),
    apob_flag: bool,
    policy: &FabricPolicy,
) {
    let multi_die = dies / sockets > 1;
    let bit = |cond: bool, mask: u32| if cond { mask } else { 0 };

    // Bit 5 stays clear with C-states enabled, unless the APOB asks for it
    // (or on an SP3 ZP-A0).
    let bit5 = !policy.global_cstate
        || apob_flag
        || (revision == Revision::ZpA0 && package == Package::Sp3);
    let pwr_300 = df.reg(F5_PIE_PWR_300);
    let mut v = pwr_300.get();
    v &= !((1 << 5) | (1 << 12) | (1 << 14) | (1 << 15));
    v |= bit(bit5, 1 << 5) | (1 << 13);
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
    fn die_memory_splits_like_the_vendor_srat() {
        use crate::df::testing::DfSpace;
        let space = DfSpace::default();
        let set = |offset: u16, value: u32| {
            space
                .regs
                .borrow_mut()
                .insert((0, offset, Some(v2::IOMS_INSTANCE)), value);
        };
        // Die 0: 0-16 GiB with the hole at 3 GiB (system addresses to 17
        // GiB); die 1: 17-33 GiB.
        set(0x104, 0xc000_0001);
        set(0x110, 0x0000_0003);
        set(0x114, (17 * 4 - 1) << 12);
        set(0x118, (17 * 4) << 12 | 1);
        set(0x11c, (33 * 4 - 1) << 12 | 1 << 5);
        let ranges = die_memory(&space.df()).unwrap();
        let ranges: std::vec::Vec<_> = ranges.iter().map(|r| (r.base, r.end, r.die)).collect();
        assert_eq!(
            ranges,
            [
                (0, 0xa_0000, 0),
                (1 << 20, 3 * GIB, 0),
                (4 * GIB, 17 * GIB, 0),
                (17 * GIB, 33 * GIB, 1),
            ]
        );
        // The scrubber's map (instance 5) doesn't count.
        space
            .regs
            .borrow_mut()
            .insert((0, 0x120, Some(INST_CS_DRAM_MAP)), 1);
        assert_eq!(die_memory(&space.df()).unwrap().len(), 4);
        // A later pair interleaving dies: one domain.
        set(0x170, (40 * 4) << 12 | 1);
        set(0x174, (41 * 4 - 1) << 12 | 1 << 10);
        assert_eq!(die_memory(&space.df()), None);
    }

    /// Four dies of one socket (an SP3 Naples), through the system's
    /// fabric.
    fn naples() -> (crate::df::model::DfModel, std::vec::Vec<crate::df::topology::Node>) {
        let nodes = (0..4)
            .map(|die| crate::df::topology::Node {
                socket: 0,
                die,
                device: 0x18 + die,
            })
            .collect();
        (crate::df::model::DfModel::default(), nodes)
    }

    #[test]
    fn four_dies_init_and_scrub() {
        let (m, nodes) = naples();
        let fabric = crate::df::Fabric::new(m.clone());
        m.set(1, None, 0, 0x40, 2);
        m.set(1, Some(1), 6, 0x1F4, 7);
        // Die 1: 64 GiB at [0, 66 GiB) with the 2 GiB hole, rule 1.
        m.set(1, None, 6, 0x48, 0);
        m.set(1, None, 6, 0x4C, ((66 << 10) - 1) << 4);
        m.set(1, Some(5), 0, 0x118, 0x3);
        m.set(1, Some(5), 0, 0x11C, 0x20);
        m.set(1, Some(5), 0, 0x104, 0x8000_0001);
        let policy = FabricPolicy::default();
        let dfs = unsafe { fabric.each(&nodes) };
        for (df, n) in dfs.iter().zip(&nodes) {
            early_init(df, 4, Revision::ZpB2, &policy);
            let present = if n.die == 1 { 0b10 } else { 0 };
            scrub_init(df, (n.socket, n.die), present, &policy);
        }
        // 4 dies: per-instance clock gating including IOMS (gate clear).
        assert_eq!(m.get(3, Some(18), 0, 0x3F0), 1);
        assert_eq!(m.get(2, Some(4), 0, 0x3F0), 1);
        assert_eq!(m.get(0, Some(2), 3, 0x104), 0x1000_0000);
        assert_eq!(m.get(1, Some(1), 6, 0x1F4), 0);
        // Die 1: redirect only on UMC1, rate for 64 GiB at 24 h (q = 88512 -> 7).
        assert_eq!(m.get(1, Some(0), 6, 0x58), 0);
        assert_eq!(m.get(1, Some(1), 6, 0x58), 1);
        assert_eq!(m.get(1, None, 6, 0x4C) & 0xF, 7);
        assert_eq!(m.get(1, None, 6, 0x48), 1);
        // Die 0 has no DRAM: scrubber untouched.
        assert_eq!(m.get(0, None, 6, 0x48), 0);
    }

    #[test]
    fn four_dies_pie_power_management() {
        let (m, nodes) = naples();
        let fabric = crate::df::Fabric::new(m.clone());
        let dfs = unsafe { fabric.each(&nodes) };
        for df in &dfs {
            let policy = FabricPolicy::default();
            late_init(df, (4, 1), (Revision::ZpB1, Package::Sp3), false, &policy);
        }
        // B1 with 4 dies: bit 12 clear, 13 and 14 set.
        assert_eq!(m.get(2, None, 5, 0x300), 0x6000);
        assert_eq!(m.get(2, None, 5, 0x358), 0x80E0_E0E0);
        assert_eq!(m.get(2, None, 5, 0x304), 0x7C0);
        assert_eq!(m.get(2, None, 5, 0x354), 0x46);
    }

    #[test]
    fn scrub_rate_for_16g_in_a_day() {
        // 16 GiB / 64 = 2^28 lines; 24 * 3.96e12 / 2^28 = 354_067 -> index 9.
        assert_eq!(scrub_rate(16 << 30, 24), 9);
        assert_eq!(scrub_rate(0, 24), 0);
    }
}
