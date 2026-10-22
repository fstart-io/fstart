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
    registers::{BROADCAST, Fabric},
};
use heapless::Vec;

// Function 0
const F0_INSTANCE_COUNT: u16 = 0x040;
const F0_VGA_EN: u16 = 0x080;
const F0_DRAM_HOLE: u16 = 0x104;
const fn f0_dram_base(i: u16) -> u16 {
    0x110 + 8 * i
}
const fn f0_dram_limit(i: u16) -> u16 {
    0x114 + 8 * i
}
const fn f0_io_base(i: u16) -> u16 {
    0x0c0 + 8 * i
}
const fn f0_io_limit(i: u16) -> u16 {
    0x0c4 + 8 * i
}
const fn f0_mmio_base(i: u16) -> u16 {
    0x200 + 0x10 * i
}
const fn f0_mmio_limit(i: u16) -> u16 {
    0x204 + 0x10 * i
}
const fn f0_mmio_ctrl(i: u16) -> u16 {
    0x208 + 0x10 * i
}
const F0_CLK_GATE: u16 = 0x3f0;
const F0_RAS_CTRL: u16 = 0x3f8;
const F1_SYSTEM_CFG: u16 = 0x200;
const F2_CS_CFG_68: u16 = 0x068;
const F2_CS_CFG_6C: u16 = 0x06c;
const F3_IOMS_04C: u16 = 0x04c;
const F3_CCM_104: u16 = 0x104;
const F3_CCM_108: u16 = 0x108;
const F3_IOMS_300: u16 = 0x300;
const F4_EXT_CFG: u16 = 0x044;
const F4_CFG_0C0: u16 = 0x0c0;
const F4_CCM_0C4: u16 = 0x0c4;
const F4_CFG_0C8: u16 = 0x0c8;
const F5_PIE_CFG: u16 = 0x104;
const PIE_CFG_LINT_ERRATUM_1158: u32 = 1 << 8;
const PIE_CFG_DIS_SYNC_FLOOD_PROP: u32 = 1 << 19;
const F5_PIE_PWR_300: u16 = 0x300;
const F5_PIE_PWR_304: u16 = 0x304;
const F5_PIE_PWR_354: u16 = 0x354;
const F5_PIE_PWR_358: u16 = 0x358;
/// [31:4] base >> 20, [0] enable.
const F6_SCRUB_BASE: u16 = 0x048;
/// [31:4] limit >> 20, [3:0] rate.
const F6_SCRUB_LIMIT: u16 = 0x04c;
/// Per UMC instance.
const F6_REDIR_SCRUB: u16 = 0x058;
const F6_ERR_STATUS_0: u16 = 0x1f0;
const F6_ERR_STATUS_1: u16 = 0x1f4;
const F6_CLK_GATE_DC: u16 = 0x0dc;

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
pub fn program(plan: &Plan, dfs: &mut [&mut dyn Fabric]) {
    fn write_all(dfs: &mut [&mut dyn Fabric], offset: u16, value: u32) {
        for df in dfs.iter_mut() {
            df.write(0, offset, BROADCAST, value);
        }
    }
    for w in &plan.mmio {
        let pair = u16::from(w.pair);
        let old = dfs[0].read(0, f0_mmio_ctrl(pair), BROADCAST);
        let ctrl = (old & !0xff0) | (u32::from(ioms_fabric_id(w.die)) << 4) | 3;
        write_all(dfs, f0_mmio_base(pair), ((w.base + 0xffff) >> 16) as u32);
        write_all(dfs, f0_mmio_limit(pair), ((w.end() - 1) >> 16) as u32);
        write_all(dfs, f0_mmio_ctrl(pair), ctrl);
    }
    for w in &plan.io {
        let pair = u16::from(w.pair);
        let end = (w.end() - 1) as u32;
        let old = dfs[0].read(0, f0_io_limit(pair), BROADCAST);
        write_all(dfs, f0_io_base(pair), (w.base as u32 & 0x1ff_f000) | 3);
        write_all(
            dfs,
            f0_io_limit(pair),
            (old & 0xfe00_0f00) | u32::from(ioms_fabric_id(w.die)) | (end & 0x1ff_f000),
        );
    }
}

