// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs (Genoa/Siena) NBIO: PCIe wrapper addressing (not the older 1118xxxx
//! core register bank), the vendor base configuration tables and IOHC
//! bridge visibility. Addresses are those of IOS0; a physical IOS is the
//! `ios << 20` offset, never a fused/filtered root ordinal.
//! NFX AmdNbioPei ffa31e96..ffa322f2, ffa33a07..ffa33bce, ffa376a2..ffa37720;
//! public openSIL NbioBaseInitGenoa.c, NbioSmnTables.c, NbioBaseReg.h.
use super::iohc::IohcBridgeCntl;
use crate::smn::{SmnAccess, SmnReg};
use crate::table;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

pub mod tables;

register_bitfields![u32,
    pub CompletionControl [
        TIMEOUT_TO_SUCCESS OFFSET(29) NUMBITS(1) [],
        IGNORE_TIMEOUT_ERROR OFFSET(31) NUMBITS(1) []
    ],
    /// The PCI identity of the bridge in front of an NBIF instance, which
    /// the NBIF uses for the requests and completions it originates.
    pub NbifUpstream [
        BUS OFFSET(16) NUMBITS(8) [],
        DEVICE OFFSET(24) NUMBITS(5) [],
        FUNCTION OFFSET(29) NUMBITS(3) []
    ],
    /// RCC_DEVn_EPFn_STRAP0.
    pub NbifFunctionStrap0 [
        FUNCTION_ENABLE OFFSET(28) NUMBITS(1) []
    ]
];

/// One vendor table record; the condition is a property mask that applies
/// only when ALL its bits are in the root's live property word.
pub type Record = table::Record<SmnReg, u32, u32>;

/// The property word bits the vendor tables test (AGESA GNB `PROPERTY_*`).
pub mod property {
    pub const IOHC_CLOCK_GATING: u32 = 1 << 0;
    pub const IOHC_NO_CLOCK_GATING: u32 = 1 << 1;
    pub const TPH_COMPLETER: u32 = 1 << 2;
    pub const NBIF_CLOCK_GATING: u32 = 1 << 4;
    pub const NBIF_NO_CLOCK_GATING: u32 = 1 << 5;
    pub const SYSHUB_CLOCK_GATING: u32 = 1 << 6;
    pub const SYSHUB_NO_CLOCK_GATING: u32 = 1 << 7;
    pub const SST_CLOCK_GATING: u32 = 1 << 8;
    pub const SST_NO_CLOCK_GATING: u32 = 1 << 9;
    /// Presence bits 16..31: IOHC, SYSHUB, NBIF0..2, IOMMU, L1 IOAGR,
    /// L1 PCIE0/1, L1 NBIF0, NB, PCIE0..2, SDPMUX, SST.
    pub const ALL_PRESENT: u32 = 0xffff_0000;
    pub const PRESENT_SYSHUB: u32 = 1 << 17;
    pub const PRESENT_NBIF0: u32 = 1 << 18;
    pub const PRESENT_NBIF1: u32 = 1 << 19;
    pub const PRESENT_NBIF2: u32 = 1 << 20;
    pub const PRESENT_SDPMUX: u32 = 1 << 30;
    pub const PRESENT_SST: u32 = 1 << 31;
}

/// Platform choices the tables depend on. The default leaves every clock
/// gating feature off and TPH completion disabled, as openSIL does with
/// `NbioGlobalCgOverride` = 0; gating saves power, not function.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Policy {
    /// IOHC, SST and NBIF MGCG clock gating.
    pub clock_gating: bool,
    /// SYSHUB MGCG clock gating.
    pub syshub_clock_gating: bool,
    pub tph_completer: bool,
}

