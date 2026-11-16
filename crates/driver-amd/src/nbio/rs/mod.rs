// SPDX-License-Identifier: MIT OR Apache-2.0
//! The RS (Genoa, Siena) NBIO: one per IOS, its registers at `IOS << 20`
//! in the I/O die's one SMN space, which every IOS's root bus reaches
//! ([`Ios`]). The IOHC, IOAPIC and IOMMU registers are the shared ones
//! ([`super::iohc`], [`super::ioapic`], [`super::iommu`]); what RS programs
//! differently is its vendor base configuration (AmdNbioPei
//! `AmdNbioBaseConfiguration`; Siena NFX `ffa31e96..ffa322f2`, openSIL
//! `NbioBaseInitGenoa.c`), its IOMMU policy, the wrapper addressing of its
//! PCIe cores and its IOHC bridge controls.
//!
//! Order, every IOS before the next step: [`early`] (bus number, SB
//! location), then [`base_init`] (the tables, which route the IOAPIC, and
//! the NBIF identities) and [`iommu_init`]; after the links trained,
//! [`after_training`] and [`set_bridge_visible`].
use super::ioapic::Route;
use super::iohc::{IohcBridgeCntl, NB_BUS_NUM_CNTL, NbBusNumCntl, iohc_bridge_cntl};
use crate::smn::{Smn, SmnAccess, SmnReg, SmnRegister};
use crate::table;
use fstart_pci::ConfigRegionAccess;
use tock_registers::RegisterLongName;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::register_bitfields;

/// The IOS with the FCH.
pub const PRIMARY_IOS: u8 = crate::df::v4::PRIMARY_IOS;
/// The IOS whose IOHC holds the PSP BAR ([`super::iohc::PSP_BAR`] at
/// `13B102E0`, where AmdPlatformRasRsDxe reads PSP_BASE_ADDR_LO): MP0's
/// window must reach this IOS, not the FCH's.
pub const PSP_IOS: u8 = 0;

/// One IOS's NBIO: the shared register definitions (those of IOS0) moved
/// to its `IOS << 20`, through the SMN pair of its root bus.
#[derive(Clone)]
pub struct Ios<P> {
    smn: Smn<P>,
    pci: P,
    /// The physical IOS.
    pub index: u8,
}

impl<P: ConfigRegionAccess + Clone> Ios<P> {
    /// IOS `index` with root bus `bus`.
    ///
    /// # Safety
    /// The bus is routed to the IOS and the caller owns the SMN pair of
    /// that bus while this value is used.
    pub unsafe fn new(pci: P, index: u8, bus: u8) -> Self {
        Self {
            // SAFETY: the constructor's contract.
            smn: unsafe { Smn::on_bus(pci.clone(), bus) },
            pci,
            index,
        }
    }

    /// The configuration space.
    pub fn pci(&self) -> &P {
        &self.pci
    }

    /// The root bus.
    pub fn bus(&self) -> u8 {
        self.smn.bus()
    }

    /// The SMN space unmoved: registers whose addresses name the IOS
    /// themselves, and the SMU and MPIO mailboxes.
    pub fn smn(&self) -> &Smn<P> {
        &self.smn
    }

    /// The IOS with the FCH.
    pub fn fch(&self) -> bool {
        self.index == PRIMARY_IOS
    }
}

impl<P: ConfigRegionAccess + Clone> SmnAccess for Ios<P> {
    type Reg<'a, R: RegisterLongName>
        = SmnRegister<'a, P, R>
    where
        Self: 'a;
    fn reg<R: RegisterLongName>(&self, reg: SmnReg<R>) -> Self::Reg<'_, R> {
        self.smn.reg(reg.offset(u32::from(self.index) << 20))
    }
}

#[rustfmt::skip]
pub mod tables;

register_bitfields![u32,
    /// PCIECORE::PCIE_STRAP_F0 (`0x1A3802C0`), the straps a core's ports
    /// live with.
    pub PcieStrapF0 [
        AER_EN OFFSET(5) NUMBITS(1) []
    ],
    /// PCIe wrapper completion control (`0x1A380080`).
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

/// One vendor table record; it applies when ALL bits of its property mask
/// are in the IOS's property word ([`properties`]).
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

/// The board's choices the tables depend on. The default leaves every
/// clock gating feature off and TPH completion disabled, as openSIL does
/// with `NbioGlobalCgOverride` 0: gating saves power, not function.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Policy {
    /// IOHC, SST and NBIF MGCG clock gating.
    pub clock_gating: bool,
    /// SYSHUB MGCG clock gating.
    pub syshub_clock_gating: bool,
    pub tph_completer: bool,
}

/// The property word of IOS `ios` (NFX `ffa32021..ffa32179`): odd IOS have
/// NBIF1 only, even ones NBIF0/2; only IOS0 of a socket has the SDPMUX and
/// only IOS0/1 the SST.
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

