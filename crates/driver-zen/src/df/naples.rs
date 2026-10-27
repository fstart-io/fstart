//! Naples (ZP, DF2) fabric init: the AmdFabricZpPei steps on every die of
//! every socket (SOC_DF §3.6 "= SP4", SP4r2 DF_ZP §3.3–3.9) and the DXE PIE
//! power management (DF_ZP §3.9, SP3 PCD 0x10 delta).
//!
//! Order (PEI): clock gating → CS → CCM → IOMS → PIE RAS → scrubber.
//! [`late_init`] is the DXE-entry step; ReadyToBoot is
//! [`program::ready_to_boot`](super::program::ready_to_boot).

use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

use super::topology::Topology;
use super::v2::{self, CCM_INSTANCES, CS_INSTANCES, IOMS_INSTANCE, PIE_INSTANCE};
use super::{COMPONENT_CNT, DRAM_HOLE, Df, FBICNT, GLOBAL_CTRL, Reg, or_bits, regs};
use crate::soc::Revision;

/// Naples policy; `Default` is the donor (naples-s8026-v202) default.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// Scrub period in hours, 0 = scrubber off (PCD 0x65 = 0x18).
    pub scrub_hours: u8,
    /// F6x58 redirect on populated channels (PCD 0x66 = TRUE).
    pub scrub_redirect: bool,
    /// F0x3F8 \[5:4\] (PCD 0x67 = 0xFF: keep).
    pub ras_a: Option<u8>,
    /// F0x3F8 3-bit fields (PCD 0x68 = 0xFF: keep).
    pub ras_b: Option<u8>,
    /// F5x104 bit 19 (IDS option 0x43 = 0 sets it; CBS default clears).
    pub sync_flood: bool,
    /// PCD 0x10 == 1 ("Global C-state Control" auto/enabled): F5x300 bit 5
    /// stays clear.
    pub global_cstate: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            scrub_hours: 24,
            scrub_redirect: true,
            ras_a: None,
            ras_b: None,
            sync_flood: false,
            global_cstate: true,
        }
    }
}

register_bitfields![u32,
    /// F4xC0 \[CCM, IOMS\] (DF_ZP §3.5–3.6).
    pub F4XC0 [
        FIELD OFFSET(12) NUMBITS(6) []
    ],
    /// F3x104 \[CCM\].
    pub F3X104 [
        A0 OFFSET(5) NUMBITS(1) [],
        BIT28 OFFSET(28) NUMBITS(1) []
    ],
    /// F5x104: sync flood on fatal error (IDS option 0x43, DF_ZP §3.7).
    pub F5X104 [
        SYNC_FLOOD OFFSET(19) NUMBITS(1) []
    ],
    /// F6x48 ZP scrubber base (address\[47:20\]) and enable.
    pub SCRUB_BASE [
        EN OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(4) NUMBITS(28) []
    ],
    /// F6x4C ZP scrub rate and limit.
    pub SCRUB_LIMIT [
        RATE OFFSET(0) NUMBITS(4) [],
        LIMIT OFFSET(4) NUMBITS(28) []
    ],
    /// F6x58 \[UMC\] redirect.
    pub SCRUB_REDIRECT [
        EN OFFSET(0) NUMBITS(1) []
    ],
    /// F5x300 PIE power management (DF_ZP §3.9; names of 5/12/13 unknown).
    pub PIE_PM [
        BIT5 OFFSET(5) NUMBITS(1) [],
        BIT12 OFFSET(12) NUMBITS(1) [],
        BIT13 OFFSET(13) NUMBITS(1) [],
        MULTI_DIE OFFSET(14) NUMBITS(1) [],
        MULTI_SOCKET OFFSET(15) NUMBITS(1) []
    ]
];