/// The live property word of physical IOHC `ios` (NFX ffa32021..ffa32179):
/// odd roots have NBIF1 only, even roots NBIF0/2; only IOS0 of a socket
/// has the SDPMUX and only IOS0/1 the SST.
pub const fn properties(ios: u8, policy: Policy) -> u32 {
    use property::*;
    let mut absent = if ios & 1 != 0 {
        PRESENT_NBIF0 | PRESENT_NBIF2
    } else {
        PRESENT_NBIF1
    };
    if ios & 3 != 0 {
        absent |= PRESENT_SDPMUX;
    }
    if ios & 2 != 0 {
        absent |= PRESENT_SST;
    }
    let mut live = ALL_PRESENT & !absent;
    live |= if policy.clock_gating {
        IOHC_CLOCK_GATING | SST_CLOCK_GATING | NBIF_CLOCK_GATING
    } else {
        IOHC_NO_CLOCK_GATING | SST_NO_CLOCK_GATING | NBIF_NO_CLOCK_GATING
    };
    live |= if policy.syshub_clock_gating {
        SYSHUB_CLOCK_GATING
    } else {
        SYSHUB_NO_CLOCK_GATING
    };
    if policy.tph_completer {
        live |= TPH_COMPLETER;
    }
    live
}

/// Apply one vendor table to physical IOHC `ios`.
pub fn apply_table(smn: &impl SmnAccess, table: &[Record], ios: u8, live: u32) {
    let offset = u32::from(ios) << 20;
    table::apply_when(
        table.iter().copied(),
        |&mask| live & mask == mask,
        |reg: SmnReg| smn.reg(reg.offset(offset)),
    );
}

const NBIF_UPSTREAM: [(SmnReg<NbifUpstream::Register>, u32, u32); 3] = [
    // NBIF0 behind dev 7 fn 1 (NbifConfigureCommonOptions ffa32608).
    (SmnReg::new(0x1013_101c), 7, 1),
    // NBIF2 behind dev 7 fn 2, NBIF "5" without a bridge (ffa31ba7..).
    (SmnReg::new(0x1013_121c), 7, 2),
    (SmnReg::new(0x1053_101c), 0, 0),
];
const NBIF0_F2_STRAP0: SmnReg<NbifFunctionStrap0::Register> = SmnReg::new(0x1013_4400);
const NBIF1_F2_STRAP0: SmnReg<NbifFunctionStrap0::Register> = SmnReg::new(0x1023_4400);
/// NBIFMM::INTR_LINE_ENABLE, one bit per endpoint function of device 0.
const NBIF0_INTR_LINE_ENABLE: SmnReg = SmnReg::new(0x1013_a008);
const NBIF1_INTR_LINE_ENABLE: SmnReg = SmnReg::new(0x1023_a008);

/// NFX `NbioBaseConfiguration` table pass for one root, in vendor order:
/// the per-socket table on IOS0, the common tables, then on roots with
/// NBIF0/2 the NBIF table and `NbioBaseInit` (upstream identities, F2
/// disabled), and `NbifConfigureCommonOptions`' NBIF0 upstream identity.
/// Its PCD-selected NBIF0 F1 capability straps, the stepping-dependent
/// IOHC client VC routing and the non-PCI BARs are separate work.
///
/// `bus` is the root's primary bus number. Run after every root's bus
/// identity and southbridge location are published.
pub fn base_configuration(smn: &impl SmnAccess, ios: u8, bus: u8, policy: Policy) {
    let live = properties(ios, policy);
    let offset = u32::from(ios) << 20;
    if ios & 3 == 0 {
        apply_table(smn, tables::ONCE_PER_SOCKET, ios, live);
    }
    apply_table(smn, tables::PPR_INIT, ios, live);
    apply_table(smn, tables::SDP_MUX, ios, live);
    apply_table(smn, tables::EARLY_INIT, ios, live);
    let upstream = |index: usize| {
        let (reg, device, function) = NBIF_UPSTREAM[index];
        smn.reg(reg.offset(offset)).modify(
            NbifUpstream::BUS.val(bus.into())
                + NbifUpstream::DEVICE.val(device)
                + NbifUpstream::FUNCTION.val(function),
        );
    };
    if ios & 1 == 0 {
        apply_table(smn, tables::NBIF, ios, live);
        upstream(1);
        upstream(2);
        for (strap, lines) in [
            (NBIF0_F2_STRAP0, NBIF0_INTR_LINE_ENABLE),
            (NBIF1_F2_STRAP0, NBIF1_INTR_LINE_ENABLE),
        ] {
            smn.reg(strap.offset(offset))
                .modify(NbifFunctionStrap0::FUNCTION_ENABLE::CLEAR);
            let lines = smn.reg(lines.offset(offset));
            lines.set(lines.get() & !(1 << 2));
        }
    }
    upstream(0);
}