/// Run a vendor table on `nbio` with its property word `live`.
pub fn apply<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>, table: &[Record], live: u32) {
    table::apply_when(
        table.iter().copied(),
        |&mask| live & mask == mask,
        |reg: SmnReg| nbio.reg(reg),
    );
}

/// The NBIF upstream identity registers: NBIF0 behind device 7 function 1
/// (`NbifConfigureCommonOptions`, `ffa32608`), NBIF2 behind 7.2 and NBIF
/// "5" without a bridge (`ffa31ba7`).
const NBIF_UPSTREAM: [(SmnReg<NbifUpstream::Register>, u32, u32); 3] = [
    (SmnReg::new(0x1013_101c), 7, 1),
    (SmnReg::new(0x1013_121c), 7, 2),
    (SmnReg::new(0x1053_101c), 0, 0),
];
const NBIF0_F2_STRAP0: SmnReg<NbifFunctionStrap0::Register> = SmnReg::new(0x1013_4400);
const NBIF1_F2_STRAP0: SmnReg<NbifFunctionStrap0::Register> = SmnReg::new(0x1023_4400);
/// NBIFMM::INTR_LINE_ENABLE, one bit per endpoint function of device 0.
const NBIF0_INTR_LINE_ENABLE: SmnReg = SmnReg::new(0x1013_a008);
const NBIF1_INTR_LINE_ENABLE: SmnReg = SmnReg::new(0x1023_a008);

/// The first loop of the vendor's base configuration (`ffa31e96..ffa31f7b`),
/// on every IOS before any other NBIO step: the IOS's root bus (with
/// LAT_MODE, keeping the other bits) and its SB location
/// ([`super::iohc::set_sb_location`]). An IOHC that keeps the reset
/// location forwards the configuration requests nothing on it claims (an
/// absent device on its root bus) to an FCH link it does not have, and they
/// never complete: on the S8040 the first such read wedged every later
/// configuration access.
pub fn early<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>) {
    nbio.reg(NB_BUS_NUM_CNTL)
        .modify(NbBusNumCntl::BUS.val(nbio.bus().into()) + NbBusNumCntl::LAT_MODE::SET);
    super::iohc::set_sb_location(nbio, nbio.fch());
}

/// The second loop (`ffa3207e..ffa322cb`), in vendor order: the socket
/// table on IOS0, the common tables, then on an IOS with NBIF0/2 the NBIF
/// table and `NbioBaseInit` (upstream identities, F2 hidden), and
/// `NbifConfigureCommonOptions`' NBIF0 upstream identity. The IOAPIC
/// routes are in the tables.
pub fn base_init<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>, policy: Policy) {
    let ios = nbio.index;
    let live = properties(ios, policy);
    if ios & 3 == 0 {
        apply(nbio, tables::ONCE_PER_SOCKET, live);
    }
    apply(nbio, tables::PPR_INIT, live);
    apply(nbio, tables::SDP_MUX, live);
    apply(nbio, tables::EARLY_INIT, live);
    let upstream = |(reg, device, function): (SmnReg<NbifUpstream::Register>, u32, u32)| {
        nbio.reg(reg).modify(
            NbifUpstream::BUS.val(nbio.bus().into())
                + NbifUpstream::DEVICE.val(device)
                + NbifUpstream::FUNCTION.val(function),
        );
    };
    if ios & 1 == 0 {
        apply(nbio, tables::NBIF, live);
        upstream(NBIF_UPSTREAM[1]);
        upstream(NBIF_UPSTREAM[2]);
        for (strap, lines) in [
            (NBIF0_F2_STRAP0, NBIF0_INTR_LINE_ENABLE),
            (NBIF1_F2_STRAP0, NBIF1_INTR_LINE_ENABLE),
        ] {
            nbio.reg(strap)
                .modify(NbifFunctionStrap0::FUNCTION_ENABLE::CLEAR);
            let lines = nbio.reg(lines);
            lines.set(lines.get() & !(1 << 2));
        }
    }
    upstream(NBIF_UPSTREAM[0]);
}

/// The bridges' INTx routes into the IOS's IOAPIC, which the tables
/// program (openSIL `NbioIoapicTbl.h`; the Siena vendor tables program the
/// same): 24 bridges. Here a route's `map` is the IOAPIC pin of the
/// bridge's own interrupt within its group, not a group's first pin as on
/// SP3.
pub const ROUTES: [Route; 24] = {
    const TRIPLES: [(u8, u8, u8); 24] = [
        (0, 0, 0),
        (1, 0, 0),
        (2, 0, 0),
        (3, 0, 0),
        (4, 0, 0),
        (4, 2, 0),
        (3, 2, 0),
        (2, 2, 0),
        (1, 2, 0),
        (0, 2, 1),
        (0, 1, 1),
        (1, 1, 1),
        (2, 1, 1),
        (3, 1, 1),
        (4, 1, 1),
        (4, 3, 1),
        (3, 3, 1),
        (2, 3, 1),
        (1, 3, 2),
        (0, 3, 2),
        (0, 0, 2),
        (1, 0, 2),
        (2, 0, 3),
        (3, 0, 3),
    ];
    let mut routes = [Route {
        group: 0,
        swizzle: 0,
        map: 0,
    }; 24];
    let mut i = 0;
    while i < routes.len() {
        let (group, swizzle, map) = TRIPLES[i];
        routes[i] = Route {
            group,
            swizzle,
            map,
        };
        i += 1;
    }
    routes
};

