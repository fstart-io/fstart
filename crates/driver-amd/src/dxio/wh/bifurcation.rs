// SPDX-License-Identifier: MIT OR Apache-2.0
//! The core-level host programming of a WH PCIe core around the
//! reconfiguration (DEVICES.md §2.2): `MpioCfgCoreBeforeBifurcationWh`
//! (Dell `0xf64e90`, MPIO API `+0x18`; GIGABYTE `0x709b642e`; order [I],
//! content [D]) and `MpioCfgCoreAfterBifurcationWh` (`0xf65040`, `+0x1C`
//! [D]).
//!
//! Which path a core takes before the bifurcation is [`core_path`]
//! (`0xf64e90` [I], GIGABYTE `0x709b8338` [I]): a core whose wrapper the
//! early-link scan marked (`wrapper + 0x12` bit 10, set by
//! `MpioCleanUpEarlyInitSP5Wh 0xf61370` for a bonus core with a port in
//! LTSSM `0x10..=0x1B`) takes the register path of
//! `BeforeReconfigEarlyTrainingProgramming` (`0xf7be90`, GIGABYTE
//! `0x709c3ca7`; [`before_bifurcation_early`]) if it also holds the
//! `EarlyBmcLinkTraining` lane (`0xf64d80`), and nothing at all otherwise;
//! every other core gets [`before_bifurcation`]: the SLT path (`0xf7fe20`),
//! its ports' [`super::straps::before_reconfig`] (the platform runs them),
//! then the CXL list (`0xf7ee80`) and `MpioCorePcieStraps` (`0xf7fc70`,
//! GIGABYTE `0x709c5b33`), whose PCIe list (with the IDE block `0xf7f830`)
//! is gated by an allocated PCIe port and is followed, ungated, by the
//! three lists every core gets (`0xf7dab0`, `0xf7dbd0`, `0xf7f2d0`; [I] in
//! both donors), then the bonus core's (`0xf7f0e0`).
//!
//! The early core's per-port callback
//! (`BeforeReconfigEarlyTrainingPerPortProgramming`, Dell `0xf7bd90`,
//! GIGABYTE `0x709c3c47`) writes nothing [I, both]: it checks its
//! arguments and looks up the port's GnbHandle. The early core's ports
//! therefore get no per-port straps, as the vendor's.
//!
//! The straps are request `0x0C` on the core's base index
//! ([`super::straps::STRAP_INDEX`]); what they mean beyond the input that
//! feeds them is not known (DEVICES.md §4 Q5).

use super::straps::{Emit, Full, Ops, Space};
use crate::nbio::wh::pcie::{
    self, Core008, Core024, Core70c, Core204, Core304, Core854, CxlErrorLog, PcieStrapF0, PortId,
};

/// The wrapper flags (`wrapper + 0x12`) the vendor builds from the NBIO
/// input block (`PcieFixupPlatformConfig`, Dell `0xf36e60`; DEVICES.md
/// §2.2). [`Default`] is both donors' build values except where noted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CoreFlags {
    /// Bit 0, `PcdCfgACSEnable` (1/1).
    pub acs: bool,
    /// Bit 1, `PcdCfgPCIeLTREnable` (1/1).
    pub ltr: bool,
    /// Bit 2, `PcdCfgPcieAriSupport` (1/1).
    pub ari: bool,
    /// Bits 3 and 4: always 1 in both donors, unnamed.
    pub bit3: bool,
    pub bit4: bool,
    /// Bit 5, `PcdAmdDpcCapability` (`0xFE` = auto = 1): straps
    /// `0x18`/`0x4D` of every port's [`super::straps::before_reconfig`].
    pub dpc: bool,
    /// Bit 6, `PcdCfgAEREnable` (1/1).
    pub aer: bool,
    /// Bit 7, `PcdCfgAerCtoLogCapable` (Dell 1, GIGABYTE 0).
    pub aer_cto_log: bool,
    /// Bits 9:8, `PcdPcieEcrcEnablement ? PcdCfgEcrcControl : 0` (Dell 0,
    /// GIGABYTE 1); 3 when IDE is on without bypass.
    pub ecrc: u8,
    /// Bit 11, `PcdAmdDlfCapEn` (`0xFE` = 1): straps `1`/`0x49`.
    pub dlf: bool,
    /// Bit 12, `PcdCfgDeskewEmptymode` (1/1).
    pub deskew_empty: bool,
    /// Bits 14:13, `PcdCfgIdeBypassMode` (0 or above 2 taken as 2; Dell 2).
    pub ide_bypass: u8,
}

