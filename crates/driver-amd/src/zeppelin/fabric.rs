// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin Data Fabric initialization. The sequences follow the vendor
//! AmdFabricZpPei/Dxe and the MMIO/IO distribution of AmdSocSp4ZpPei (sp4r2
//! reports DF_ZP and SOC_DISPATCH). Each die has its own DF; what AGESA calls a
//! broadcast access is a direct access repeated on every die.
use super::{
    Revision,
    topology::{NONPCI_MMIO_SIZE, ioms_fabric_id},
};
use crate::fam17h::{
    apob::Apob,
    df::{
        IoBase, IoLimit, MmioCtrl, VGA_EN as F0_VGA_EN, VgaEn, io_base as f0_io_base,
        io_limit as f0_io_limit, mmio_base as f0_mmio_base, mmio_ctrl as f0_mmio_ctrl,
        mmio_limit as f0_mmio_limit,
    },
    registers::{Df, DfReg},
};
use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u32,
    InstanceCount [COUNT OFFSET(0) NUMBITS(8) []],
    DramHole [VALID OFFSET(0) NUMBITS(1) [], BASE OFFSET(24) NUMBITS(8) []],
    DramBase [VALID OFFSET(0) NUMBITS(1) [], HOLE_EN OFFSET(1) NUMBITS(1) []],
    DramLimit [DST_DIE OFFSET(5) NUMBITS(3) []],
    ClkGate [ENABLE OFFSET(0) NUMBITS(1) []],
    RasCtrl [B3 OFFSET(3) NUMBITS(1) []],
    SystemCfg [MULTI_SOCKET OFFSET(5) NUMBITS(1) []],
    Ccm104 [B5 OFFSET(5) NUMBITS(1) [], B28 OFFSET(28) NUMBITS(1) []],
    Ccm108 [B6 OFFSET(6) NUMBITS(1) []],
    Ioms04c [B5 OFFSET(5) NUMBITS(1) []],
    Ioms300 [B8 OFFSET(8) NUMBITS(1) []],
    Cfg0c0 [B17_12 OFFSET(12) NUMBITS(6) []],
    ExtCfg [ENABLE OFFSET(0) NUMBITS(1) []],
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
const F0_INSTANCE_COUNT: DfReg<InstanceCount::Register> = DfReg::new(0, 0x040);
const F0_DRAM_HOLE: DfReg<DramHole::Register> = DfReg::new(0, 0x104);
const fn f0_dram_base(i: u16) -> DfReg<DramBase::Register> {
    DfReg::new(0, 0x110 + 8 * i)
}
const fn f0_dram_limit(i: u16) -> DfReg<DramLimit::Register> {
    DfReg::new(0, 0x114 + 8 * i)
}
const F0_CLK_GATE: DfReg<ClkGate::Register> = DfReg::new(0, 0x3f0);
const F0_RAS_CTRL: DfReg<RasCtrl::Register> = DfReg::new(0, 0x3f8);
const F1_SYSTEM_CFG: DfReg<SystemCfg::Register> = DfReg::new(1, 0x200);
const F2_CS_CFG_68: DfReg = DfReg::new(2, 0x068);
const F2_CS_CFG_6C: DfReg = DfReg::new(2, 0x06c);
const F3_IOMS_04C: DfReg<Ioms04c::Register> = DfReg::new(3, 0x04c);
const F3_CCM_104: DfReg<Ccm104::Register> = DfReg::new(3, 0x104);
const F3_CCM_108: DfReg<Ccm108::Register> = DfReg::new(3, 0x108);
const F3_IOMS_300: DfReg<Ioms300::Register> = DfReg::new(3, 0x300);
const F4_EXT_CFG: DfReg<ExtCfg::Register> = DfReg::new(4, 0x044);
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
const F6_ERR_STATUS_0: DfReg = DfReg::new(6, 0x1f0);
const F6_ERR_STATUS_1: DfReg = DfReg::new(6, 0x1f4);
const F6_CLK_GATE_DC: DfReg<ClkGateDc::Register> = DfReg::new(6, 0x0dc);

const INST_CCM0: u8 = 2;
const INST_CCM1: u8 = 3;
const INST_IOMS: u8 = 4;
const INST_CS_DRAM_MAP: u8 = 5;

pub const MMIO_PAIRS: usize = 8;
pub const IO_PAIRS: usize = 8;
const ALIGN_16M: u64 = !0xff_ffff;
const GIB: u64 = 1 << 30;