/// The SMN base of PCIe wrapper `wrapper` of IOS `ios`: wrappers 0 and 1
/// are PCIe, an even IOS also has wrapper 2. Not quadrant relative: it
/// goes through the IOS's own SMN view.
const fn wrapper_base(ios: u8, wrapper: u8) -> Option<u32> {
    if ios >= 4 || wrapper > 2 || (wrapper == 2 && ios & 1 != 0) {
        return None;
    }
    Some(0x1a38_0000 + ((ios as u32 + 4 * wrapper as u32) << 20))
}

/// The completion control of PCIe wrapper `wrapper`.
pub const fn completion_control(
    ios: u8,
    wrapper: u8,
) -> Option<SmnReg<CompletionControl::Register>> {
    match wrapper_base(ios, wrapper) {
        Some(base) => Some(SmnReg::new(base + 0x80)),
        None => None,
    }
}

/// The live straps of PCIe wrapper `wrapper` (as [`completion_control`]).
pub const fn strap_f0(ios: u8, wrapper: u8) -> Option<SmnReg<PcieStrapF0::Register>> {
    match wrapper_base(ios, wrapper) {
        Some(base) => Some(SmnReg::new(base + 0x2c0)),
        None => None,
    }
}

/// The AER capability on the ports of a core MPIO did not configure: the
/// one with the PSP-trained BMC link, whose straps MPIO's writes do not
/// reach (openSIL `MpioCfgWrapperBeforeBifurcation`, MpioCfgPoints.c:1356,
/// for the early BMC link; the S8040's AmdNbioPei `ffcf3247` the same).
pub fn enable_aer<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>, wrapper: u8) {
    if let Some(strap) = strap_f0(nbio.index, wrapper) {
        nbio.smn().reg(strap).modify(PcieStrapF0::AER_EN::SET);
    }
}

/// After link training (NFX wrapper callback `ffa375c0`, PCD B4 TRUE):
/// a completion timeout stays a timeout but is no longer escalated to a
/// fatal error. PCD B5 (timeout to success, FALSE) is left alone. The
/// wrappers' SMN addresses are not quadrant relative: they go through the
/// IOS's own SMN view.
pub fn after_training<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>) {
    let smn = nbio.smn();
    (0..3)
        .filter_map(|wrapper| completion_control(nbio.index, wrapper))
        .for_each(|control| {
            smn.reg(control)
                .modify(CompletionControl::IGNORE_TIMEOUT_ERROR::SET)
        });
}

/// PCIe logical bridges of an IOS: two nine-port wrappers, and the
/// four-port one on even IOS.
pub const fn pcie_bridges(ios: u8) -> u8 {
    if ios & 1 == 0 { 22 } else { 18 }
}

/// Hide (`BRIDGE_DIS` and `CFG_DIS`) or show the PCIe root-port bridge with
/// logical bridge ID `lbid`, CRS retry on either way (NFX
/// `PcieControlPorts`/`PcieEnablePorts`). A hidden bridge does not decode
/// configuration requests, so software never reaches a port whose link
/// and core are down.
pub fn set_bridge_visible<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>, lbid: u8, visible: bool) {
    nbio.reg(iohc_bridge_cntl(lbid.into())).modify(if visible {
        IohcBridgeCntl::BRIDGE_DIS::CLEAR
            + IohcBridgeCntl::CFG_DIS::CLEAR
            + IohcBridgeCntl::CRS_ENABLE::SET
    } else {
        IohcBridgeCntl::BRIDGE_DIS::SET
            + IohcBridgeCntl::CFG_DIS::SET
            + IohcBridgeCntl::CRS_ENABLE::SET
    });
}