impl Default for CoreFlags {
    fn default() -> Self {
        Self {
            acs: true,
            ltr: true,
            ari: true,
            bit3: true,
            bit4: true,
            dpc: true,
            aer: true,
            aer_cto_log: true,
            ecrc: 0,
            dlf: true,
            deskew_empty: true,
            ide_bypass: 2,
        }
    }
}

impl CoreFlags {
    /// The flags as the vendor's wrapper holds them.
    pub const fn bits(&self) -> u16 {
        (self.acs as u16)
            | (self.ltr as u16) << 1
            | (self.ari as u16) << 2
            | (self.bit3 as u16) << 3
            | (self.bit4 as u16) << 4
            | (self.dpc as u16) << 5
            | (self.aer as u16) << 6
            | (self.aer_cto_log as u16) << 7
            | ((self.ecrc & 3) as u16) << 8
            | (self.dlf as u16) << 11
            | (self.deskew_empty as u16) << 12
            | ((self.ide_bypass & 3) as u16) << 13
    }

    /// Bit 7 ? bit 6 : 0, the AER completion-timeout log.
    const fn cto_log(&self) -> bool {
        self.aer_cto_log && self.aer
    }

    /// The flags of a core whose port descriptor asks for IDE
    /// (`ConfigureCoreFeatures 0xf77bb0`: descriptor `+0x38` bit 7 puts
    /// `+0x42[7:6]` in bits 14:13; no bypass forces ECRC on).
    pub const fn with_ide(mut self, bypass: u8) -> Self {
        self.ide_bypass = bypass & 3;
        if self.ide_bypass == 0 {
            self.ecrc = 3;
        }
        self
    }
}

/// The MPIO input-block values the core steps read (Dell [P]; GIGABYTE
/// where it differs, DEVICES.md §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorePolicy {
    /// `CfgAEREnable` (1/1): strap `0x5C`.
    pub aer: bool,
    /// `CfgRcvErrEnable` (Dell 0, GIGABYTE 1): `0x1A38_0204[13]`.
    pub rcv_err: bool,
    /// `SurpriseDownFeature` (1/1): strap `0x46`.
    pub surprise_down: bool,
    /// `PcdAmdCxlFlitMode` (Dell 1): straps `0x53`, `0x28`, `0x29`.
    pub cxl_flit_mode: bool,
    /// `PcdDataObjectExchange` (1/1): straps `0x6D`, `0x44`.
    pub doe: bool,
    /// `PcdActiveSltMode` (0/0).
    pub slt: bool,
    /// SEV-SNP, SEV-TIO and the IDE capability (0/0/0 on Dell).
    pub sev_snp: bool,
    pub sev_tio: bool,
    pub ide_cap: bool,
    /// `IdeCapPcrcSup` and `IdeCapTeeLimitStreamSup` (with IDE).
    pub ide_pcrc: bool,
    pub ide_tee_limit_stream: bool,
    /// `CxlCorrectableErrorLogging`, `CxlUnCorrectableErrorLogging` (1/1).
    pub cxl_correctable_log: bool,
    pub cxl_uncorrectable_log: bool,
    /// `PcdMaster7bitSteeringTag` (WH block `+4`, 0/0).
    pub steering_tag_7bit: bool,
}

impl Default for CorePolicy {
    fn default() -> Self {
        Self::DELL
    }
}

impl CorePolicy {
    /// The Dell PR9XJ build's values.
    pub const DELL: Self = Self {
        aer: true,
        rcv_err: false,
        surprise_down: true,
        cxl_flit_mode: true,
        doe: true,
        slt: false,
        sev_snp: false,
        sev_tio: false,
        ide_cap: false,
        ide_pcrc: false,
        ide_tee_limit_stream: false,
        cxl_correctable_log: true,
        cxl_uncorrectable_log: true,
        steering_tag_7bit: false,
    };
}

/// What a core holds, as the vendor's per-port callback finds it
/// (`ctx[0]`, `ctx[1]`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CorePorts {
    /// A port of the core is a CXL port.
    pub cxl: bool,
    /// A port of the core is an allocated PCIe port.
    pub pcie: bool,
}

const BONUS: u8 = 6;

/// What `MpioCfgCoreBeforeBifurcationWh` does with a core.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CorePath {
    /// [`before_ports`], the ports' straps, [`before_bifurcation`].
    Normal,
    /// [`before_bifurcation_early`] only.
    Early,
    /// Nothing: a marked core without the early-BMC lane.
    Skip,
}