const APOB_GROUP_MEMORY: u32 = 1;
const APOB_TYPE_MEM_CONFIG: u32 = 2;
const APOB_TYPE_PIE_PWR: u32 = 0xf;
/// Size of the APOB entry header the vendor offsets count from.
const APOB_ENTRY_HEADER: usize = 48;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// TOM or the ECAM window are outside the DF MMIO model.
    Inputs,
    /// No room for a die's MMIO window.
    NoSpace(u8),
    TooManyWindows,
    /// A die has no usable below-4G DF window for its non-PCI MMIO pool.
    NoBelow4g(u8),
}

/// A DF MMIO or I/O base/limit pair routed to a die's IOMS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub pair: u8,
    pub base: u64,
    pub size: u64,
    pub die: u8,
}
impl Window {
    pub const fn end(&self) -> u64 {
        self.base + self.size
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Plan {
    pub mmio: Vec<Window, MMIO_PAIRS>,
    pub io: Vec<Window, IO_PAIRS>,
}

fn push(windows: &mut Vec<Window, MMIO_PAIRS>, pair: usize, base: u64, size: u64, die: u8) -> Result<(), Error> {
    if pair >= MMIO_PAIRS {
        return Err(Error::TooManyWindows);
    }
    windows
        .push(Window {
            pair: pair as u8,
            base,
            size,
            die,
        })
        .map_err(|_| Error::TooManyWindows)
}

/// The vendor MMIO/IO distribution for one socket with `n` dies.
///
/// The windows below 4 GiB split the space between TOM and the PCIe
/// configuration space and the space between the configuration space and
/// 0xfe000000 between the dies; die 0's window also covers up to 0xfebfffff.
/// Above 4 GiB, every die gets an equal part of the larger of
/// [TOM2, 0xfd_0000_0000) and [1 TiB, 0x7fd_0000_0000); the last die gets the
/// rest up to 2^48. A spare window routes the HPET range to die 0.
pub fn plan(tom: u64, tom2: u64, cfg_size: u64, n: u8) -> Result<Plan, Error> {
    let cfg_end = 0xf000_0000 + cfg_size;
    if cfg_end > 0xfe00_0000 || tom > 0xf000_0000 || n == 0 || n > 8 {
        return Err(Error::Inputs);
    }
    let nn = u64::from(n);
    let above = 0xfe00_0000 - cfg_end;
    let below = 0xf000_0000 - tom;
    let mut total = above + below;
    let (mut above_small, mut below_small) = (false, false);
    if n > 1 {
        if above < (below / nn) * 8 / 10 {
            total -= above;
            above_small = true;
        }
        if below < (above / nn) * 8 / 10 {
            total -= below;
            below_small = true;
        }
        if above_small && below_small {
            return Err(Error::NoSpace(0));
        }
    }
    let mut plan = Plan::default();
    let per = total / nn;
    let per_al = per & ALIGN_16M;
    let (dies_above, mut size, mut cursor) = if above_small {
        (0, below - per_al * (nn - 1), 0xf000_0000u64)
    } else {
        let mut dies_above = above / (per & 0xffff_ffff);
        if above - per * dies_above > per / 2 {
            dies_above += 1;
        }
        let dies_above = dies_above.max(1);
        (dies_above, above - per_al * (dies_above - 1), 0xfe00_0000u64)
    };
    let mut left = dies_above;
    let mut below_used = 0u32;
    let mut per_b_al = 0;
    for k in 0..u64::from(n) {
        below_used |= 1 << k;
        if left == 0 {
            if below_small {
                return Err(Error::NoSpace(k as u8));
            }
            if k == dies_above {
                let m = nn - k;
                per_b_al = (below / m) & ALIGN_16M;
                size = below - per_b_al * (m - 1);
                cursor = 0xf000_0000;
            }
            if k == dies_above + 1 {
                size = per_b_al;
            }
        } else {
            if k == 1 {
                size = per_al;
            }
            left -= 1;
        }
        cursor -= size;
        let window = if k == 0 && !above_small {
            size + 0xc0_0000
        } else {
            size
        };
        push(&mut plan.mmio, 2 * k as usize, cursor, window, k as u8)?;
    }

    let mut second = None;
    if n < 8 && ((above_small && above != 0) || (below != 0 && (below_small || n == 1))) {
        let d = (0..8).find(|d| below_used & (1 << d) == 0).ok_or(Error::TooManyWindows)?;
        second = Some(d);
        if above_small {
            push(&mut plan.mmio, 2 * d, cfg_end, 0xfec0_0000 - cfg_end, 0)?;
        } else {
            push(&mut plan.mmio, 2 * d, tom, below, 0)?;
        }
    }

    // Above 4 GiB, skipping the HyperTransport hole at 0xfd_0000_0000.
    let mut above_used = 0u32;
    let tib = 1u64 << 40;
    let mut base = tom2.max(4 * GIB);
    let mut limit = 0x7fdu64 << 32;
    let size1 = if (base >> 32) < 0xfd { (0xfd << 32) - base } else { 0 };
    let size2 = if limit > tib { limit - base.max(tib) } else { 0 };
    if size1 == 0 && size2 == 0 {
        limit = 0;
    } else if size2 <= size1 {
        limit = 0xfd << 32;
    } else {
        base = tom2.max(tib);
    }
    if limit > base {
        let span = limit - base;
        let per_die = (span / nn) & ALIGN_16M;
        for k in 0..n {
            let last = k == n - 1;
            let size = if last { span - per_die * (nn - 1) } else { per_die };
            above_used |= 1 << k;
            if size >= 0x1_0000 {
                let window = if last { (1 << 48) - base } else { size };
                push(&mut plan.mmio, 2 * usize::from(k) + 1, base, window, k)?;
            }
            base += size;
        }
    }

    // HPET: the first unused below-4G pair, else the first unused above-4G pair.
    for d in 0..8usize {
        if below_used & (1 << d) == 0 && second != Some(d) {
            push(&mut plan.mmio, 2 * d, 0xfed0_0000, 0x1_0000, 0)?;
            break;
        }
        if below_used & (1 << d) != 0 && above_used & (1 << d) == 0 {
            push(&mut plan.mmio, 2 * d + 1, 0xfed0_0000, 0x1_0000, 0)?;
            break;
        }
    }

    // I/O: die 0 gets an extra 4 KiB, the last die the rest of the 32 MiB space.
    let q = (0xf000 / u64::from(n)) & !0xfff;
    let mut io_cursor = 0;
    for k in 0..n {
        let size = if k == n - 1 {
            0x200_0000 - io_cursor
        } else if k == 0 {
            q + 0x1000
        } else {
            q
        };
        plan.io
            .push(Window {
                pair: k,
                base: io_cursor,
                size,
                die: k,
            })
            .map_err(|_| Error::TooManyWindows)?;
        io_cursor += size;
    }
    Ok(plan)
}

/// Program the plan into every die's DF (`dfs[die]`).
pub fn program<P: ConfigRegionAccess>(plan: &Plan, dfs: &[Df<P>]) {
    let Some(df0) = dfs.first() else { return };
    for w in &plan.mmio {
        let pair = u16::from(w.pair);
        let mut ctrl = df0.reg(f0_mmio_ctrl(pair)).extract();
        ctrl.modify(
            MmioCtrl::DST_FABRIC_ID.val(ioms_fabric_id(w.die).into())
                + MmioCtrl::RE::SET
                + MmioCtrl::WE::SET,
        );
        // Base, limit and control each on every die, in that order.
        dfs.iter()
            .for_each(|df| df.reg(f0_mmio_base(pair)).set(((w.base + 0xffff) >> 16) as u32));
        dfs.iter()
            .for_each(|df| df.reg(f0_mmio_limit(pair)).set(((w.end() - 1) >> 16) as u32));
        dfs.iter()
            .for_each(|df| df.reg(f0_mmio_ctrl(pair)).set(ctrl.get()));
    }
    for w in &plan.io {
        let pair = u16::from(w.pair);
        let end = (w.end() - 1) as u32;
        let mut limit = df0.reg(f0_io_limit(pair)).extract();
        limit.modify(
            IoLimit::DST_FABRIC_ID.val(ioms_fabric_id(w.die).into())
                + IoLimit::LIMIT.val(end >> 12),
        );
        let base = IoBase::RE::SET + IoBase::WE::SET + IoBase::BASE.val(w.base as u32 >> 12);
        dfs.iter()
            .for_each(|df| df.reg(f0_io_base(pair)).write(base));
        dfs.iter()
            .for_each(|df| df.reg(f0_io_limit(pair)).set(limit.get()));
    }
}

/// The MMIO window of `pair` if it is enabled and routed to `die`.
fn mmio_window(df0: &Df<impl ConfigRegionAccess>, pair: u16, die: u8) -> Option<(u64, u64)> {
    let ctrl = df0.reg(f0_mmio_ctrl(pair)).extract();
    if !ctrl.matches_all(MmioCtrl::RE::SET + MmioCtrl::WE::SET)
        || ctrl.read(MmioCtrl::DST_FABRIC_ID) != u32::from(ioms_fabric_id(die))
    {
        return None;
    }
    let base = u64::from(df0.reg(f0_mmio_base(pair)).get()) << 16;
    let end = (u64::from(df0.reg(f0_mmio_limit(pair)).get()) + 1) << 16;
    Some((base, end - base))
}

/// The vendor code keeps the top 8 MiB of every die's below-4G window (pair
/// 2 * die) as its non-PCI MMIO pool. On die 0 that's 0xfe400000-0xfebfffff.
pub fn nonpci_mmio_base(df0: &Df<impl ConfigRegionAccess>, die: u8) -> Result<u32, Error> {
    match mmio_window(df0, 2 * u16::from(die), die) {
        Some((base, size)) if base + size <= 4 * GIB && size >= u64::from(NONPCI_MMIO_SIZE) => {
            Ok((base + size - u64::from(NONPCI_MMIO_SIZE)) as u32)
        }
        _ => Err(Error::NoBelow4g(die)),
    }
}

/// The DF MMIO windows routed to `die`, for PCI resource allocation.
pub fn mmio_windows(df0: &Df<impl ConfigRegionAccess>, die: u8) -> Vec<(u64, u64), MMIO_PAIRS> {
    (0..MMIO_PAIRS as u16)
        .filter_map(|pair| mmio_window(df0, pair, die))
        .collect()
}

fn clock_gating(df: &Df<impl ConfigRegionAccess>, single_die: bool) {
    const INSTANCES: [u8; 12] = [0, 1, 2, 3, 5, 12, 13, 14, 15, 16, 17, 18];
    let gate = df
        .instance(F6_CLK_GATE_DC, INST_IOMS)
        .is_set(ClkGateDc::IOMS);
    if !gate && single_die {
        df.reg(F0_CLK_GATE).write(ClkGate::ENABLE::SET);
        return;
    }
    for instance in INSTANCES {
        df.instance(F0_CLK_GATE, instance)
            .write(ClkGate::ENABLE::SET);
    }
    if !gate {
        df.instance(F0_CLK_GATE, INST_IOMS)
            .write(ClkGate::ENABLE::SET);
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
        df.instance(F3_CCM_108, instance)
            .modify(Ccm108::B6::SET);
    }
    // IOMS
    df.instance(F3_IOMS_04C, INST_IOMS)
        .modify(Ioms04c::B5::SET);
    df.instance(F3_IOMS_300, INST_IOMS)
        .modify(Ioms300::B8::SET);
    df.instance(F4_CFG_0C0, INST_IOMS)
        .modify(Cfg0c0::B17_12.val(if rev_a0 { 0x1e } else { 0 }));
}

fn pie_ras_init(df: &Df<impl ConfigRegionAccess>) {
    let instances = df.reg(F0_INSTANCE_COUNT).read(InstanceCount::COUNT) as u8;
    // Sync flood propagation stays enabled (vendor default). The erratum
    // 1158 workaround is from the revision guide; the vendor code doesn't
    // set it.
    df.reg(F5_PIE_CFG)
        .modify(PieCfg::DIS_SYNC_FLOOD_PROP::CLEAR + PieCfg::LINT_ERRATUM_1158::SET);
    // Clear the error status left from before the reset.
    for instance in 0..instances {
        for status in [F6_ERR_STATUS_0, F6_ERR_STATUS_1] {
            let status = df.instance(status, instance);
            if status.get() != 0 {
                status.set(0);
            }
        }
    }
    for instance in 0..12 {
        df.instance(F0_RAS_CTRL, instance)
            .modify(RasCtrl::B3::CLEAR);
    }
}

/// AmdFabricZpPei, per die.
pub fn early_init(df: &Df<impl ConfigRegionAccess>, dies: u8, revision: Revision) {
    clock_gating(df, dies == 1);
    ccm_ioms_init(df, revision == Revision::A0);
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
        let dram_base = df.instance(f0_dram_base(i), INST_CS_DRAM_MAP).extract();
        let dram_limit = df.instance(f0_dram_limit(i), INST_CS_DRAM_MAP).extract();
        // Socket 0: the destination is the die number.
        if !dram_base.matches_all(DramBase::VALID::SET + DramBase::HOLE_EN::SET)
            || dram_limit.read(DramLimit::DST_DIE) != u32::from(die)
        {
            continue;
        }
        let hole = df.instance(F0_DRAM_HOLE, INST_CS_DRAM_MAP).extract();
        if hole.is_set(DramHole::VALID) {
            size -= 4 * GIB - (u64::from(hole.read(DramHole::BASE)) << 24);
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

fn apob_byte(apob: &Apob, kind: u32, instance: Option<u32>, offset: usize, min_version: u32) -> Option<u8> {
    if apob.version() < min_version {
        return None;
    }
    apob.find_instance(APOB_GROUP_MEMORY, kind, instance)?
        .get(offset.checked_sub(APOB_ENTRY_HEADER)?)
        .copied()
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
    let populated = [0x38, 0x39]
        .map(|offset| apob_byte(apob, APOB_TYPE_MEM_CONFIG, Some(u32::from(die)), offset, 0));
    let [Some(a), Some(b)] = populated else {
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
    // APOB group 1 type 0xf, byte 0x130 (meaning unknown), APOB version 9 and up.
    let apob_flag = apob_byte(apob, APOB_TYPE_PIE_PWR, None, 0x130, 9).is_some_and(|b| b != 0);
    let bit = |cond: bool, mask: u32| if cond { mask } else { 0 };

    // Bit 5 stays clear with C6 enabled (vendor PCD 0x13 = 1).
    let pwr_300 = df.reg(F5_PIE_PWR_300);
    let mut v = pwr_300.get();
    v &= !((1 << 5) | (1 << 12) | (1 << 14) | (1 << 15));
    v |= bit(apob_flag, 1 << 5) | (1 << 13);
    let early_rev = matches!(revision, Revision::A0 | Revision::B0 | Revision::B1);
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
        pwr_354.set((pwr_354.get() & 0xffff_0000) | bit(sockets > 1, 0x7100) | bit(multi_die, 0x46));
    }
}

/// Sockets in the system, from DF1x200.
pub fn sockets(df0: &Df<impl ConfigRegionAccess>) -> u8 {
    df0.reg(F1_SYSTEM_CFG).read(SystemCfg::MULTI_SOCKET) as u8 + 1
}

/// Route the legacy VGA ranges to the IOMS of `target`; call on every die's DF.
pub fn route_vga(df: &Df<impl ConfigRegionAccess>, target: u8) {
    df.reg(F0_VGA_EN).modify(
        VgaEn::ENABLE::SET
            + VgaEn::CPU_DIS::CLEAR
            + VgaEn::DST_FABRIC_ID.val(ioms_fabric_id(target).into()),
    );
}

/// ReadyToBoot in the vendor code: "Disable extended configuration access".
pub fn disable_ext_cfg(df: &Df<impl ConfigRegionAccess>) {
    df.reg(F4_EXT_CFG).modify(ExtCfg::ENABLE::CLEAR);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn mj11_single_die_plan() {
        // MJ11-EC1: TOM 0xe0000000, TOM2 0x420000000, 128-bus ECAM.
        let plan = plan(0xe000_0000, 0x4_2000_0000, 128 << 20, 1).unwrap();
        let mmio: std::vec::Vec<_> = plan
            .mmio
            .iter()
            .map(|w| (w.pair, w.base, w.end(), w.die))
            .collect();
        assert_eq!(
            mmio,
            [
                // Die 0: [ECAM end, 0xfe000000) plus the 12 MiB up to 0xfec00000.
                (0, 0xf800_0000, 0xfec0_0000, 0),
                // The space between TOM and the ECAM.
                (2, 0xe000_0000, 0xf000_0000, 0),
                // [1 TiB, 0x7fd_0000_0000) is larger than [TOM2, 0xfd_0000_0000).
                (1, 1 << 40, 1 << 48, 0),
                (4, 0xfed0_0000, 0xfed1_0000, 0),
            ]
        );
        assert_eq!(
            plan.io.as_slice(),
            [Window {
                pair: 0,
                base: 0,
                size: 0x200_0000,
                die: 0
            }]
        );
    }

    #[test]
    fn scrub_rate_for_16g_in_a_day() {
        // 16 GiB / 64 = 2^28 lines; 24 * 3.96e12 / 2^28 = 354_067 -> index 9.
        assert_eq!(scrub_rate(16 << 30, 24), 9);
        assert_eq!(scrub_rate(0, 24), 0);
    }
}