/// The IOHC bridge control of logical bridge `lbid` (the index of its
/// devfn in the native remap list) on physical IOS `ios`.
pub const fn bridge_control(ios: u8, lbid: u8) -> Option<SmnReg<IohcBridgeCntl::Register>> {
    if ios >= 4 || lbid >= 24 {
        return None;
    }
    Some(SmnReg::new(
        0x13b3_1004 + ((ios as u32) << 20) + ((lbid as u32) << 10),
    ))
}

/// PCIe logical bridges of a root: two nine-port wrappers, plus the
/// four-port wrapper on even roots.
pub const fn pcie_bridges(ios: u8) -> u8 {
    if ios & 1 == 0 { 22 } else { 18 }
}

/// Hide (`BRIDGE_DIS` + `CFG_DIS`) or expose a PCIe root-port bridge, CRS
/// retry enabled either way (NFX PcieControlPorts/PcieEnablePorts). A hidden
/// bridge no longer decodes configuration requests, so software never
/// reaches a port whose link and core are down. Returns the readback.
pub fn set_bridge_visible(smn: &impl SmnAccess, ios: u8, lbid: u8, visible: bool) -> Option<u32> {
    let reg = smn.reg(bridge_control(ios, lbid)?);
    reg.modify(if visible {
        IohcBridgeCntl::BRIDGE_DIS::CLEAR
            + IohcBridgeCntl::CFG_DIS::CLEAR
            + IohcBridgeCntl::CRS_ENABLE::SET
    } else {
        IohcBridgeCntl::BRIDGE_DIS::SET
            + IohcBridgeCntl::CFG_DIS::SET
            + IohcBridgeCntl::CRS_ENABLE::SET
    });
    Some(reg.get())
}