register_bitfields![u32,
    /// L2 `IOMMU_INTERRUPT` (D0F2x7C).
    Interrupt [
        PIN OFFSET(0) NUMBITS(3) []
    ],
    /// D0F2x50 IOMMU misc.
    IommuMisc [
        HT_ATS_RESERVED OFFSET(22) NUMBITS(1) []
    ],
    /// L1 control 4 (+0xC8).
    L1Control4 [
        MULTIPLE_ATS OFFSET(0) NUMBITS(1) [],
        TIMEOUT_EXTEND OFFSET(2) NUMBITS(1) []
    ],
    /// L1 misc (+0x1C).
    L1Misc [
        FORCE_INVALID_FILTER OFFSET(6) NUMBITS(1) [],
        DISABLE_4K_WAIT OFFSET(12) NUMBITS(1) [],
        ENABLE_NW OFFSET(17) NUMBITS(1) []
    ],
    /// L1 feature control (+0x9C).
    L1Feature [
        PMR_LOCK OFFSET(1) NUMBITS(1) [],
        EXE_LOCK OFFSET(2) NUMBITS(1) []
    ],
    /// L1 control 2 (+0x38).
    L1Control2 [
        RESPONSE_MODE OFFSET(24) NUMBITS(3) [],
        NONCONSUMED_ERROR OFFSET(27) NUMBITS(1) [],
        CONSUMED_ERROR OFFSET(28) NUMBITS(1) []
    ],
    /// L1 wake (+0xD4).
    L1Wake [
        DMA OFFSET(0) NUMBITS(1) []
    ],
    /// L1 clock control (+0xCC).
    L1Clock [
        DMA OFFSET(4) NUMBITS(1) [],
        CACHE OFFSET(5) NUMBITS(1) [],
        SLAVE OFFSET(6) NUMBITS(1) [],
        PERF OFFSET(8) NUMBITS(1) [],
        MEMORY OFFSET(9) NUMBITS(1) [],
        REG OFFSET(10) NUMBITS(1) [],
        HOSTREQ OFFSET(11) NUMBITS(1) [],
        DMARSP OFFSET(12) NUMBITS(1) [],
        HOSTRSP OFFSET(13) NUMBITS(1) [],
        SION OFFSET(14) NUMBITS(1) [],
        HYSTERESIS OFFSET(15) NUMBITS(8) [],
        L2 OFFSET(31) NUMBITS(1) []
    ],
    /// IOAGR L1 XT support (0x153000DC).
    L1Support [
        XT OFFSET(5) NUMBITS(1) []
    ],
    /// IOAGR credit mode (0x1530010C).
    CreditMode [
        RELOCATE OFFSET(31) NUMBITS(1) []
    ],
    /// IOAGR VC7 credits (0x15300118, 0x15300120).
    Credit7 [
        VC7 OFFSET(24) NUMBITS(8) []
    ],
    /// IOAGR read credits (0x15300124).
    ReadCredit [
        VC4 OFFSET(16) NUMBITS(4) []
    ],
    /// IOAGR write credits (0x15300128).
    WriteCredit [
        VC4 OFFSET(16) NUMBITS(4) [],
        VC6 OFFSET(24) NUMBITS(4) []
    ],
    /// L2 error rule lock (0x13F01200, 0x157000C0).
    ErrorRule [
        LOCK OFFSET(0) NUMBITS(1) []
    ],
    /// Supported page sizes (0x157000D0, 0x13F01250).
    PageSize [
        GUEST OFFSET(0) NUMBITS(7) [],
        HOST OFFSET(8) NUMBITS(7) []
    ],
    /// L2 control 5 (0x13F01130).
    Control5 [
        PARTIAL_GUEST OFFSET(12) NUMBITS(7) []
    ],
    /// L2 misc (0x13F0111C).
    L2Misc [
        ATS_IW OFFSET(0) NUMBITS(1) [],
        MASK_NW OFFSET(1) NUMBITS(1) [],
        ATOMIC_FILTER OFFSET(2) NUMBITS(1) []
    ],
    /// L2 command control (0x13F01158).
    Command [
        FLUSH_WAIT OFFSET(1) NUMBITS(1) [],
        FLUSH_INVALIDATE OFFSET(2) NUMBITS(1) []
    ],
    /// L2 filter control (0x13F0114C, 0x15700088).
    Filter [
        BYPASS OFFSET(0) NUMBITS(1) []
    ],
    /// L2 table walker control (0x13F01150).
    Walker [
        COHERENT OFFSET(6) NUMBITS(1) [],
        PREFETCH OFFSET(8) NUMBITS(1) [],
        RANGE OFFSET(12) NUMBITS(3) [],
        NESTED OFFSET(25) NUMBITS(1) []
    ],
    /// L2A clock control (0x157000CC).
    ClockA [
        REG_DISABLE OFFSET(0) NUMBITS(1) [],
        DYNAMIC_DISABLE OFFSET(1) NUMBITS(1) [],
        CACHE_DISABLE OFFSET(2) NUMBITS(1) []
    ],
    /// The EFR shadow (D0F2x80, L2A 0x15704330) as RS has it.
    EfrShadow0 [
        SNP_AVIC OFFSET(18) NUMBITS(3) [],
        GAM OFFSET(21) NUMBITS(3) []
    ],
    /// D0F2x84, L2A 0x15704334.
    EfrShadow1 [
        INVALIDATE_ALL OFFSET(19) NUMBITS(1) [],
        SNP OFFSET(31) NUMBITS(1) []
    ],
    /// L2B clock control (0x13F01240).
    ClockB [
        REG_DISABLE OFFSET(0) NUMBITS(1) [],
        DYNAMIC_DISABLE OFFSET(1) NUMBITS(1) [],
        MISC_DISABLE OFFSET(2) NUMBITS(1) [],
        CACHE_DISABLE OFFSET(3) NUMBITS(1) []
    ]
];