const F6XDC: Reg = Reg::new(6, 0xDC);
const F2X68: Reg = Reg::new(2, 0x68);
const F2X6C: Reg = Reg::new(2, 0x6C);
const F4XC4: Reg = Reg::new(4, 0xC4);
const F4XC8: Reg = Reg::new(4, 0xC8);
const F4XC0_REG: Reg<F4XC0::Register> = Reg::new(4, 0xC0);
const F3X104_REG: Reg<F3X104::Register> = Reg::new(3, 0x104);
const F3X108: Reg = Reg::new(3, 0x108);
const F3X4C: Reg = Reg::new(3, 0x4C);
const F3X300: Reg = Reg::new(3, 0x300);
const F5X104_REG: Reg<F5X104::Register> = Reg::new(5, 0x104);
const SCRUB_BASE_REG: Reg<SCRUB_BASE::Register> = Reg::new(6, 0x48);
const SCRUB_LIMIT_REG: Reg<SCRUB_LIMIT::Register> = Reg::new(6, 0x4C);
const SCRUB_REDIRECT_REG: Reg<SCRUB_REDIRECT::Register> = Reg::new(6, 0x58);
const PIE_PM_REG: Reg<PIE_PM::Register> = Reg::new(5, 0x300);
const F5X358: Reg = Reg::new(5, 0x358);
const F5X304: Reg = Reg::new(5, 0x304);
const F5X354: Reg = Reg::new(5, 0x354);

/// Instances that get clock gating unless the die is alone and ungated
/// (DF_ZP §3.3; instance 4 only when F6xDC\[4\] bit 0 is clear).
const CLOCK_GATING_INSTANCES: [u8; 13] = [0, 1, 2, 3, 4, 5, 12, 13, 14, 15, 16, 17, 18];

fn is_a0(rev: Revision) -> bool {
    rev == Revision::ZpA0
}

/// Dies of each socket's nodes: `(socket, die, node, dies_in_socket)`.
fn dies(t: &Topology) -> impl Iterator<Item = (u8, u8, u8, u8)> + '_ {
    t.nodes.iter().map(|n| {
        let in_socket = t.nodes.iter().filter(|m| m.socket == n.socket).count() as u8;
        (n.socket, n.die, v2::node(n.socket, n.die), in_socket)
    })
}

/// PEI fabric init. `umc_present[node]`: bit `i` = UMC `i` of that die is
/// populated (APOB group 1 type 2 bytes 0x38/0x39).
pub fn init<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    topology: &Topology,
    revision: Revision,
    policy: &Policy,
    umc_present: &[u8; 8],
) {
    let a0 = is_a0(revision);
    // SAFETY (whole function): registers and instances of present dies, as
    // AmdFabricZpPei accesses them (IC, SOC_DF JSON df_sites).
    unsafe {
        let total_dies = df.node(0, regs::COMPONENT_CNT).read(COMPONENT_CNT::PIE);
        for (_, _, node, _) in dies(topology) {
            // Clock gating.
            let gate = df.inst(node, IOMS_INSTANCE, F6XDC).get() & 1 != 0;
            if !gate && total_dies == 1 {
                df.node(node, regs::CLOCK_GATING).set(1);
            } else {
                for &i in CLOCK_GATING_INSTANCES.iter().filter(|&&i| i != 4 || !gate) {
                    df.inst(node, i, regs::CLOCK_GATING).set(1);
                }
            }
        }
        for (_, _, node, _) in dies(topology) {
            // CS.
            df.node(node, F2X68).set(0x0006_5019);
            df.node(node, F2X6C).set(0x1004_0611);
            // CCM.
            for &i in CCM_INSTANCES.iter() {
                df.inst(node, i, F4XC4).set(0x0002_0001);
            }
            df.node(node, F4XC8).set(0x0102_0000);
            for &i in CCM_INSTANCES.iter() {
                df.inst(node, i, F4XC0_REG).modify(F4XC0::FIELD::CLEAR);
                df.inst(node, i, F3X104_REG)
                    .modify(F3X104::A0.val(u32::from(a0)) + F3X104::BIT28::SET);
                or_bits(&df.inst(node, i, F3X108), 0x40);
            }
            // IOMS.
            or_bits(&df.inst(node, IOMS_INSTANCE, F3X4C), 0x20);
            or_bits(&df.inst(node, IOMS_INSTANCE, F3X300), 0x100);
            df.inst(node, IOMS_INSTANCE, F4XC0_REG)
                .modify(F4XC0::FIELD.val(if a0 { 0x1E } else { 0 }));
        }
        for (_, _, node, _) in dies(topology) {
            // PIE RAS.
            df.node(node, F5X104_REG)
                .modify(F5X104::SYNC_FLOOD.val(u32::from(policy.sync_flood)));
            let n = df.node(node, regs::FBICNT).read(FBICNT::COUNT) as u8;
            for i in 0..n {
                for reg in [regs::PIE_RAS_STATUS_LO, regs::PIE_RAS_STATUS_HI] {
                    let r = df.inst(node, i, reg);
                    if r.get() != 0 {
                        r.set(0);
                    }
                }
            }
            for i in 0..12 {
                let r = df.inst(node, i, regs::GLOBAL_CTRL);
                let mut v = r.extract();
                if let Some(a) = policy.ras_a.filter(|&a| a < 4) {
                    v.modify(GLOBAL_CTRL::RAS_A.val(u32::from(a)));
                }
                if let Some(b) = policy.ras_b.filter(|&b| b < 8) {
                    let b = u32::from(b);
                    v.modify(
                        GLOBAL_CTRL::RAS_B0.val(b)
                            + GLOBAL_CTRL::RAS_B1.val(b)
                            + GLOBAL_CTRL::RAS_B2.val(b),
                    );
                }
                v.modify(GLOBAL_CTRL::BIT3::CLEAR);
                r.set(v.get());
            }
        }
        for (socket, die, node, _) in dies(topology) {
            scrub(
                df,
                socket,
                die,
                node,
                policy,
                umc_present[usize::from(node)],
            );
        }
    }
}