/// Physical IOS, then wrapper, NOT the fused/filtered root ordinal.
/// Wrappers 0/1 cover PCIe; even IOSes additionally have NBIF wrapper 2,
/// as in the checked Rs lane-window descriptors used by `mpio::rs`.
pub const fn completion_control(
    ios: u8,
    wrapper: u8,
) -> Option<SmnReg<CompletionControl::Register>> {
    if ios >= 4 || wrapper > 2 || (wrapper == 2 && ios & 1 != 0) {
        return None;
    }
    Some(SmnReg::new(
        0x1a38_0080 + ((ios as u32 + 4 * wrapper as u32) << 20),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn properties_follow_the_vendor_root_layout() {
        use property::*;
        let off = Policy::default();
        let ios0 = properties(0, off);
        assert_eq!(ios0 & ALL_PRESENT, ALL_PRESENT & !PRESENT_NBIF1);
        assert_eq!(ios0 & 0xffff, 0x2a2);
        let ios2 = properties(2, off);
        assert_eq!(ios2 & (PRESENT_SDPMUX | PRESENT_SST | PRESENT_NBIF1), 0);
        assert_ne!(ios2 & PRESENT_NBIF0, 0);
        let ios3 = properties(3, off);
        assert_eq!(ios3 & (PRESENT_NBIF0 | PRESENT_NBIF2), 0);
        assert_ne!(ios3 & PRESENT_NBIF1, 0);
        let all = Policy {
            clock_gating: true,
            syshub_clock_gating: true,
            tph_completer: true,
        };
        assert_eq!(properties(0, all) & 0xffff, 0x155);
        let no_syshub = Policy {
            syshub_clock_gating: false,
            ..all
        };
        assert_eq!(properties(0, no_syshub) & 0xffff, 0x195);
    }

    #[test]
    fn base_configuration_offsets_tables_and_nbif_identities_per_root() {
        use crate::smn::testing::SmnSpace;
        let hw = SmnSpace::default();
        // CRS retry and P2P rows of the early table, IOS3.
        hw.set(0x13e1_0118u32, 0x3);
        hw.set(0x13e1_0028u32, 0xa000_0000);
        base_configuration(&hw.smn(), 3, 0x40, Policy::default());
        assert_eq!(hw.get(0x13e1_0118u32), 0x2);
        assert_eq!(hw.get(0x13e1_0028u32), 0xa262_0006);
        // Odd root: only NBIF0's upstream identity, no NBIF table rows.
        assert_eq!(hw.get(0x1043_101cu32), 0x2740_0000);
        assert_eq!(hw.get(0x1043_121cu32), 0);
        // Even root: NBIF2/5 identities and F2 disabled, offset by IOS2.
        hw.set(0x1033_4400u32, 0x1000_14c0);
        hw.set(0x1033_a008u32, 0x0000_0337);
        base_configuration(&hw.smn(), 2, 0x00, Policy::default());
        assert_eq!(hw.get(0x1033_121cu32), 0x4700_0000);
        assert_eq!(hw.get(0x1033_4400u32), 0x0000_14c0);
        assert_eq!(hw.get(0x1033_a008u32) & 4, 0);
        // The per-socket table runs on IOS0 only.
        base_configuration(&hw.smn(), 0, 0xc0, Policy::default());
        assert_eq!(hw.get(0x1013_101cu32), 0x27c0_0000);
        assert_eq!(hw.get(0x1013_4000u32) & 0xffff, 0x14dc);
        assert_eq!(hw.get(0x1033_4000u32) & 0xffff, 0x14dc);
    }

    #[test]
    fn bridge_visibility_uses_logical_bridge_and_physical_root() {
        use crate::smn::testing::SmnSpace;
        let hw = SmnSpace::default();
        assert_eq!(bridge_control(3, 9).unwrap().address(), 0x13e3_3404);
        assert!(bridge_control(4, 0).is_none() && bridge_control(0, 24).is_none());
        hw.set(0x13b3_1004u32 + (5 << 10), 0x8000_0000);
        assert_eq!(
            set_bridge_visible(&hw.smn(), 0, 5, false),
            Some(0x8004_0005)
        );
        assert_eq!(set_bridge_visible(&hw.smn(), 0, 5, true), Some(0x8004_0000));
        assert_eq!(pcie_bridges(2), 22);
        assert_eq!(pcie_bridges(3), 18);
    }
    use crate::smn::testing::SmnSpace;
    use tock_registers::interfaces::{ReadWriteable, Readable};
    #[test]
    fn physical_wrapper_addressing_and_policy_preserve_other_fields() {
        let hw = SmnSpace::default();
        for (ios, wrapper, address) in [
            (0, 0, 0x1a38_0080),
            (2, 1, 0x1a98_0080),
            (0, 2, 0x1ab8_0080),
            (2, 2, 0x1ad8_0080),
        ] {
            let control = completion_control(ios, wrapper).unwrap();
            assert_eq!(control.address(), address);
            hw.set(control, 0x1234_5678);
            hw.smn()
                .reg(control)
                .modify(CompletionControl::IGNORE_TIMEOUT_ERROR::SET);
            assert_eq!(hw.get(control), 0x9234_5678);
            assert!(
                !hw.smn()
                    .reg(control)
                    .is_set(CompletionControl::TIMEOUT_TO_SUCCESS)
            );
        }
        assert!(completion_control(4, 0).is_none());
        assert!(completion_control(0, 3).is_none());
        assert!(completion_control(1, 2).is_none());
        assert!(completion_control(3, 2).is_none());
    }
}