/// The IOMMU L1s of an RS IOS: PCIE0, PCIE1 and IOAGR.
pub const IOMMU_L1: [u32; 3] = [0x1470_0000, 0x14b0_0000, 0x1530_0000];
const L2_INTERRUPT: SmnReg<Interrupt::Register> = SmnReg::new(0x13f0_007c);
const L2_MISC: SmnReg<IommuMisc::Register> = SmnReg::new(0x13f0_0050);
const L2_MISC_CONTROL: SmnReg<L2Misc::Register> = SmnReg::new(0x13f0_111c);
const L2_PARTIAL: SmnReg<Control5::Register> = SmnReg::new(0x13f0_1130);
const L2_FILTER: SmnReg<Filter::Register> = SmnReg::new(0x13f0_114c);
const L2_WALKER: SmnReg<Walker::Register> = SmnReg::new(0x13f0_1150);
const L2_COMMAND: SmnReg<Command::Register> = SmnReg::new(0x13f0_1158);
const L2_ERROR_RULE: SmnReg<ErrorRule::Register> = SmnReg::new(0x13f0_1200);
const L2_CLOCK: SmnReg<ClockB::Register> = SmnReg::new(0x13f0_1240);
const L2_PAGE_SIZE: SmnReg<PageSize::Register> = SmnReg::new(0x13f0_1250);
const L2A_FILTER: SmnReg<Filter::Register> = SmnReg::new(0x1570_0088);
const L2A_ERROR_RULE: SmnReg<ErrorRule::Register> = SmnReg::new(0x1570_00c0);
const L2A_CLOCK: SmnReg<ClockA::Register> = SmnReg::new(0x1570_00cc);
const L2A_PAGE_SIZE: SmnReg<PageSize::Register> = SmnReg::new(0x1570_00d0);
const IOAGR_SUPPORT: SmnReg<L1Support::Register> = SmnReg::new(0x1530_00dc);
const EFR_SHADOW0: SmnReg<EfrShadow0::Register> = SmnReg::new(super::iommu::EFR_SHADOW0.address());
const EFR_SHADOW1: SmnReg<EfrShadow1::Register> = SmnReg::new(super::iommu::EFR_SHADOW1.address());
const L2A_EFR_SHADOW0: SmnReg<EfrShadow0::Register> =
    SmnReg::new(super::iommu::L2A_EFR_SHADOW0.address());
const L2A_EFR_SHADOW1: SmnReg<EfrShadow1::Register> =
    SmnReg::new(super::iommu::L2A_EFR_SHADOW1.address());
const IOAGR_CREDIT_MODE: SmnReg<CreditMode::Register> = SmnReg::new(0x1530_010c);
const IOAGR_CREDIT7: [SmnReg<Credit7::Register>; 2] =
    [SmnReg::new(0x1530_0118), SmnReg::new(0x1530_0120)];
const IOAGR_READ_CREDIT: SmnReg<ReadCredit::Register> = SmnReg::new(0x1530_0124);
const IOAGR_WRITE_CREDIT: SmnReg<WriteCredit::Register> = SmnReg::new(0x1530_0128);