/// The vendor code keeps the top 8 MiB of every die's below-4G window (pair
/// 2 * die) as its non-PCI MMIO pool. On die 0 that's 0xfe400000-0xfebfffff.
pub fn nonpci_mmio_base(df0: &mut impl Fabric, die: u8) -> Result<u32, Error> {
    let pair = 2 * u16::from(die);
    let ctrl = df0.read(0, f0_mmio_ctrl(pair), BROADCAST);
    let base = u64::from(df0.read(0, f0_mmio_base(pair), BROADCAST)) << 16;
    let top = (u64::from(df0.read(0, f0_mmio_limit(pair), BROADCAST)) + 1) << 16;
    if ctrl & 3 != 3
        || ((ctrl >> 4) & 0xff) as u8 != ioms_fabric_id(die)
        || top > 4 * GIB
        || top - base < u64::from(NONPCI_MMIO_SIZE)
    {
        return Err(Error::NoBelow4g(die));
    }
    Ok((top - u64::from(NONPCI_MMIO_SIZE)) as u32)
}

/// The DF MMIO windows routed to `die`, for PCI resource allocation.
pub fn mmio_windows(df0: &mut impl Fabric, die: u8) -> Vec<(u64, u64), MMIO_PAIRS> {
    (0..MMIO_PAIRS as u16)
        .filter_map(|pair| {
            let ctrl = df0.read(0, f0_mmio_ctrl(pair), BROADCAST);
            if ctrl & 3 != 3 || ((ctrl >> 4) & 0xff) as u8 != ioms_fabric_id(die) {
                return None;
            }
            let base = u64::from(df0.read(0, f0_mmio_base(pair), BROADCAST)) << 16;
            let end = (u64::from(df0.read(0, f0_mmio_limit(pair), BROADCAST)) + 1) << 16;
            Some((base, end - base))
        })
        .collect()
}

fn clock_gating(df: &mut impl Fabric, single_die: bool) {
    const INSTANCES: [u8; 12] = [0, 1, 2, 3, 5, 12, 13, 14, 15, 16, 17, 18];
    let gate = df.read(6, F6_CLK_GATE_DC, INST_IOMS) & 1 != 0;
    if !gate && single_die {
        df.write(0, F0_CLK_GATE, BROADCAST, 1);
        return;
    }
    for instance in INSTANCES {
        df.write(0, F0_CLK_GATE, instance, 1);
    }
    if !gate {
        df.write(0, F0_CLK_GATE, INST_IOMS, 1);
    }
}

fn ccm_ioms_init(df: &mut impl Fabric, rev_a0: bool) {
    // CS
    df.write(2, F2_CS_CFG_68, BROADCAST, 0x0006_5019);
    df.write(2, F2_CS_CFG_6C, BROADCAST, 0x1004_0611);
    // CCM
    for instance in [INST_CCM0, INST_CCM1] {
        df.write(4, F4_CCM_0C4, instance, 0x0002_0001);
    }
    df.write(4, F4_CFG_0C8, BROADCAST, 0x0102_0000);
    for instance in [INST_CCM0, INST_CCM1] {
        df.update(4, F4_CFG_0C0, instance, 0x3_f000, 0);
        df.update(
            3,
            F3_CCM_104,
            instance,
            1 << 5,
            (1 << 28) | if rev_a0 { 1 << 5 } else { 0 },
        );
        df.update(3, F3_CCM_108, instance, 0, 1 << 6);
    }
    // IOMS
    df.update(3, F3_IOMS_04C, INST_IOMS, 0, 1 << 5);
    df.update(3, F3_IOMS_300, INST_IOMS, 0, 1 << 8);
    df.update(4, F4_CFG_0C0, INST_IOMS, 0x3_f000, if rev_a0 { 0x1e << 12 } else { 0 });
}

fn pie_ras_init(df: &mut impl Fabric) {
    let instances = (df.read(0, F0_INSTANCE_COUNT, BROADCAST) & 0xff) as u8;
    // Sync flood propagation stays enabled (vendor default). Bit 8 is the
    // workaround for erratum 1158 (LINT0/LINT1 while the APIC is software
    // disabled) from the revision guide; the vendor code doesn't set it.
    df.update(
        5,
        F5_PIE_CFG,
        BROADCAST,
        PIE_CFG_DIS_SYNC_FLOOD_PROP,
        PIE_CFG_LINT_ERRATUM_1158,
    );
    // Clear the error status left from before the reset.
    for instance in 0..instances {
        for status in [F6_ERR_STATUS_0, F6_ERR_STATUS_1] {
            if df.read(6, status, instance) != 0 {
                df.write(6, status, instance, 0);
            }
        }
    }
    for instance in 0..12 {
        df.update(0, F0_RAS_CTRL, instance, 1 << 3, 0);
    }
}

/// AmdFabricZpPei, per die.
pub fn early_init(df: &mut impl Fabric, dies: u8, revision: Revision) {
    clock_gating(df, dies == 1);
    ccm_ioms_init(df, revision == Revision::A0);
    pie_ras_init(df);
}