/// The path of a core: `marked` is the early-link scan's mark (a bonus
/// core with an ABL-trained port), `early_lane` whether the core holds
/// the `EarlyBmcLinkTraining` socket and lane.
pub const fn core_path(marked: bool, early_lane: bool) -> CorePath {
    match (marked, early_lane) {
        (false, _) => CorePath::Normal,
        (true, true) => CorePath::Early,
        (true, false) => CorePath::Skip,
    }
}

fn emit(core: u8, rc: u8, a0: bool) -> Emit {
    Emit::new(PortId { core, rc, port: 0 }, a0)
}

/// `0xf7fe20`: SLT mode, off the bonus core.
fn slt(e: &mut Emit, f: &CoreFlags) {
    // 0xf7f590
    e.core_strap(0x42, Space::Port, f.aer.into());
    e.core_strap(0x63, Space::Tl, f.aer.into());
    e.core_strap(0x7d, Space::Tl, f.cto_log().into());
    e.field(pcie::STRAP_F0, PcieStrapF0::AER_EN.val(f.aer.into()));
    // 0xf7f250
    e.core_strap(0x5d, Space::Tl, f.acs.into());
    e.core_strap(0x5e, Space::Tl, f.acs.into());
    e.field(pcie::STRAP_F0, PcieStrapF0::ACS_EN.val(f.acs.into()));
}

/// `0xf7f830`: the IDE straps.
fn ide(e: &mut Emit, f: &CoreFlags, p: &CorePolicy) {
    let bypass = f.ide_bypass != 0;
    if (!p.sev_snp && !p.sev_tio && !p.ide_cap) || bypass {
        e.core_strap(0x6b, Space::Tl, 0);
        e.core_strap(0x67, Space::Tl, 1);
        e.core_strap(0x69, Space::Tl, 0);
        e.core_strap(0x6a, Space::Tl, 0);
    } else {
        e.core_strap(0x6b, Space::Tl, 1);
        e.core_strap(0x67, Space::Tl, (!p.ide_pcrc).into());
        e.core_strap(0x68, Space::Tl, p.ide_tee_limit_stream.into());
        e.core_strap(0x69, Space::Tl, 1);
        e.core_strap(0x6a, Space::Tl, 1);
    }
    if (p.sev_tio || p.ide_cap) && !bypass {
        e.field(pcie::CORE_70C, Core70c::IDE.val(2));
    }
    if p.sev_tio && !bypass {
        e.core_strap(0x6c, Space::Tl, 1);
    }
}

/// `0xf7fc70`: a core with a PCIe port.
fn pcie_list(e: &mut Emit, core: u8, f: &CoreFlags, p: &CorePolicy) {
    e.core_strap(0x5c, Space::Tl, p.aer.into());
    e.core_strap(0x74, Space::Port, if core == BONUS { 1 } else { 3 });
    e.field(pcie::CORE_204, Core204::RCV_ERR_EN.val(p.rcv_err.into()));
    e.core_strap(0x5d, Space::Tl, f.acs.into());
    e.core_strap(0x5e, Space::Tl, f.acs.into());
    e.core_strap(0x5f, Space::Tl, 1);
    for (port, tl, on) in [
        (0x3f, 0x60, f.ari),
        (0x40, 0x61, f.bit3),
        (0x41, 0x62, f.bit4),
        (0x42, 0x63, f.aer),
    ] {
        e.core_strap(port, Space::Port, on.into());
        e.core_strap(tl, Space::Tl, on.into());
    }
    e.core_strap(0x7d, Space::Tl, f.cto_log().into());
    e.core_strap(0x64, Space::Tl, u32::from(f.ecrc & 1));
    e.core_strap(0x65, Space::Tl, u32::from(f.ecrc >> 1 & 1));
    e.core_strap(0x43, Space::Port, f.ltr.into());
    e.core_strap(0x66, Space::Tl, f.ltr.into());
    e.core_strap(0x46, Space::Tl, p.surprise_down.into());
    ide(e, f, p);
    if p.doe {
        e.core_strap(0x6d, Space::Tl, 1);
        e.core_strap(0x44, Space::Port, 1);
    }
    e.core_strap(0x6e, Space::Tl, 1);
    e.core_strap(0x6f, Space::Tl, 1);
    e.core_strap(0x1f, Space::Port, f.deskew_empty.into());
}