/// The IOMMU L1 and L2 policy of an IOS, before any device masters (openSIL
/// `NbioIommuTbl.h`, Genoa B0; the Siena vendor firmware leaves the same
/// state except the NW bits, with `dense`). The EFR shadows keep SNP, AVIC
/// and GAM off: fstart enables none of them. Translation stays off until the
/// OS enables it.
pub fn iommu_init<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>, dense: bool, clock_gating: bool) {
    nbio.reg(L2_INTERRUPT).modify(Interrupt::PIN.val(0));
    nbio.reg(L2_MISC).modify(IommuMisc::HT_ATS_RESERVED::CLEAR);
    for l1 in IOMMU_L1 {
        nbio.reg::<L1Control4::Register>(SmnReg::new(l1 + 0xc8))
            .modify(L1Control4::MULTIPLE_ATS::SET + L1Control4::TIMEOUT_EXTEND::SET);
        nbio.reg::<L1Misc::Register>(SmnReg::new(l1 + 0x1c)).modify(
            L1Misc::FORCE_INVALID_FILTER::SET
                + L1Misc::DISABLE_4K_WAIT::SET
                + L1Misc::ENABLE_NW.val((!dense).into()),
        );
        nbio.reg::<L1Feature::Register>(SmnReg::new(l1 + 0x9c))
            .modify(L1Feature::PMR_LOCK::SET + L1Feature::EXE_LOCK::SET);
        nbio.reg::<L1Control2::Register>(SmnReg::new(l1 + 0x38))
            .modify(
                L1Control2::RESPONSE_MODE.val(1)
                    + L1Control2::NONCONSUMED_ERROR::SET
                    + L1Control2::CONSUMED_ERROR::SET,
            );
        nbio.reg::<L1Wake::Register>(SmnReg::new(l1 + 0xd4))
            .modify(L1Wake::DMA::SET);
    }
    // The credits change with the relocation mode set, then it is cleared.
    nbio.reg(IOAGR_CREDIT_MODE)
        .modify(CreditMode::RELOCATE::SET);
    for credit in IOAGR_CREDIT7 {
        nbio.reg(credit).modify(Credit7::VC7.val(1));
    }
    nbio.reg(IOAGR_READ_CREDIT).modify(ReadCredit::VC4.val(1));
    nbio.reg(IOAGR_WRITE_CREDIT)
        .modify(WriteCredit::VC4.val(1) + WriteCredit::VC6.val(1));
    nbio.reg(IOAGR_CREDIT_MODE)
        .modify(CreditMode::RELOCATE::CLEAR);
    nbio.reg(L2_ERROR_RULE).modify(ErrorRule::LOCK::SET);
    nbio.reg(L2A_ERROR_RULE).modify(ErrorRule::LOCK::SET);
    for size in [L2A_PAGE_SIZE, L2_PAGE_SIZE] {
        nbio.reg(size)
            .modify(PageSize::GUEST.val(0x49) + PageSize::HOST.val(0x49));
    }
    nbio.reg(L2_PARTIAL)
        .modify(Control5::PARTIAL_GUEST.val(0x49));
    nbio.reg(L2_MISC_CONTROL).modify(
        L2Misc::ATS_IW::SET + L2Misc::MASK_NW.val((!dense).into()) + L2Misc::ATOMIC_FILTER::SET,
    );
    nbio.reg(L2_COMMAND)
        .modify(Command::FLUSH_WAIT::SET + Command::FLUSH_INVALIDATE::CLEAR);
    nbio.reg(L2_FILTER).modify(Filter::BYPASS::CLEAR);
    nbio.reg(L2A_FILTER).modify(Filter::BYPASS::CLEAR);
    nbio.reg(L2_WALKER).modify(
        Walker::COHERENT::SET + Walker::PREFETCH::SET + Walker::RANGE.val(7) + Walker::NESTED::SET,
    );
    // No SEV-SNP, AVIC or guest APIC modes: the EFR shadows do not
    // advertise them. Siena's L2A shadow does not decode.
    nbio.reg(EFR_SHADOW1)
        .modify(EfrShadow1::INVALIDATE_ALL::CLEAR + EfrShadow1::SNP::CLEAR);
    nbio.reg(EFR_SHADOW0)
        .modify(EfrShadow0::SNP_AVIC::CLEAR + EfrShadow0::GAM::CLEAR);
    if !dense {
        nbio.reg(L2A_EFR_SHADOW0)
            .modify(EfrShadow0::SNP_AVIC::CLEAR + EfrShadow0::GAM::CLEAR);
        nbio.reg(L2A_EFR_SHADOW1).modify(EfrShadow1::SNP::CLEAR);
    }
    let off = u32::from(!clock_gating);
    nbio.reg(L2A_CLOCK).modify(
        ClockA::REG_DISABLE.val(off)
            + ClockA::DYNAMIC_DISABLE.val(off)
            + ClockA::CACHE_DISABLE.val(off),
    );
    nbio.reg(L2_CLOCK).modify(
        ClockB::REG_DISABLE.val(off)
            + ClockB::DYNAMIC_DISABLE.val(off)
            + ClockB::MISC_DISABLE.val(off)
            + ClockB::CACHE_DISABLE.val(off),
    );
    let on = u32::from(clock_gating);
    for l1 in IOMMU_L1 {
        nbio.reg::<L1Clock::Register>(SmnReg::new(l1 + 0xcc))
            .modify(
                L1Clock::DMA.val(on)
                    + L1Clock::CACHE.val(on)
                    + L1Clock::SLAVE.val(on)
                    + L1Clock::PERF.val(on)
                    + L1Clock::MEMORY.val(on)
                    + L1Clock::REG.val(on)
                    + L1Clock::HOSTREQ.val(on)
                    + L1Clock::DMARSP.val(on)
                    + L1Clock::HOSTRSP.val(on)
                    + L1Clock::SION.val(on)
                    + L1Clock::L2.val(on)
                    + L1Clock::HYSTERESIS.val(0x20),
            );
    }
    nbio.reg(IOAGR_SUPPORT).modify(L1Support::XT::SET);
}

/// The IOMMU's function (D0F2) on IOS `nbio`'s root bus. Its BAR and
/// capability are reached there, not through an SMN alias (openSIL Genoa).
fn iommu_function<P: ConfigRegionAccess + Clone, R: tock_registers::RegisterLongName>(
    nbio: &Ios<P>,
    offset: u16,
) -> fstart_pci::ConfigRegister<P, R> {
    // SAFETY: the IOMMU's capability registers; the BSP owns them.
    unsafe {
        fstart_pci::ConfigRegister::new(
            nbio.pci().clone(),
            fstart_pci::PciAddress::new(0, nbio.bus(), 0, 2),
            super::iommu::CAPABILITY + offset,
        )
    }
}