/// DRAM behind this die's scrubber, minus the MMIO hole if it is in range.
fn scrub_dram_size(df: &mut impl Fabric, die: u8) -> u64 {
    let base = df.read(6, F6_SCRUB_BASE, BROADCAST);
    let limit = df.read(6, F6_SCRUB_LIMIT, BROADCAST);
    if limit == 0 {
        return 0;
    }
    let mut size = u64::from((limit >> 4).wrapping_sub(base >> 4).wrapping_add(1)) << 20;
    for i in 0..2 {
        let dram_base = df.read(0, f0_dram_base(i), INST_CS_DRAM_MAP);
        let dram_limit = df.read(0, f0_dram_limit(i), INST_CS_DRAM_MAP);
        // Socket 0: the destination is the die number.
        if dram_base & 1 == 0 || (dram_limit & 0xe0) != u32::from(die) << 5 || dram_base & 2 == 0 {
            continue;
        }
        let hole = df.read(0, F0_DRAM_HOLE, INST_CS_DRAM_MAP);
        if hole & 1 != 0 {
            size -= 4 * GIB - u64::from(hole & 0xff00_0000);
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
pub fn scrub_init(df: &mut impl Fabric, die: u8, apob: &Apob) -> Option<(u8, u64)> {
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
        df.write(6, F6_REDIR_SCRUB, instance as u8, u32::from(populated != 0));
    }
    let rate = scrub_rate(size, 24);
    df.update(6, F6_SCRUB_LIMIT, BROADCAST, 0xf, u32::from(rate));
    df.update(6, F6_SCRUB_BASE, BROADCAST, 0, 1);
    Some((rate, size))
}

/// AmdFabricZpDxe: PIE power management, per die.
pub fn late_init(df: &mut impl Fabric, dies: u8, sockets: u8, revision: Revision, apob: &Apob) {
    let multi_die = dies / sockets > 1;
    // APOB group 1 type 0xf, byte 0x130 (meaning unknown), APOB version 9 and up.
    let apob_flag = apob_byte(apob, APOB_TYPE_PIE_PWR, None, 0x130, 9).is_some_and(|b| b != 0);
    let bit = |cond: bool, mask: u32| if cond { mask } else { 0 };

    // Bit 5 stays clear with C6 enabled (vendor PCD 0x13 = 1).
    let mut v = df.read(5, F5_PIE_PWR_300, BROADCAST);
    v &= !((1 << 5) | (1 << 12) | (1 << 14) | (1 << 15));
    v |= bit(apob_flag, 1 << 5) | (1 << 13);
    let early_rev = matches!(revision, Revision::A0 | Revision::B0 | Revision::B1);
    v |= bit(!(dies > 1 && early_rev), 1 << 12);
    v |= bit(multi_die, 1 << 14) | bit(sockets > 1, 1 << 15);
    df.write(5, F5_PIE_PWR_300, BROADCAST, v);

    let v = df.read(5, F5_PIE_PWR_358, BROADCAST);
    let v = (v & 0x0f00_0000) | 0x80e0_00e0 | bit(multi_die, 0x0000_e000);
    df.write(5, F5_PIE_PWR_358, BROADCAST, v);

    let v = df.read(5, F5_PIE_PWR_304, BROADCAST);
    let v = (v & !0x420) | 0x3c0 | bit(sockets == 1 || apob_flag, 1 << 10) | bit(apob_flag, 1 << 5);
    df.write(5, F5_PIE_PWR_304, BROADCAST, v);

    if dies > 1 {
        let v = df.read(5, F5_PIE_PWR_354, BROADCAST);
        let v = (v & 0xffff_0000) | bit(sockets > 1, 0x7100) | bit(multi_die, 0x46);
        df.write(5, F5_PIE_PWR_354, BROADCAST, v);
    }
}

/// Sockets in the system, from DF1x200.
pub fn sockets(df0: &mut impl Fabric) -> u8 {
    (((df0.read(1, F1_SYSTEM_CFG, BROADCAST) >> 5) & 1) + 1) as u8
}

/// Route the legacy VGA ranges to the IOMS of `target`; call on every die's DF.
pub fn route_vga(df: &mut impl Fabric, target: u8) {
    df.update(0, F0_VGA_EN, BROADCAST, 0xff4, (u32::from(ioms_fabric_id(target)) << 4) | 1);
}

/// ReadyToBoot in the vendor code: "Disable extended configuration access".
pub fn disable_ext_cfg(df: &mut impl Fabric) {
    df.update(4, F4_EXT_CFG, BROADCAST, 1, 0);
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