/// DRAM behind a die's scrubber: F6x48/4C range minus the MMIO hole when
/// DRAM rule 0 or 1 (PIE copy) targets this die with the hole on
/// (DF_ZP §3.8 `ffdc4c2a`).
fn scrub_bytes<P: ConfigRegionAccess + Clone>(df: &Df<P>, socket: u8, die: u8, node: u8) -> u64 {
    // SAFETY: registers of a present die.
    unsafe {
        let limit = df.node(node, SCRUB_LIMIT_REG);
        if limit.get() == 0 {
            return 0;
        }
        let base = df.node(node, SCRUB_BASE_REG).read(SCRUB_BASE::BASE);
        let size = u64::from(
            limit
                .read(SCRUB_LIMIT::LIMIT)
                .wrapping_sub(base)
                .wrapping_add(1),
        ) << 20;
        let hole = df.inst(node, PIE_INSTANCE, regs::DRAM_HOLE);
        (0..2u8)
            .filter(|&i| {
                let b = df
                    .inst(
                        node,
                        PIE_INSTANCE,
                        Reg::<v2::DRAM_BASE::Register>::new(0, regs::DRAM_BASE_OFFSET)
                            .at(i, regs::DRAM_STRIDE),
                    )
                    .extract();
                let l = df
                    .inst(
                        node,
                        PIE_INSTANCE,
                        Reg::<()>::new(0, regs::DRAM_LIMIT_OFFSET).at(i, regs::DRAM_STRIDE),
                    )
                    .get();
                b.is_set(v2::DRAM_BASE::VALID)
                    && (l & 0xE0) == u32::from(socket << 2 | die) << 5
                    && b.is_set(v2::DRAM_BASE::HOLE_EN)
                    && hole.is_set(DRAM_HOLE::VALID)
            })
            .fold(size, |size, _| {
                size.wrapping_sub((1 << 32) - (u64::from(hole.read(DRAM_HOLE::BASE)) << 24))
            })
    }
}

fn scrub<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    socket: u8,
    die: u8,
    node: u8,
    policy: &Policy,
    present: u8,
) {
    let size = scrub_bytes(df, socket, die, node);
    if size == 0 || present & 3 == 0 {
        return;
    }
    // SAFETY: registers of a present die.
    unsafe {
        if policy.scrub_redirect {
            for (k, &i) in CS_INSTANCES.iter().enumerate() {
                df.inst(node, i, SCRUB_REDIRECT_REG)
                    .write(SCRUB_REDIRECT::EN.val(u32::from(present >> k & 1)));
            }
        }
        if policy.scrub_hours != 0 {
            let rate = rate(policy.scrub_hours, size);
            df.node(node, SCRUB_LIMIT_REG)
                .modify(SCRUB_LIMIT::RATE.val(u32::from(rate)));
            df.node(node, SCRUB_BASE_REG).modify(SCRUB_BASE::EN::SET);
        }
    }
}

/// The ZP rate index (same formula and thresholds as DF3, DF_ZP §3.8).
fn rate(hours: u8, bytes: u64) -> u8 {
    const T: [u64; 16] = [
        640, 1280, 2560, 5120, 10200, 20500, 41000, 81900, 163_800, 327_700, 655_400, 1_310_000,
        2_620_000, 5_240_000, 10_490_000, 20_970_000,
    ];
    let lines = bytes >> 6;
    if lines == 0 {
        return 0;
    }
    let q = u64::from(hours) * 3_960_000_000_000 / lines;
    T.iter().rposition(|&t| t <= q).unwrap_or(0) as u8
}