/// Decode IOS `nbio`'s IOMMU registers at `base` (512 KiB aligned, below
/// 4 GiB). `false` if the function has no IOMMU capability.
pub fn map_iommu<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>, base: u32) -> bool {
    use super::iohc::{BarHigh, IommuBar};
    use super::iommu::{CAPABILITY_ID, CapHeader};
    if iommu_function::<_, CapHeader::Register>(nbio, 0).read(CapHeader::ID) != CAPABILITY_ID {
        return false;
    }
    let low = iommu_function::<_, IommuBar::Register>(nbio, 4);
    let address = IommuBar::BASE.val(base >> IommuBar::BASE.shift);
    iommu_function::<_, BarHigh::Register>(nbio, 8).write(BarHigh::ADDRESS.val(0));
    low.write(address);
    low.write(address + IommuBar::ENABLE::SET);
    true
}

/// The IOMMU of IOS `nbio` at `base`, for IVRS.
///
/// # Safety
/// [`map_iommu`] decoded it there; the window is identity mapped uncached.
pub unsafe fn iommu<P: ConfigRegionAccess + Clone>(
    nbio: &Ios<P>,
    base: u32,
) -> Option<super::iommu::Iommu> {
    let read = |offset| iommu_function::<_, ()>(nbio, offset).get();
    // SAFETY: the caller's contract.
    unsafe { super::iommu::capture(base.into(), read(0), read(0x0c), read(0x10)) }
}

/// Decode IOS `nbio`'s IOAPIC at `base` (64 KiB aligned) with ID `id`. The
/// tables routed its bridges ([`base_init`]); its redirections are the
/// caller's to mask.
pub fn map_ioapic<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>, base: u64, id: u8) {
    use super::ioapic::{IOAPIC_ID, IoapicId};
    nbio.reg(IOAPIC_ID).write(IoapicId::ID.val(id.into()));
    super::iohc::IOAPIC_BAR.map(nbio, base as u32);
}

register_bitfields![u32,
    /// The PCIe port's transmit requester ID (`0x1A340084`): the identity
    /// the port's own requests carry.
    pub TxRequester [
        FUNCTION OFFSET(0) NUMBITS(3) [],
        DEVICE OFFSET(3) NUMBITS(5) [],
        BUS OFFSET(8) NUMBITS(8) []
    ]
];