/// The core's steps of `MpioCfgCoreBeforeBifurcationWh` after its ports'
/// [`super::straps::before_reconfig`], for core `core` (RC aperture `rc`)
/// that is not the early-trained one; `slt` first ([`before_ports`]).
pub fn before_bifurcation(
    core: u8,
    rc: u8,
    holds: CorePorts,
    flags: &CoreFlags,
    policy: &CorePolicy,
    a0: bool,
) -> Result<Ops, Full> {
    let mut e = emit(core, rc, a0);
    let bonus = core == BONUS;
    // 0xf7ee80
    if holds.cxl && !bonus {
        for (id, space) in [
            (0x36, Space::Port),
            (0x57, Space::Tl),
            (0x37, Space::Port),
            (0x58, Space::Tl),
            (0x38, Space::Port),
        ] {
            e.core_strap(id, space, 1);
        }
        e.field(
            pcie::CXL_ERROR_LOG,
            CxlErrorLog::CORRECTABLE.val(policy.cxl_correctable_log.into())
                + CxlErrorLog::UNCORRECTABLE.val(if policy.cxl_uncorrectable_log { 2 } else { 0 }),
        );
    }
    if holds.pcie {
        pcie_list(&mut e, core, flags, policy);
    }
    // 0xf7dab0
    let flit = policy.cxl_flit_mode && !bonus;
    e.core_strap(0x53, Space::Tl, flit.into());
    e.core_strap(0x28, Space::Port, flit.into());
    e.core_strap(0x29, Space::Port, (flit && !a0).into());
    // 0xf7dbd0
    if bonus {
        e.core_strap(0x2a, Space::Port, 0);
    } else {
        e.core_strap(0x2a, Space::Port, 1);
        e.core_strap(0x54, Space::Tl, 1);
    }
    // 0xf7f2d0
    if a0 {
        e.core_strap(0x72, Space::Port, 0x200);
    }
    e.core_strap(0x73, Space::Tl, 0x200);
    // 0xf7f0e0
    if bonus && holds.pcie {
        for (id, space, value) in [
            (0x39, Space::Port, 1),
            (0x59, Space::Tl, 1),
            (0x3a, Space::Port, 1),
            (0x3b, Space::Port, 0),
            (0x5a, Space::Tl, 0),
            (0x3c, Space::Port, 0),
            (0x3d, Space::Port, 0),
            (0x5b, Space::Tl, 0),
            (0x3e, Space::Port, 0),
        ] {
            e.core_strap(id, space, value);
        }
    }
    e.finish()
}

/// The SLT path the vendor runs before the core's ports (`PcdActiveSltMode`
/// on a core other than the bonus one); empty otherwise.
pub fn before_ports(
    core: u8,
    rc: u8,
    flags: &CoreFlags,
    policy: &CorePolicy,
    a0: bool,
) -> Result<Ops, Full> {
    let mut e = emit(core, rc, a0);
    if policy.slt && core != BONUS {
        slt(&mut e, flags);
    }
    e.finish()
}

/// `BeforeReconfigEarlyTrainingProgramming` (`0xf7be90`) for the core of
/// the ABL-trained link, in place of [`before_bifurcation`] and its ports'
/// straps: `PCIE_STRAP_F0` ACS, AER and ECRC, the AER completion-timeout
/// log, the receiver-error enable and the surprise-down strap.
pub fn before_bifurcation_early(
    core: u8,
    rc: u8,
    flags: &CoreFlags,
    policy: &CorePolicy,
    a0: bool,
) -> Result<Ops, Full> {
    let mut e = emit(core, rc, a0);
    e.field(
        pcie::STRAP_F0,
        PcieStrapF0::ACS_EN.val(flags.acs.into()) + PcieStrapF0::AER_EN.val(flags.aer.into()),
    );
    e.field(
        pcie::CORE_304,
        Core304::AER_CTO_LOG.val(flags.cto_log().into()),
    );
    e.field(
        pcie::STRAP_F0,
        PcieStrapF0::ECRC_GEN_EN.val(u32::from(flags.ecrc & 1))
            + PcieStrapF0::ECRC_CHECK_EN.val(u32::from(flags.ecrc >> 1 & 1)),
    );
    e.field(
        pcie::CORE_204,
        Core204::RCV_ERR_EN.val(policy.rcv_err.into()),
    );
    e.core_strap(0x46, Space::Tl, policy.surprise_down.into());
    e.finish()
}