/// DXE-entry PIE power management on every die (DF_ZP §3.9; S3: the caller
/// must replay it on resume). `apob_flag`: APOB group 1 type 0xF byte
/// 0x130 (meaning unknown).
pub fn late_init<P: ConfigRegionAccess + Clone>(
    df: &Df<P>,
    topology: &Topology,
    revision: Revision,
    policy: &Policy,
    apob_flag: bool,
) {
    let multi_socket = topology.sockets > 1;
    // A0/B0/B1: AGESA's `family == ZP && (revision & 7)`.
    let early = matches!(revision, Revision::ZpA0 | Revision::ZpB0 | Revision::ZpB1);
    // SAFETY: F5 registers of present dies (AmdFabricZpDxe 0x12dc).
    unsafe {
        let total_dies = df.node(0, regs::COMPONENT_CNT).read(COMPONENT_CNT::PIE);
        for (_, _, node, in_socket) in dies(topology) {
            let multi_die = in_socket > 1;
            // Package is SP3 here, so ZP-A0 always sets bit 5.
            let bit5 = !policy.global_cstate || apob_flag || is_a0(revision);
            df.node(node, PIE_PM_REG).modify(
                PIE_PM::BIT5.val(u32::from(bit5))
                    + PIE_PM::BIT12.val(u32::from(!(multi_die && early)))
                    + PIE_PM::BIT13::SET
                    + PIE_PM::MULTI_DIE.val(u32::from(multi_die))
                    + PIE_PM::MULTI_SOCKET.val(u32::from(multi_socket)),
            );
            let r = df.node(node, F5X358);
            r.set(r.get() & 0x0F00_0000 | 0x80E0_00E0 | if multi_die { 0xE000 } else { 0 });
            let r = df.node(node, F5X304);
            r.set(
                r.get() & !0x420
                    | 0x3C0
                    | u32::from(!multi_socket || apob_flag) << 10
                    | u32::from(apob_flag) << 5,
            );
            if total_dies > 1 {
                let r = df.node(node, F5X354);
                r.set(
                    r.get() & 0xFFFF_0000
                        | if multi_socket { 0x7100 } else { 0 }
                        | if multi_die { 0x46 } else { 0 },
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::model::DfModel;
    use super::*;
    use crate::soc::Generation;
    use crate::topology::Node;

    fn one_socket(dies: u8) -> Topology {
        Topology {
            generation: Generation::Naples,
            sockets: 1,
            nodes: (0..dies)
                .map(|d| Node {
                    socket: 0,
                    die: d,
                    device: 0x18 + d,
                })
                .collect(),
            root_bridges: heapless::Vec::new(),
        }
    }

    #[test]
    fn init_and_scrub() {
        let m = DfModel::default();
        let df = Df::new(m.clone());
        m.set(0, None, 1, 0x204, 4);
        m.set(1, None, 0, 0x40, 2);
        m.set(1, Some(1), 6, 0x1F4, 7);
        // Die 1: 64 GiB at [0, 66 GiB) with the 2 GiB hole, rule 1.
        m.set(1, None, 6, 0x48, 0);
        m.set(1, None, 6, 0x4C, ((66 << 10) - 1) << 4);
        m.set(1, Some(5), 0, 0x118, 0x3);
        m.set(1, Some(5), 0, 0x11C, 0x20);
        m.set(1, Some(5), 0, 0x104, 0x8000_0001);
        init(
            &df,
            &one_socket(4),
            Revision::ZpB2,
            &Policy::default(),
            &[0, 0b10, 0, 0, 0, 0, 0, 0],
        );
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
    fn dxe_pie_pm() {
        let m = DfModel::default();
        let df = Df::new(m.clone());
        m.set(0, None, 1, 0x204, 4);
        late_init(
            &df,
            &one_socket(4),
            Revision::ZpB1,
            &Policy::default(),
            false,
        );
        // B1 with 4 dies: bit 12 clear, 13 and 14 set.
        assert_eq!(m.get(2, None, 5, 0x300), 0x6000);
        assert_eq!(m.get(2, None, 5, 0x358), 0x80E0_E0E0);
        assert_eq!(m.get(2, None, 5, 0x304), 0x7C0);
        assert_eq!(m.get(2, None, 5, 0x354), 0x46);
    }
}