/// Give the root port in `slot` its PCI identity (`bus`, `devfn`) as the
/// requester ID of what it sends (openSIL `PcieCommonEngineConfiguration`;
/// Siena's vendor firmware leaves the same): `0x1A340084 + IOS << 20 +
/// wrapper << 22 + port << 12`, by the wrapper-local port, not the
/// logical bridge.
pub fn set_requester<P: ConfigRegionAccess + Clone>(nbio: &Ios<P>, slot: u8, bus: u8, devfn: u8) {
    let (wrapper, port) = crate::dxio::rs::ports::wrapper_port(slot);
    let reg = SmnReg::<TxRequester::Register>::new(
        0x1a34_0084 + (u32::from(wrapper) << 22) + (u32::from(port) << 12),
    );
    nbio.reg(reg).modify(
        TxRequester::FUNCTION.val((devfn & 7).into())
            + TxRequester::DEVICE.val((devfn >> 3).into())
            + TxRequester::BUS.val(bus.into()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smn::model::Model;

    /// The SMN of a modeled IOS on `bus`.
    struct Hw(Model, u8);
    impl Hw {
        fn get(&self, address: u32) -> u32 {
            let state = self.0.0.borrow();
            state.smn.get(&(self.1, address)).copied().unwrap_or(0)
        }
        fn set(&self, address: u32, value: u32) {
            self.0.0.borrow_mut().smn.insert((self.1, address), value);
        }
    }
    fn nbio(ios: u8, bus: u8) -> (Hw, Ios<Model>) {
        let model = Model::default();
        // SAFETY: a model.
        let nbio = unsafe { Ios::new(model.clone(), ios, bus) };
        (Hw(model, bus), nbio)
    }

    #[test]
    fn properties_follow_the_vendor_layout() {
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
    fn base_init_offsets_tables_and_nbif_identities_per_ios() {
        let (hw, ios3) = nbio(3, 0x40);
        // CRS retry and P2P rows of the early table.
        hw.set(0x13e1_0118u32, 0x3);
        hw.set(0x13e1_0028u32, 0xa000_0000);
        base_init(&ios3, Policy::default());
        assert_eq!(hw.get(0x13e1_0118u32), 0x2);
        assert_eq!(hw.get(0x13e1_0028u32), 0xa262_0006);
        // Odd IOS: only NBIF0's upstream identity, no NBIF rows.
        assert_eq!(hw.get(0x1043_101cu32), 0x2740_0000);
        assert_eq!(hw.get(0x1043_121cu32), 0);
        // Even IOS: NBIF2/5 identities and F2 hidden.
        let (hw, ios2) = nbio(2, 0);
        hw.set(0x1033_4400u32, 0x1000_14c0);
        hw.set(0x1033_a008u32, 0x0000_0337);
        base_init(&ios2, Policy::default());
        assert_eq!(hw.get(0x1033_121cu32), 0x4700_0000);
        assert_eq!(hw.get(0x1033_4400u32), 0x0000_14c0);
        assert_eq!(hw.get(0x1033_a008u32) & 4, 0);
        // The socket table runs on IOS0 only.
        let (hw, ios0) = nbio(0, 0xc0);
        base_init(&ios0, Policy::default());
        assert_eq!(hw.get(0x1013_101cu32), 0x27c0_0000);
        assert_eq!(hw.get(0x1013_4000u32) & 0xffff, 0x14dc);
        assert_eq!(hw.get(0x1033_4000u32) & 0xffff, 0x14dc);
    }

    #[test]
    fn early_keeps_reserved_bits_and_moves_the_sb_location() {
        use crate::nbio::iohc::SB_LOCATION;
        let (hw, ios3) = nbio(3, 0x40);
        hw.set(NB_BUS_NUM_CNTL.address() + (3 << 20), 0xabcd_2000);
        hw.set(SB_LOCATION.address() + (3 << 20), 0x4_0001);
        early(&ios3);
        assert_eq!(hw.get(NB_BUS_NUM_CNTL.address() + (3 << 20)), 0xabcd_2140);
        assert_eq!(hw.get(SB_LOCATION.address() + (3 << 20)), 0);
    }

    #[test]
    fn bridge_controls_and_wrappers_are_per_ios() {
        let (hw, ios3) = nbio(3, 0x40);
        hw.set(0x13e3_1004u32 + (5 << 10), 0x8000_0000);
        set_bridge_visible(&ios3, 5, false);
        assert_eq!(hw.get(0x13e3_1004u32 + (5 << 10)), 0x8004_0005);
        set_bridge_visible(&ios3, 5, true);
        assert_eq!(hw.get(0x13e3_1004u32 + (5 << 10)), 0x8004_0000);
        assert_eq!(pcie_bridges(2), 22);
        assert_eq!(pcie_bridges(3), 18);
        for (ios, wrapper, address) in [
            (0, 0, 0x1a38_0080),
            (2, 1, 0x1a98_0080),
            (0, 2, 0x1ab8_0080),
            (2, 2, 0x1ad8_0080),
        ] {
            assert_eq!(completion_control(ios, wrapper).unwrap().address(), address);
            assert_eq!(strap_f0(ios, wrapper).unwrap().address(), address + 0x240);
        }
        assert!(completion_control(1, 2).is_none() && completion_control(4, 0).is_none());
        assert!(strap_f0(1, 2).is_none());
    }

    #[test]
    fn the_tables_program_the_ioapic_routes() {
        let programmed: std::vec::Vec<u32> = [tables::PPR_INIT, tables::EARLY_INIT, tables::NBIF]
            .into_iter()
            .flatten()
            .filter(|r| (0x1430_0040..0x1430_00a0).contains(&r.reg.address()))
            .map(|r| match r.op {
                table::Op::Update {
                    clear: 0x001f_0037,
                    set,
                } => set,
                _ => panic!("route record at {:#x}", r.reg.address()),
            })
            .collect();
        assert_eq!(programmed, ROUTES.map(|route| route.value()));
    }

    #[test]
    fn iommu_policy_matches_the_siena_vendor_state() {
        // Registers as the S8040's vendor firmware leaves them on IOS3.
        let vendor = [
            (0x1470_001c, 0x5040),
            (0x13f0_111c, 0x60005),
            (0x1570_00cc, 0x50000),
            (0x13f0_1240, 0x50000),
            (0x1470_00cc, 0x8010_7f70),
        ];
        let (hw, ios3) = nbio(3, 0x40);
        for (address, value) in vendor {
            hw.set(address + (3 << 20), value);
        }
        iommu_init(&ios3, true, true);
        for (address, value) in vendor {
            assert_eq!(hw.get(address + (3 << 20)), value, "{address:#x}");
        }
        // Siena's L2A EFR shadow does not decode; Genoa's is cleared.
        assert_eq!(hw.get(0x1570_4330 + (3 << 20)), 0);
        // Genoa keeps NW on, and the credits end with relocation off.
        iommu_init(&ios3, false, true);
        assert_eq!(hw.get(0x1470_001c + (3 << 20)), 0x25040);
        assert_eq!(hw.get(0x1530_010c + (3 << 20)) >> 31, 0);
        for l1 in IOMMU_L1 {
            assert_eq!(hw.get(l1 + 0x9c + (3 << 20)) & 6, 6);
        }
    }
}