/// `0xf82a40`, the core step of `MpioCfgCoreAfterBifurcationWh` after its
/// ports' [`super::straps::after_reconfig`]: `0x1A38_0024[6:0]` (core 0:
/// 0x19, 0x10 on A0; cores 1-3: 0x19; the bonus core all ones and its
/// ports' `0x1A34_090C` = `0x22 + port`), then the 7-bit steering tag.
pub fn after_bifurcation(core: u8, rc: u8, policy: &CorePolicy, a0: bool) -> Result<Ops, Full> {
    let mut e = emit(core, rc, a0);
    match core {
        0 => e.field(
            pcie::CORE_024,
            Core024::VALUE.val(if a0 { 0x10 } else { 0x19 }),
        ),
        1..=3 => e.field(pcie::CORE_024, Core024::VALUE.val(0x19)),
        BONUS => {
            e.field(pcie::CORE_024, Core024::VALUE.val(0x7f));
            for port in 0..4u8 {
                let reg = pcie::PORT_90C.at(PortId { core, rc, port });
                e.push(super::straps::Op::Proxy {
                    address: reg.address(),
                    clear: u32::MAX,
                    set: 0x22 + u32::from(port),
                });
            }
        }
        _ => {}
    }
    if policy.steering_tag_7bit {
        e.field(pcie::CORE_854, Core854::STEERING_TAG_7BIT::SET);
        e.field(pcie::CORE_008, Core008::STEERING_TAG_7BIT::SET);
    }
    e.finish()
}

#[cfg(test)]
mod tests {
    use super::super::straps::{Op, strap_index};
    use super::*;

    fn straps(ops: &Ops) -> heapless::Vec<(u16, Space, u32), 96> {
        ops.iter()
            .filter_map(|op| match *op {
                Op::Strap {
                    index,
                    value,
                    space,
                    ..
                } => Some((index, space, value)),
                _ => None,
            })
            .collect()
    }

    /// Strap `id` of `space` (TL and port straps share indices).
    fn strap(ops: &Ops, id: u8, space: Space) -> Option<u32> {
        let index = strap_index(false, id, None, space);
        straps(ops)
            .iter()
            .find(|&&(i, s, _)| (i, s) == (index, space))
            .map(|&(_, _, v)| v)
    }

    #[test]
    fn the_early_path_needs_the_scan_s_mark() {
        assert_eq!(core_path(false, false), CorePath::Normal);
        // The early-BMC lane's core without a trained port: the normal
        // path, its ports' straps included.
        assert_eq!(core_path(false, true), CorePath::Normal);
        assert_eq!(core_path(true, true), CorePath::Early);
        assert_eq!(core_path(true, false), CorePath::Skip);
    }

    #[test]
    fn the_common_lists_follow_the_pcie_list() {
        // `MpioCorePcieStraps`: the PCIe list ends with strap 0x1F, then
        // 0x53/0x28/0x29, 0x2A/0x54, 0x73; the bonus list after them.
        let holds = CorePorts {
            cxl: true,
            pcie: true,
        };
        let ops = before_bifurcation(1, 1, holds, &CoreFlags::default(), &CorePolicy::DELL, false)
            .unwrap();
        let ids: heapless::Vec<(u16, Space), 96> =
            straps(&ops).iter().map(|&(i, s, _)| (i, s)).collect();
        let pos = |id: u8, space: Space| {
            let index = strap_index(false, id, None, space);
            ids.iter().position(|&x| x == (index, space)).unwrap()
        };
        assert!(pos(0x38, Space::Port) < pos(0x5c, Space::Tl));
        assert!(pos(0x1f, Space::Port) < pos(0x53, Space::Tl));
        assert_eq!(pos(0x53, Space::Tl) + 1, pos(0x28, Space::Port));
        assert_eq!(pos(0x29, Space::Port) + 1, pos(0x2a, Space::Port));
        assert_eq!(pos(0x54, Space::Tl) + 1, pos(0x73, Space::Tl));
        assert_eq!(pos(0x73, Space::Tl) + 1, ids.len());
    }

    #[test]
    fn default_flags_are_the_donors_bits() {
        // b0..b7 (with Dell's AER CTO log), DLF, deskew, IDE bypass 2.
        assert_eq!(
            CoreFlags::default().bits(),
            0xff | 1 << 11 | 1 << 12 | 2 << 13
        );
        let ide = CoreFlags::default().with_ide(0);
        assert_eq!((ide.ide_bypass, ide.ecrc), (0, 3));
    }

    #[test]
    fn a_pcie_core_gets_the_pcie_list_and_the_common_ones() {
        let holds = CorePorts {
            cxl: false,
            pcie: true,
        };
        let ops = before_bifurcation(2, 3, holds, &CoreFlags::default(), &CorePolicy::DELL, false)
            .unwrap();
        assert_eq!(strap(&ops, 0x5c, Space::Tl), Some(1));
        assert_eq!(strap(&ops, 0x74, Space::Port), Some(3));
        assert_eq!(strap(&ops, 0x7d, Space::Tl), Some(1));
        // IDE off: 0x6B = 0, 0x67 = 1.
        assert_eq!(strap(&ops, 0x6b, Space::Tl), Some(0));
        assert_eq!(strap(&ops, 0x67, Space::Tl), Some(1));
        assert_eq!(strap(&ops, 0x6d, Space::Tl), Some(1));
        assert_eq!(strap(&ops, 0x1f, Space::Port), Some(1));
        assert_eq!(strap(&ops, 0x53, Space::Tl), Some(1));
        assert_eq!(strap(&ops, 0x29, Space::Port), Some(1));
        assert_eq!(strap(&ops, 0x2a, Space::Port), Some(1));
        assert_eq!(strap(&ops, 0x73, Space::Tl), Some(0x200));
        assert_eq!(strap(&ops, 0x39, Space::Port), None);
        // CfgRcvErrEnable 0 on Dell, through the proxy.
        assert!(ops.contains(&Op::Proxy {
            address: 0x1a38_0204 + (2 << 20),
            clear: 1 << 13,
            set: 0
        }));
        // No PCIe port, no CXL: the common lists only.
        let ops = before_bifurcation(
            2,
            3,
            CorePorts::default(),
            &CoreFlags::default(),
            &CorePolicy::DELL,
            false,
        )
        .unwrap();
        assert_eq!(straps(&ops).len(), 6);
    }

    #[test]
    fn bonus_core_and_cxl_core() {
        let holds = CorePorts {
            cxl: true,
            pcie: true,
        };
        let f = CoreFlags::default();
        let bonus = before_bifurcation(6, 5, holds, &f, &CorePolicy::DELL, false).unwrap();
        assert_eq!(strap(&bonus, 0x74, Space::Port), Some(1));
        assert_eq!(strap(&bonus, 0x39, Space::Port), Some(1));
        assert_eq!(strap(&bonus, 0x3e, Space::Port), Some(0));
        assert_eq!(strap(&bonus, 0x36, Space::Port), None, "no CXL on core 6");
        assert_eq!(strap(&bonus, 0x53, Space::Tl), Some(0));
        assert_eq!(strap(&bonus, 0x2a, Space::Port), Some(0));
        let cxl = before_bifurcation(0, 0, holds, &f, &CorePolicy::DELL, false).unwrap();
        assert_eq!(strap(&cxl, 0x38, Space::Port), Some(1));
        assert!(cxl.contains(&Op::Proxy {
            address: 0x1a38_0314,
            clear: 0xffff,
            set: 1 << 8 | 2
        }));
    }

    #[test]
    fn early_core_and_after_bifurcation() {
        let ops = before_bifurcation_early(6, 5, &CoreFlags::default(), &CorePolicy::DELL, false)
            .unwrap();
        let f0 = 0x1a38_02c0 + (6 << 20);
        assert!(ops.contains(&Op::Proxy {
            address: f0,
            clear: 3 << 5,
            set: 3 << 5
        }));
        assert_eq!(strap(&ops, 0x46, Space::Tl), Some(1));
        let after = after_bifurcation(6, 5, &CorePolicy::DELL, false).unwrap();
        assert_eq!(after.len(), 5);
        assert!(after.contains(&Op::Proxy {
            address: 0x1a34_090c + (6 << 20) + (3 << 12),
            clear: u32::MAX,
            set: 0x25
        }));
        let a0 = after_bifurcation(0, 0, &CorePolicy::DELL, true).unwrap();
        assert_eq!(
            a0[..],
            [Op::Proxy {
                address: 0x1a38_0024,
                clear: 0x7f,
                set: 0x10
            }]
        );
        let slt = CorePolicy {
            slt: true,
            ..CorePolicy::DELL
        };
        assert_eq!(
            before_ports(1, 1, &CoreFlags::default(), &slt, false)
                .unwrap()
                .len(),
            7
        );
        assert!(
            before_ports(6, 5, &CoreFlags::default(), &slt, false)
                .unwrap()
                .is_empty()
        );
    }
}
