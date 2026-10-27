//! PCIe port setup around DXIO training (PCIE_DXIO §3.8, §3.9, §3.10,
//! §3.13): port mapping and devfn remap, bridge hiding, straps, the
//! state-5 and post-training register writes, hotplug port setup, SATA
//! function enable, xGMI requests and the PERST pins.
//!
//! [`PortSetup`] implements [`Hooks`] for [`super::train`]; call
//! [`PortSetup::finish`] after it.
//!
//! PCD-gated writes take the donor build defaults (§6); each site names
//! the Rome / Milan PCD. Register names follow Oxide `milan/pcie.h`; bits
//! without a public name are named by the report's description.

use fstart_pci::ConfigRegionAccess;
use heapless::Vec;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};
use tock_registers::{RegisterLongName, register_bitfields};

use super::engine::FwEngine;
use super::hotplug::HotplugKind;
use super::lanes::{self, Engine, EngineKind, EqPresets, PcieEngine};
use super::{Dxio, DxioError, Hooks, PcieReset, UnitView, op, prop};
use crate::nbio::{Nbio, NbioReg, nbif};
use crate::smn::{Smn, SmnAddr};
use crate::smu::SmuMessages;
use crate::soc::Generation;

register_bitfields![u32,
    /// IOHC `IOHC_BRIDGE_CNTL` (`0x13B31004 + bridge * 0x400`).
    pub BRIDGE_CNTL [
        BRIDGE_DIS OFFSET(0) NUMBITS(1) [],
        CFG_DIS OFFSET(2) NUMBITS(1) [],
        CRS_ENABLE OFFSET(18) NUMBITS(1) []
    ],
    /// IOHC `NB_PROG_DEVICE_REMAP`.
    pub DEVICE_REMAP [
        DEVFN OFFSET(0) NUMBITS(8) []
    ],
    pub PORT_HW_DBG [
        BIT15 OFFSET(15) NUMBITS(1) []
    ],
    /// `PCIE_PORT_TX_CTL`.
    pub TX_CTL [
        /// Cleared before hotplug/after reconfig (unnamed).
        BIT15 OFFSET(15) NUMBITS(1) [],
        /// Rome PCD 0x154 (1).
        BIT20 OFFSET(20) NUMBITS(1) []
    ],
    pub TX_ID [
        DEVFN OFFSET(0) NUMBITS(8) [],
        BUS OFFSET(8) NUMBITS(8) []
    ],
    pub LC_CTL [
        BIT23 OFFSET(23) NUMBITS(1) []
    ],
    pub LC_TRAIN_CTL [
        BIT13 OFFSET(13) NUMBITS(1) []
    ],
    pub LC_WIDTH_CTL [
        BIT19 OFFSET(19) NUMBITS(1) []
    ],
    pub LC_CTL2 [
        F15_14 OFFSET(14) NUMBITS(2) [],
        F24_23 OFFSET(23) NUMBITS(2) []
    ],
    /// `PCIE_PORT_LC_STATE0`: current LTSSM state.
    pub LC_STATE0 [
        CURRENT OFFSET(0) NUMBITS(6) []
    ],
    pub LC_CTL6 [
        BIT0 OFFSET(0) NUMBITS(1) [],
        BIT2 OFFSET(2) NUMBITS(1) []
    ],
    pub LC_CTL11 [
        /// Rome PCD 0x129 / Milan 0x182 (1).
        BIT13 OFFSET(13) NUMBITS(1) []
    ],
    /// `PCIE_LC_CNTL4`.
    pub LC_CTL4 [
        /// Gen3 EQ search mode (3 = preset).
        EQ_8GT_MODE OFFSET(8) NUMBITS(2) [],
        BIT11 OFFSET(11) NUMBITS(1) [],
        BIT16 OFFSET(16) NUMBITS(1) []
    ],
    /// `PCIE_LC_CNTL8`.
    pub LC_CTL8 [
        /// Gen4 EQ search mode.
        EQ_16GT_MODE OFFSET(0) NUMBITS(2) [],
        BIT8 OFFSET(8) NUMBITS(1) [],
        BIT11 OFFSET(11) NUMBITS(1) []
    ],
    pub STRAP_MISC [
        F2_1 OFFSET(1) NUMBITS(2) []
    ],
    pub CORE_PCIE_CTL [
        BIT17 OFFSET(17) NUMBITS(1) [],
        BIT21 OFFSET(21) NUMBITS(1) []
    ],
    pub CORE_PCIE_CTL2 [
        F14_13 OFFSET(13) NUMBITS(2) []
    ],
    pub CORE_CI_CTL [
        BIT29 OFFSET(29) NUMBITS(1) [],
        BIT31 OFFSET(31) NUMBITS(1) []
    ],
    pub CORE_P_CTL [
        BIT6 OFFSET(6) NUMBITS(1) [],
        BIT14 OFFSET(14) NUMBITS(1) [],
        F15_14 OFFSET(14) NUMBITS(2) []
    ],
    pub CORE_STRAP_F0 [
        F14_13 OFFSET(13) NUMBITS(2) [],
        BIT18 OFFSET(18) NUMBITS(1) [],
        BIT20 OFFSET(20) NUMBITS(1) []
    ],
    pub CORE_SDP_CTL [
        PORT_ID OFFSET(0) NUMBITS(4) []
    ],
    pub RX_MARGIN_CTL_CAP [
        BIT2 OFFSET(2) NUMBITS(1) []
    ],
    pub RX_MARGIN1 [
        F19_7 OFFSET(7) NUMBITS(13) []
    ],
    pub RX_MARGIN2 [
        F23_19 OFFSET(19) NUMBITS(5) []
    ],
    /// Root port config `0x58`: PCIe capabilities (dword 0).
    pub CFG_PCIE_CAP [
        SLOT_IMPLEMENTED OFFSET(24) NUMBITS(1) []
    ],
    /// Root port config `0x6C`: slot capabilities.
    pub CFG_SLOT_CAP [
        PHYSICAL_SLOT OFFSET(19) NUMBITS(13) []
    ],
    /// Root port config `0x80`: device control 2.
    pub CFG_DEV_CTL2 [
        COMPLETION_TIMEOUT OFFSET(0) NUMBITS(4) []
    ],
    /// Bridge config `0x18`: bus numbers.
    pub CFG_BUSES [
        SECONDARY OFFSET(8) NUMBITS(8) [],
        SUBORDINATE OFFSET(16) NUMBITS(8) []
    ],
    /// FCH GPIO control (Linux `pinctrl-amd` bit names).
    pub FCH_GPIO [
        DRV_STRENGTH OFFSET(17) NUMBITS(2) [],
        PULL_UP_ENABLE OFFSET(20) NUMBITS(1) [],
        OUTPUT_VALUE OFFSET(22) NUMBITS(1) [],
        OUTPUT_ENABLE OFFSET(23) NUMBITS(1) []
    ],
    /// The IOMUX dword holding pins 24..27; pin 26 is byte 2.
    pub FCH_IOMUX_24 [
        PIN26 OFFSET(16) NUMBITS(8) []
    ]
];

/// Port register offsets (`PCIE_PORT_*`).
pub mod port {
    pub const HW_DBG: u32 = 0x008;
    pub const TX_CTL: u32 = 0x080;
    pub const TX_ID: u32 = 0x084;
    pub const LC_CTL: u32 = 0x280;
    pub const LC_TRAIN_CTL: u32 = 0x284;
    pub const LC_WIDTH_CTL: u32 = 0x288;
    pub const LC_STATE0: u32 = 0x294;
    pub const LC_CTL2: u32 = 0x2C4;
    pub const LC_CTL6: u32 = 0x2EC;
    pub const LC_CTL4: u32 = 0x2D8;
    pub const STRAP_MISC: u32 = 0x304;
    pub const LC_CTL8: u32 = 0x374;
    pub const LC_CTL11: u32 = 0x390;
}

/// Core register offsets (`PCIE_CORE_*`).
pub mod core_reg {
    pub const PCIE_CTL: u32 = 0x040;
    pub const PCIE_CTL2: u32 = 0x070;
    pub const CI_CTL: u32 = 0x080;
    pub const P_CTL: u32 = 0x100;
    pub const SDP_CTL: u32 = 0x18C;
    pub const STRAP_F0: u32 = 0x2C0;
    pub const RSMU_TIMER: u32 = 0x488;
    pub const RX_MARGIN_CTL_CAP: u32 = 0x4D4;
    pub const RX_MARGIN1: u32 = 0x4D8;
    pub const RX_MARGIN2: u32 = 0x4DC;
}

/// Root-port config offsets reached through the SMN config aperture.
pub mod cfg {
    pub const BUSES: u32 = 0x18;
    pub const PCIE_CAP: u32 = 0x58;
    pub const SLOT_CAP: u32 = 0x6C;
    pub const DEV_CTL2: u32 = 0x80;
    pub const DPC_STATUS: u32 = 0x388;
}

/// RSMU PCIe straps (Oxide `pcie_rsmu.h` numbering).
pub mod strap {
    pub const EQ_DS_RX_PRESET_HINT: u16 = 0x9F;
    pub const EQ_US_RX_PRESET_HINT: u16 = 0xA0;
    pub const EQ_DS_TX_PRESET: u16 = 0xA1;
    pub const EQ_US_TX_PRESET: u16 = 0xA2;
    pub const EQ_16GT_DS_TX_PRESET: u16 = 0xA3;
    pub const EQ_16GT_US_TX_PRESET: u16 = 0xA4;
    /// Per-port straps start here, `PER_PORT` apart.
    pub const PORT_BASE: u16 = 0xD2;
    pub const PER_PORT: u16 = 0x61;
    pub const P_SPC_MODE_8GT: u16 = 0xF9;

    /// A per-port strap of `port`.
    pub const fn port(strap: u16, port: u8) -> u16 {
        strap + PER_PORT * port as u16
    }
}

/// IOHC bridge controls and the devfn remap.
pub const IOHC_BRIDGE_CNTL: SmnAddr = 0x13B3_1004;
pub const SP3_DEVICE_REMAP: SmnAddr = 0x13B1_00B8;
pub const NAPLES_DEVICE_REMAP: SmnAddr = 0x13B1_00C0;
/// Rome/Milan RSMU strap index/data (`+ nbio << 12 + core << 14`, not
/// the quadrant offset).
pub const RSMU_STRAP_INDEX: SmnAddr = 0x0904_67AC;
/// Naples strap index/data per core (`0x4A348`, `0x4A3C8`).
pub const NAPLES_STRAP_INDEX: [SmnAddr; 2] = [0x0004_A348, 0x0004_A3C8];

/// Ports (bridges) per NBIO: 18 on Rome/Milan (WAFL 16/17), 16 on Naples.
pub const fn ports_per_nbio(generation: Generation) -> usize {
    match generation {
        Generation::Naples => 16,
        Generation::Rome | Generation::Milan => 18,
    }
}

/// One PCIe core of an NBIO: the port, core and root-port config
/// apertures.
pub struct PcieCore<'n, P> {
    pub nbio: &'n Nbio<P>,
    pub core: u8,
}

impl<'n, P: ConfigRegionAccess + Clone> PcieCore<'n, P> {
    pub fn new(nbio: &'n Nbio<P>, core: u8) -> Self {
        Self { nbio, core }
    }

    /// `base + core stride`: `core << 22` on Rome/Milan (plus the quadrant
    /// `nbio << 20`), `core << 20` on Naples (one die per SMN).
    fn at(&self, base: SmnAddr) -> SmnAddr {
        match self.nbio.generation {
            Generation::Naples => base + ((self.core as u32) << 20),
            Generation::Rome | Generation::Milan => self.nbio.at(base + ((self.core as u32) << 22)),
        }
    }

    pub fn port_addr(&self, port: u8, offset: u32) -> SmnAddr {
        self.at(0x1114_0000) + ((port as u32) << 12) + offset
    }

    pub fn core_addr(&self, offset: u32) -> SmnAddr {
        self.at(0x1118_0000) + offset
    }

    pub fn cfg_addr(&self, port: u8, offset: u32) -> SmnAddr {
        self.at(0x1110_0000) + ((port as u32) << 12) + offset
    }

    pub fn port_reg<R: RegisterLongName>(&self, port: u8, offset: u32) -> NbioReg<P, R> {
        self.nbio.reg(self.port_addr(port, offset))
    }

    pub fn core_reg<R: RegisterLongName>(&self, offset: u32) -> NbioReg<P, R> {
        self.nbio.reg(self.core_addr(offset))
    }

    pub fn cfg_reg<R: RegisterLongName>(&self, port: u8, offset: u32) -> NbioReg<P, R> {
        self.nbio.reg(self.cfg_addr(port, offset))
    }

    /// Write strap `strap` of this core: RSMU index/data on Rome/Milan
    /// (`ffe00720`), the per-core index pair on Naples; index
    /// `0xFFFE0000 | strap`.
    pub fn write_strap(&self, strap: u16, value: u32) {
        let index = match self.nbio.generation {
            Generation::Naples => NAPLES_STRAP_INDEX[self.core as usize & 1],
            Generation::Rome | Generation::Milan => {
                RSMU_STRAP_INDEX + ((self.nbio.index as u32) << 12) + ((self.core as u32) << 14)
            }
        };
        self.nbio.write32(index, 0xFFFE_0000 | strap as u32);
        self.nbio.write32(index + 4, value);
    }

    /// The port's link has trained: LTSSM in L0 states `[0x10, 0x1C)`
    /// (Rome `DxioCleanUpEarlyInitSP3` predicate).
    pub fn trained(&self, port: u8) -> bool {
        let s = self
            .port_reg::<LC_STATE0::Register>(port, port::LC_STATE0)
            .read(LC_STATE0::CURRENT);
        (0x10..0x1C).contains(&s)
    }
}

/// One PCIe engine mapped to a hardware port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MappedPort {
    /// Board engine index.
    pub engine: usize,
    pub socket: u8,
    /// NBIO quadrant (Rome/Milan) or die (Naples).
    pub nbio: u8,
    pub core: u8,
    /// Hardware port within the core (`mac_port_id`).
    pub port: u8,
    /// Assigned devfn after the remap.
    pub devfn: u8,
    /// Logical bridge: the slot of `devfn` in the default devfn table
    /// (Rome `ffe04e2c` template `+0x33`). IOHC_BRIDGE_CNTL and the SMU
    /// hotplug map are indexed by it, not by the physical port.
    pub bridge: u8,
    pub trained: bool,
}

impl MappedPort {
    /// NB_PROG_DEVICE_REMAP index (physical port): `core * 8 + port`.
    pub const fn remap_index(&self) -> usize {
        self.core as usize * 8 + self.port as usize
    }
}

/// The logical bridge answering at `devfn`: its slot in the default
/// devfn table (`ffe04e2c`).
pub fn logical_bridge(generation: Generation, devfn: u8) -> Option<u8> {
    lanes::default_devfns(generation)
        .iter()
        .position(|&d| d == devfn)
        .map(|i| i as u8)
}

/// Map the returned PCIe engines to ports (`ffe0333d`): each wire engine
/// is its board engine (same order, start lane checked), the hardware port
/// is `mac_port_id`. Engines whose lanes moved or whose port is out of the
/// core's range are dropped.
pub fn map_ports<'u>(
    generation: Generation,
    board: &'u [Engine<'_>],
    unit: &'u UnitView<'_>,
) -> impl Iterator<Item = MappedPort> + 'u {
    unit.engines
        .iter()
        .zip(unit.board)
        .filter(|(e, _)| e.is_pcie())
        .filter_map(move |(e, &b)| {
            let p = lanes::place(generation, board.get(b as usize)?).ok()?;
            let ports = if p.core == 2 { 2 } else { 8 };
            (p.wire.start == e.start_lane && e.mac_port_id < ports).then_some(MappedPort {
                engine: b as usize,
                socket: unit.socket,
                nbio: p.nbio,
                core: p.core,
                port: e.mac_port_id,
                devfn: 0,
                bridge: 0,
                trained: false,
            })
        })
}

/// The remap list (`ffe04fa8`): requested devfns per port (0 = none) keep
/// their value; the defaults not requested anywhere fill the others in
/// order.
pub fn remap_devfns<const N: usize>(requested: &[u8; N], defaults: &[u8]) -> [u8; N] {
    let mut free = defaults.iter().copied().filter(|d| !requested.contains(d));
    requested.map(|r| if r != 0 { r } else { free.next().unwrap_or(0) })
}

/// Hide every bridge of the NBIO (`ffdfd20e`: mask `0xFFFBFFFA`, value
/// `0x40005`).
pub fn hide_bridges<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>) {
    (0..ports_per_nbio(nbio.generation)).for_each(|b| {
        bridge(nbio, b).modify(
            BRIDGE_CNTL::BRIDGE_DIS::SET + BRIDGE_CNTL::CFG_DIS::SET + BRIDGE_CNTL::CRS_ENABLE::SET,
        )
    });
}

/// Show one bridge (`ffdfd299`: value `0x40000`).
pub fn show_bridge<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>, index: usize) {
    bridge(nbio, index).modify(
        BRIDGE_CNTL::BRIDGE_DIS::CLEAR + BRIDGE_CNTL::CFG_DIS::CLEAR + BRIDGE_CNTL::CRS_ENABLE::SET,
    );
}

fn bridge<P: ConfigRegionAccess + Clone>(
    nbio: &Nbio<P>,
    index: usize,
) -> NbioReg<P, BRIDGE_CNTL::Register> {
    nbio.reg(nbio.at(IOHC_BRIDGE_CNTL + 0x400 * index as u32))
}

/// Write NB_PROG_DEVICE_REMAP byte 0 of every port.
pub fn write_remap<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>, list: &[u8]) {
    let base = match nbio.generation {
        Generation::Naples => NAPLES_DEVICE_REMAP,
        Generation::Rome | Generation::Milan => SP3_DEVICE_REMAP,
    };
    list.iter().enumerate().for_each(|(i, &d)| {
        nbio.reg::<DEVICE_REMAP::Register>(nbio.at(base + 4 * i as u32))
            .modify(DEVICE_REMAP::DEVFN.val(d as u32))
    });
}

/// State 0xF, per mapped port (Rome `ffdfd4e4`, Milan `ffd0f76b`;
/// Naples: the pre-training TX_CTL write, sp4r2 `ffde433f`).
pub fn before_reconfig<P: ConfigRegionAccess + Clone>(
    c: &PcieCore<'_, P>,
    port: u8,
    pcie: &PcieEngine,
    width: u8,
) {
    let tx = c.port_reg::<TX_CTL::Register>(port, port::TX_CTL);
    if c.nbio.generation == Generation::Naples {
        tx.modify(TX_CTL::BIT15::CLEAR);
        return;
    }
    // EQ straps from the port template, which defaults to
    // `EqPresets::DONOR` (Rome `ffdfb789`, `ffdfd53c..d5c3`).
    let q = pcie.eq.unwrap_or(EqPresets::DONOR);
    [
        (strap::EQ_DS_RX_PRESET_HINT, q.ds_rx_hint),
        (strap::EQ_US_RX_PRESET_HINT, q.us_rx_hint),
        (strap::EQ_DS_TX_PRESET, q.ds_tx),
        (strap::EQ_US_TX_PRESET, q.us_tx),
        (strap::EQ_16GT_DS_TX_PRESET, q.ds_tx_16gt),
        (strap::EQ_16GT_US_TX_PRESET, q.us_tx_16gt),
    ]
    .iter()
    .for_each(|&(s, v)| c.write_strap(s, v as u32));
    // EQ search modes and the template fields that are zero on board rows
    // without vetting/override bits (Rome `ffdfd803..d9ff`, Milan
    // `ffd0f8af..fb91`). The 8 GT mode is preset after `speed_fixup`.
    c.port_reg::<LC_CTL4::Register>(port, port::LC_CTL4).modify(
        LC_CTL4::BIT16::CLEAR
            + LC_CTL4::BIT11::CLEAR
            + LC_CTL4::EQ_8GT_MODE.val(pcie.extra.eq_search_mode.unwrap_or(0) as u32),
    );
    c.port_reg::<LC_CTL8::Register>(port, port::LC_CTL8)
        .modify(LC_CTL8::BIT11::CLEAR + LC_CTL8::BIT8::CLEAR + LC_CTL8::EQ_16GT_MODE.val(0));
    // Rome PCD 0x13D (1): 8 GT/s SKP mode 2 on x1 and x4 links.
    if width == 1 || width == 4 {
        c.write_strap(strap::port(strap::P_SPC_MODE_8GT, port), 2);
    }
    c.port_reg::<LC_CTL11::Register>(port, port::LC_CTL11)
        .modify(LC_CTL11::BIT13::SET);
    tx.modify(TX_CTL::BIT20::SET);
}

/// State 5, per mapped port: clear DPC status (W1C) and TX_CTL bit 15
/// (Rome `ffdfe33b`); Naples (sp4r2 `ffdeb04b`): LC_CTL6 bit 0 = 0
/// (PCD 0xE0), bit 2 = 1 (0xE1), LC_TRAIN_CTL bit 13 on present ports.
pub fn after_reconfig<P: ConfigRegionAccess + Clone>(c: &PcieCore<'_, P>, port: u8, present: bool) {
    if c.nbio.generation == Generation::Naples {
        c.port_reg::<LC_CTL6::Register>(port, port::LC_CTL6)
            .modify(LC_CTL6::BIT0::CLEAR + LC_CTL6::BIT2::SET);
        if present {
            c.port_reg::<LC_TRAIN_CTL::Register>(port, port::LC_TRAIN_CTL)
                .modify(LC_TRAIN_CTL::BIT13::SET);
        }
        return;
    }
    let dpc = c.cfg_reg::<()>(port, cfg::DPC_STATUS);
    dpc.set(dpc.get());
    c.port_reg::<TX_CTL::Register>(port, port::TX_CTL)
        .modify(TX_CTL::BIT15::CLEAR);
}

/// State 5, per used core (Rome `ffdfe33b`, Milan `ffd10ace`): lane
/// margining (Rome 0x12D / Milan 0x19B = 1), CI_CTL bit 29 (Rome 0x12E =
/// 1, Milan 0x87 = 0) and bit 31 (0x134 / 0x86 = 1), SDP port ID.
pub fn after_reconfig_core<P: ConfigRegionAccess + Clone>(c: &PcieCore<'_, P>) {
    let generation = c.nbio.generation;
    if generation == Generation::Naples {
        return;
    }
    c.core_reg::<RX_MARGIN_CTL_CAP::Register>(core_reg::RX_MARGIN_CTL_CAP)
        .modify(RX_MARGIN_CTL_CAP::BIT2::SET);
    c.core_reg::<RX_MARGIN1::Register>(core_reg::RX_MARGIN1)
        .modify(RX_MARGIN1::F19_7.val(0xC97));
    c.core_reg::<RX_MARGIN2::Register>(core_reg::RX_MARGIN2)
        .modify(RX_MARGIN2::F23_19.val(0xF));
    let ci = c.core_reg::<CORE_CI_CTL::Register>(core_reg::CI_CTL);
    ci.modify(CORE_CI_CTL::BIT31::SET);
    if generation == Generation::Rome {
        ci.modify(CORE_CI_CTL::BIT29::SET);
    }
    // SDP_CTL[3:0] = core + 2; the WAFL core gets the raw pattern
    // `& 0xF7FFFFF4 | 0x14000004` (Rome `ffdfe69c..e715`, Milan too).
    let sdp = c.core_reg::<CORE_SDP_CTL::Register>(core_reg::SDP_CTL);
    if c.core == 2 {
        sdp.set(sdp.get() & 0xF7FF_FFF4 | 0x1400_0004);
    } else {
        sdp.modify(CORE_SDP_CTL::PORT_ID.val(c.core as u32 + 2));
    }
}

/// After training, per trained or hotplug port (Rome `ffdff1e6`, Milan
/// `ffd1177a`, Naples sp4r2 `ffde4a88`).
pub fn post_training<P: ConfigRegionAccess + Clone>(
    c: &PcieCore<'_, P>,
    p: &MappedPort,
    pcie: &PcieEngine,
) {
    let port = p.port;
    let sp3 = c.nbio.generation != Generation::Naples;
    if sp3 {
        c.port_reg::<TX_ID::Register>(port, port::TX_ID)
            .modify(TX_ID::BUS.val(c.nbio.bus() as u32) + TX_ID::DEVFN.val(p.devfn as u32));
        if let Some(slot) = pcie.slot {
            c.cfg_reg::<CFG_PCIE_CAP::Register>(port, cfg::PCIE_CAP)
                .modify(CFG_PCIE_CAP::SLOT_IMPLEMENTED::SET);
            c.cfg_reg::<CFG_SLOT_CAP::Register>(port, cfg::SLOT_CAP)
                .modify(CFG_SLOT_CAP::PHYSICAL_SLOT.val(slot as u32));
        }
    } else {
        c.port_reg::<STRAP_MISC::Register>(port, port::STRAP_MISC)
            .modify(STRAP_MISC::F2_1.val(3));
    }
    c.cfg_reg::<CFG_DEV_CTL2::Register>(port, cfg::DEV_CTL2)
        .modify(CFG_DEV_CTL2::COMPLETION_TIMEOUT.val(6));
    c.port_reg::<LC_CTL::Register>(port, port::LC_CTL)
        .modify(LC_CTL::BIT23::SET);
    c.port_reg::<LC_WIDTH_CTL::Register>(port, port::LC_WIDTH_CTL)
        .modify(LC_WIDTH_CTL::BIT19::SET);
    let lc2 = c.port_reg::<LC_CTL2::Register>(port, port::LC_CTL2);
    lc2.modify(LC_CTL2::F15_14.val(1));
    if sp3 {
        lc2.modify(LC_CTL2::F24_23.val(2));
    }
    c.port_reg::<PORT_HW_DBG::Register>(port, port::HW_DBG)
        .modify(PORT_HW_DBG::BIT15::SET);
}

/// After training, per used core (Milan `ffd11664`; Naples sp4r2
/// `ffde4a88`): STRAP_F0 bits 18/20 (Milan 0x9F/0x9E = 1), PCIE_CTL,
/// PCIE_CTL2, P_CTL bit 6 = !PCD 0x18E (0).
pub fn post_training_core<P: ConfigRegionAccess + Clone>(c: &PcieCore<'_, P>) {
    let f0 = c.core_reg::<CORE_STRAP_F0::Register>(core_reg::STRAP_F0);
    let pctl = c.core_reg::<CORE_P_CTL::Register>(core_reg::P_CTL);
    let ctl = c.core_reg::<CORE_PCIE_CTL::Register>(core_reg::PCIE_CTL);
    if c.nbio.generation == Generation::Naples {
        f0.modify(CORE_STRAP_F0::BIT18::SET + CORE_STRAP_F0::F14_13.val(3));
        ctl.modify(CORE_PCIE_CTL::BIT17::CLEAR);
        pctl.modify(CORE_P_CTL::BIT6::SET + CORE_P_CTL::F15_14.val(1));
        return;
    }
    f0.modify(CORE_STRAP_F0::BIT18::SET + CORE_STRAP_F0::BIT20::SET);
    ctl.modify(CORE_PCIE_CTL::BIT21::SET + CORE_PCIE_CTL::BIT17::CLEAR);
    c.core_reg::<CORE_PCIE_CTL2::Register>(core_reg::PCIE_CTL2)
        .modify(CORE_PCIE_CTL2::F14_13.val(2));
    pctl.modify(CORE_P_CTL::BIT14::SET + CORE_P_CTL::BIT6::SET);
}

/// PEI hotplug port setup (Rome `ffdffa58`), the parts without RAS
/// policy: TX_CTL bit 15 = 0, LC_TRAIN_CTL bit 13. The per-type slot
/// capability writes (ExpressModule `0x2003B`, EntSSD `0x40020`), HP_CTL,
/// DPC masks and the firmware-first IOHC bits are open items.
pub fn hotplug_port<P: ConfigRegionAccess + Clone>(c: &PcieCore<'_, P>, port: u8) {
    c.port_reg::<TX_CTL::Register>(port, port::TX_CTL)
        .modify(TX_CTL::BIT15::CLEAR);
    c.port_reg::<LC_TRAIN_CTL::Register>(port, port::LC_TRAIN_CTL)
        .modify(LC_TRAIN_CTL::BIT13::SET);
}

/// Wait for the SMU hotplug firmware to settle: 100 000 ticks of the core
/// RSMU timer, at most 1000 reads (SmuV11Dxe `0x3FE8` bounds it the same
/// way; Naples does not).
pub fn hotplug_settle<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>) {
    let c = PcieCore::new(nbio, 0);
    let timer = c.core_reg::<()>(core_reg::RSMU_TIMER);
    let start = timer.get();
    let _ = (0..1000).find(|_| timer.get().wrapping_sub(start) >= 100_000);
}

/// Enable the SATA functions of a socket with SATA lanes. Rome/Milan:
/// NBIF1 of the socket's first NBIO, functions 1.0/2.0 and their interrupt
/// lines (`ffe04d75`: `0x1053A008` bits 8/16, `0x10535000/0x10536000`
/// bit 28; Milan assumed the same). Naples: the die's NBIF1 function 0.2
/// strap (`0x10234400` bit 28, sp4r2 `ffde6712`).
pub fn enable_sata<P: ConfigRegionAccess + Clone>(nbio: &Nbio<P>) {
    let strap = |dev, func| {
        nbio.reg::<nbif::FUNC_STRAP0::Register>(nbio.nbif(1) + nbif::func_strap0(dev, func))
            .modify(nbif::FUNC_STRAP0::EXIST::SET)
    };
    match nbio.generation {
        Generation::Naples => strap(0, 2),
        Generation::Rome | Generation::Milan => {
            nbio.modify32(nbio.nbif(1) + nbif::INTR_LINE_EN, 0, 1 << 8 | 1 << 16);
            strap(1, 0);
            strap(2, 0);
        }
    }
}

/// xGMI requests, once per socket (§3.13): Milan 2P first sends 0x40
/// (PCD 0x1A3 = 1), then ClearBer `{0x22, 0, (F1x330[4:0] << 8 | ccds)
/// << 8 | ccds, 1}` (Rome `ffdfefa5`; Milan when PCD32 0xAF ≠ 0x40).
/// `f1x330` is D18F1x330 of socket 0's node 0 (the donor reads that fixed
/// ECAM address for both sockets), `ccd_mask` the socket's present CCDs.
pub fn xgmi_init<M: SmuMessages + ?Sized>(
    generation: Generation,
    dxio: &Dxio<'_, M>,
    f1x330: u32,
    ccd_mask: u8,
    two_socket: bool,
) -> Result<(), DxioError> {
    if generation == Generation::Naples {
        return Ok(());
    }
    if generation == Generation::Milan && two_socket {
        dxio.rpc(op::XGMI_BER_ADAPT, [0; 5])?;
    }
    let ccds = ccd_mask as u32;
    let arg = ((f1x330 & 0x1F) << 8 | ccds) << 8 | ccds;
    dxio.rpc(op::SET_RUNTIME_PROP, [prop::XGMI, 0, arg, 1, 0])
        .map(|_| ())
}

/// FCH ACPI MMIO block (0xFED80000) on SMN.
pub const FCH_SMN: SmnAddr = 0x02D0_1000;
/// GPIO26 PCIE_RST0_L, GPIO27 PCIE_RST3_L, RMT GPIO10/11 PCIE_RST1/2_L
/// (Oxide `milan/iomux.h` names), as FCH offsets.
pub const PERST_PINS: [u32; 4] = [0x1568, 0x156C, 0x1228, 0x122C];
/// IOMUX dword holding pin 26.
pub const IOMUX_24: u32 = 0x0D18;

/// PERST release through the FCH GPIOs over SMN (§3.4): Rome/Milan drive
/// all four PCIE_RST pins high (`0xC40000`: output enable, high, drive
/// strength 2); Naples sets GPIO26 to `0xD20000` (adds the pull-up) and
/// IOMUX 26 to the native PCIE_RST0_L function, on the die's FCH and on
/// `naples_also` (the donor hard-codes bus 0x40). Rome writes socket 0
/// through MMIO; SMN reaches the same block (as the donor does for
/// socket 1). Each unit is released once.
pub struct FchPerst<P> {
    pub generation: Generation,
    /// `((socket, die), SMN of that FCH's root bus)`.
    pub fchs: Vec<((u8, u8), Smn<P>), 8>,
    pub naples_also: Option<Smn<P>>,
    released: u16,
}

impl<P: ConfigRegionAccess + Clone> FchPerst<P> {
    /// # Safety
    /// Every SMN handle reaches a live FCH and the caller owns its GPIO
    /// registers for PERST.
    pub unsafe fn new(
        generation: Generation,
        fchs: Vec<((u8, u8), Smn<P>), 8>,
        naples_also: Option<Smn<P>>,
    ) -> Self {
        Self {
            generation,
            fchs,
            naples_also,
            released: 0,
        }
    }

    fn drive(&self, smn: &Smn<P>) {
        // SAFETY: constructor contract.
        let gpio = |pin: u32| unsafe { smn.register::<FCH_GPIO::Register>(FCH_SMN + pin) };
        match self.generation {
            Generation::Naples => {
                gpio(PERST_PINS[0]).write(
                    FCH_GPIO::OUTPUT_ENABLE::SET
                        + FCH_GPIO::OUTPUT_VALUE::SET
                        + FCH_GPIO::PULL_UP_ENABLE::SET
                        + FCH_GPIO::DRV_STRENGTH.val(1),
                );
                // SAFETY: constructor contract.
                unsafe { smn.register::<FCH_IOMUX_24::Register>(FCH_SMN + IOMUX_24) }
                    .modify(FCH_IOMUX_24::PIN26.val(0));
            }
            Generation::Rome | Generation::Milan => PERST_PINS.iter().for_each(|&pin| {
                gpio(pin).write(
                    FCH_GPIO::OUTPUT_ENABLE::SET
                        + FCH_GPIO::OUTPUT_VALUE::SET
                        + FCH_GPIO::DRV_STRENGTH.val(2),
                )
            }),
        }
    }
}

impl<P: ConfigRegionAccess + Clone> PcieReset for FchPerst<P> {
    fn release(&mut self, socket: u8, die: u8) {
        let bit = 1u16 << (socket * 4 + die);
        if self.released & bit != 0 {
            return;
        }
        self.released |= bit;
        let key = match self.generation {
            Generation::Naples => (socket, die),
            Generation::Rome | Generation::Milan => (socket, 0),
        };
        if let Some((_, smn)) = self.fchs.iter().find(|(k, _)| *k == key) {
            self.drive(smn);
        }
        if self.generation == Generation::Naples
            && let Some(smn) = &self.naples_also
        {
            self.drive(smn);
        }
    }
}

/// Most mapped ports: 2 sockets × 4 NBIOs × 18.
pub const MAX_PORTS: usize = 144;

/// The standard [`Hooks`]: port mapping and remap at MAPPED, reconfig
/// writes at CONFIGURED, PERST through `reset`; [`finish`](Self::finish)
/// does the bridges and post-training setup.
pub struct PortSetup<'a, 'e, P, R> {
    generation: Generation,
    nbios: &'a [Nbio<P>],
    board: &'a [Engine<'e>],
    pub reset: R,
    ports: Vec<MappedPort, MAX_PORTS>,
}

impl<'a, 'e, P: ConfigRegionAccess + Clone, R: PcieReset> PortSetup<'a, 'e, P, R> {
    /// `nbios`: every NBIO of the system (Milan: the unfused ones).
    pub fn new(
        generation: Generation,
        nbios: &'a [Nbio<P>],
        board: &'a [Engine<'e>],
        reset: R,
    ) -> Self {
        Self {
            generation,
            nbios,
            board,
            reset,
            ports: Vec::new(),
        }
    }

    /// The mapped ports (for the hotplug table and PCI enumeration).
    pub fn ports(&self) -> &[MappedPort] {
        &self.ports
    }

    fn nbio(&self, socket: u8, index: u8) -> Option<&'a Nbio<P>> {
        self.nbios
            .iter()
            .find(|n| n.socket == socket && n.index == index)
    }

    fn pcie(&self, p: &MappedPort) -> Option<&PcieEngine> {
        self.board.get(p.engine).and_then(Engine::pcie_config)
    }

    /// NBIOs a unit drives: the die (Naples) or the socket's quadrants.
    fn unit_nbios(&self, socket: u8, die: u8) -> impl Iterator<Item = &'a Nbio<P>> + 'a {
        let naples = self.generation == Generation::Naples;
        self.nbios
            .iter()
            .filter(move |n| n.socket == socket && (!naples || n.index == die))
    }

    fn in_unit(&self, p: &MappedPort, socket: u8, die: u8) -> bool {
        p.socket == socket && (self.generation != Generation::Naples || p.nbio == die)
    }

    /// The used (NBIO, core) pairs of a unit.
    fn cores_of(&self, socket: u8, die: u8) -> Vec<(u8, u8), 16> {
        let mut cores: Vec<(u8, u8), 16> = Vec::new();
        self.ports
            .iter()
            .filter(|p| self.in_unit(p, socket, die))
            .for_each(|p| {
                if !cores.contains(&(p.nbio, p.core)) {
                    let _ = cores.push((p.nbio, p.core));
                }
            });
        cores
    }

    /// Bridges, post-training config and SATA after [`super::train`]
    /// (§2 step 3–4, §3.8–§3.10, §3.13).
    pub fn finish(&mut self) {
        let generation = self.generation;
        // Training status from the LTSSM.
        for i in 0..self.ports.len() {
            let p = self.ports[i];
            if let Some(n) = self.nbio(p.socket, p.nbio) {
                let core = PcieCore::new(n, p.core);
                self.ports[i].trained = core.trained(p.port);
                fstart_log::info!(
                    "dxio: engine {} -> {}.{} core {} port {} devfn {:#x} bridge {} LC_STATE0 {:#x} LC_CNTL4 {:#x} trained {}",
                    p.engine as u32,
                    p.socket,
                    p.nbio,
                    p.core,
                    p.port,
                    p.devfn,
                    p.bridge,
                    core.port_reg::<LC_STATE0::Register>(p.port, port::LC_STATE0)
                        .get(),
                    core.port_reg::<LC_CTL4::Register>(p.port, port::LC_CTL4)
                        .get(),
                    self.ports[i].trained as u8
                );
            }
        }
        self.nbios.iter().for_each(hide_bridges);
        for p in &self.ports {
            let (Some(n), Some(pcie)) = (self.nbio(p.socket, p.nbio), self.pcie(p)) else {
                continue;
            };
            let hotplug = pcie.hotplug.map(|h| h.kind);
            let keep = p.trained || hotplug.is_some_and(HotplugKind::keeps_bridge);
            if !keep {
                continue;
            }
            show_bridge(n, p.bridge as usize);
            let c = PcieCore::new(n, p.core);
            post_training(&c, p, pcie);
            if hotplug.is_some() {
                hotplug_port(&c, p.port);
            }
        }
        let mut done: Vec<(u8, u8, u8), 32> = Vec::new();
        for p in &self.ports {
            let key = (p.socket, p.nbio, p.core);
            if done.contains(&key) {
                continue;
            }
            let _ = done.push(key);
            if let Some(n) = self.nbio(p.socket, p.nbio) {
                post_training_core(&PcieCore::new(n, p.core));
            }
        }
        // SATA: Rome/Milan on the socket's first NBIO, Naples per die.
        let mut sata: Vec<(u8, u8), 8> = Vec::new();
        self.board
            .iter()
            .filter(|e| matches!(e.kind, EngineKind::Sata(_)))
            .filter_map(|e| lanes::place(generation, e).ok().map(|p| (e.socket, p.nbio)))
            .map(|(s, n)| {
                if generation == Generation::Naples {
                    (s, n)
                } else {
                    (s, 0)
                }
            })
            .for_each(|k| {
                if !sata.contains(&k) {
                    let _ = sata.push(k);
                }
            });
        sata.iter().for_each(|&(s, n)| {
            let first = self.nbios.iter().filter(|x| x.socket == s);
            let target = if generation == Generation::Naples {
                first.into_iter().find(|x| x.index == n)
            } else {
                first.min_by_key(|x| x.index)
            };
            if let Some(nbio) = target {
                enable_sata(nbio);
            }
        });
    }

    /// Clear the bus numbers early training left on the bridges of early
    /// trained engines (`& 0xFF0000FF`, state 7 / DxioCleanUpEarlyInit).
    pub fn clear_early_buses(&self) {
        for p in &self.ports {
            let (Some(n), Some(pcie)) = (self.nbio(p.socket, p.nbio), self.pcie(p)) else {
                continue;
            };
            if pcie.early_train {
                PcieCore::new(n, p.core)
                    .cfg_reg::<CFG_BUSES::Register>(p.port, cfg::BUSES)
                    .modify(CFG_BUSES::SECONDARY.val(0) + CFG_BUSES::SUBORDINATE.val(0));
            }
        }
    }
}

impl<P: ConfigRegionAccess + Clone, R: PcieReset> Hooks for PortSetup<'_, '_, P, R> {
    fn mapped(&mut self, unit: &UnitView<'_>) -> Result<(), DxioError> {
        let (socket, die) = (unit.socket, unit.die);
        let generation = self.generation;
        let naples = generation == Generation::Naples;
        let fresh: Vec<MappedPort, MAX_PORTS> = map_ports(generation, self.board, unit).collect();
        self.ports
            .retain(|p| !(p.socket == socket && (!naples || p.nbio == die)));
        for nbio in self.unit_nbios(socket, die) {
            let mut requested = [0u8; 18];
            fresh.iter().filter(|p| p.nbio == nbio.index).for_each(|p| {
                requested[p.remap_index()] = self.pcie(p).and_then(|e| e.devfn).unwrap_or(0);
            });
            let defaults = lanes::default_devfns(generation);
            let n = ports_per_nbio(generation);
            let list = remap_devfns(&requested, defaults);
            write_remap(nbio, &list[..n]);
            for p in fresh.iter().filter(|p| p.nbio == nbio.index) {
                let devfn = list[p.remap_index()];
                let p = MappedPort {
                    devfn,
                    bridge: logical_bridge(generation, devfn).ok_or(DxioError::BadDevfn(devfn))?,
                    ..*p
                };
                if let Some(pcie) = self.pcie(&p) {
                    let width = self.board[p.engine].lanes.count();
                    before_reconfig(&PcieCore::new(nbio, p.core), p.port, pcie, width);
                }
                self.ports.push(p).map_err(|_| DxioError::TooManyEngines)?;
            }
        }
        Ok(())
    }

    fn configured(&mut self, unit: &UnitView<'_>) -> Result<(), DxioError> {
        let (socket, die) = (unit.socket, unit.die);
        for p in self.ports.iter().filter(|p| self.in_unit(p, socket, die)) {
            if let (Some(n), Some(pcie)) = (self.nbio(p.socket, p.nbio), self.pcie(p)) {
                after_reconfig(&PcieCore::new(n, p.core), p.port, pcie.present);
            }
        }
        for (nbio, core) in self.cores_of(socket, die) {
            if let Some(n) = self.nbio(socket, nbio) {
                after_reconfig_core(&PcieCore::new(n, core));
            }
        }
        Ok(())
    }

    fn perst(&mut self, unit: &UnitView<'_>, _reply: &[u32; crate::smu::SMU_ARGS]) {
        self.reset.release(unit.socket, unit.die);
    }

    fn release_all(&mut self) {
        let mut seen = 0u16;
        for n in self.nbios {
            let die = if self.generation == Generation::Naples {
                n.index
            } else {
                0
            };
            let bit = 1 << (n.socket * 4 + die);
            if seen & bit == 0 {
                seen |= bit;
                self.reset.release(n.socket, die);
            }
        }
    }
}

/// The engines of a unit that early-trained (wire caps bit 1), for logs.
pub fn early_trained(engines: &[FwEngine]) -> impl Iterator<Item = &FwEngine> {
    engines
        .iter()
        .filter(|e| e.caps().is_set(super::engine::CAPS::EARLY_TRAIN))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dxio::engine::{FwEngine, kind};
    use crate::dxio::lanes::{Group, Lanes, SataEngine, Speed};
    use crate::dxio::tables::SP3_DEFAULT_DEVFN;
    use crate::nbio::tests::nbio;
    use crate::smn::model::Model;

    #[test]
    fn remap_fill() {
        // Ports 0 and 9 request 3.1 and 1.1; the rest take the remaining
        // defaults in order (`ffe04fa8`).
        let mut req = [0u8; 18];
        req[0] = 0x19;
        req[9] = 0x09;
        let l = remap_devfns(&req, &SP3_DEFAULT_DEVFN);
        assert_eq!(l[0], 0x19);
        assert_eq!(l[1], 0x0A, "1.1 is taken: 1.2 is next");
        assert_eq!(l[9], 0x09);
        assert_eq!(l[8], 0x1A, "1.2..2.1 went to ports 1..7, 3.1 is taken: 3.2");
        let mut all = l.to_vec();
        all.sort();
        let mut d = SP3_DEFAULT_DEVFN.to_vec();
        d.sort();
        assert_eq!(all, d, "a permutation of the defaults");
    }

    fn view<'u>(engines: &'u [FwEngine], board: &'u [u8]) -> UnitView<'u> {
        UnitView {
            socket: 0,
            die: 0,
            engines,
            board,
        }
    }

    struct NoReset(u32);
    impl PcieReset for NoReset {
        fn release(&mut self, _: u8, _: u8) {
            self.0 += 1;
        }
    }

    #[test]
    fn mapped_writes_remap_and_straps() {
        let (m, n) = nbio(Generation::Rome, 0, 0);
        let board = [
            Engine::pcie(
                0,
                Lanes::sp3(Group::P0, 8, 4),
                PcieEngine::new().devfn(3, 1).max_speed(Speed::Gen3),
            ),
            Engine::pcie(
                0,
                Lanes::sp3(Group::G0, 0, 16).reversed(),
                PcieEngine::new().slot(3),
            ),
            Engine::sata(0, Lanes::sp3(Group::P0, 0, 6), SataEngine::new(0)),
        ];
        let fw = |start, port| FwEngine {
            kind: kind::PCIE,
            start_lane: start,
            mac_port_id: port,
            ..Default::default()
        };
        let engines = [
            fw(0x32, 2),
            fw(0x10, 0),
            FwEngine {
                kind: kind::SATA,
                start_lane: 0x2A,
                ..Default::default()
            },
        ];
        let nbios = [n];
        let mut s = PortSetup::new(Generation::Rome, &nbios, &board, NoReset(0));
        s.mapped(&view(&engines, &[0, 1, 2])).unwrap();
        let ports = s.ports().to_vec();
        assert_eq!(ports.len(), 2);
        // P0 = NBIO0 core 1: remap index 8 + 2.
        assert_eq!(
            (ports[0].core, ports[0].remap_index(), ports[0].devfn),
            (1, 10, 0x19)
        );
        assert_eq!(
            (ports[1].core, ports[1].remap_index(), ports[1].devfn),
            (0, 0, 0x09)
        );
        // 3.1 is default slot 8: the logical bridge, not physical 10.
        assert_eq!((ports[0].bridge, ports[1].bridge), (8, 0));
        {
            let st = m.0.borrow();
            assert_eq!(st.smn[&(0, 0x13B1_00B8 + 4 * 10)], 0x19);
            assert_eq!(st.smn[&(0, 0x13B1_00B8)], 0x09);
            // The x4 port's SPC strap through RSMU (core 1: + 1 << 14).
            let rsmu = RSMU_STRAP_INDEX + (1 << 14);
            assert!(
                st.log
                    .windows(2)
                    .any(|w| w[0] == (0, rsmu, 0xFFFE_0000 | (0xF9 + 0x61 * 2))
                        && w[1] == (0, rsmu + 4, 2))
            );
            // LC_CTL11 bit 13 on NBIO0 core 1 port 2.
            assert_eq!(st.smn[&(0, 0x1154_2390)], 1 << 13);
            // Default EQ straps without board presets: 0xA4 = 5 last.
            assert!(
                st.log
                    .windows(2)
                    .any(|w| w[0] == (0, rsmu, 0xFFFE_00A4) && w[1] == (0, rsmu + 4, 5))
            );
        }
        // Port 0 (core 1 port 2) in L0, port 1 not: only logical bridge 8
        // (devfn 3.1) is shown; SATA enables NBIF1 functions.
        m.0.borrow_mut().smn.insert((0, 0x1154_2294), 0x10);
        s.finish();
        let st = m.0.borrow();
        assert_eq!(st.smn[&(0, 0x13B3_1004)], 0x40005);
        assert_eq!(st.smn[&(0, 0x13B3_1004 + 0x400 * 8)], 0x40000);
        assert_eq!(st.smn[&(0, 0x13B3_1004 + 0x400 * 10)], 0x40005);
        assert_eq!(st.smn[&(0, 0x1053_5000)], 1 << 28);
        assert_eq!(st.smn[&(0, 0x1053_6000)], 1 << 28);
        assert_eq!(st.smn[&(0, 0x1053_A008)], 1 << 8 | 1 << 16);
    }

    #[test]
    fn trained_port_post_training() {
        let (m, n) = nbio(Generation::Milan, 1, 0x40);
        // P1 = NBIO1 core 0; port 0 LC_STATE0 = 0x10 (L0).
        let lc = 0x1114_0000 + (1 << 20) + 0x294;
        m.0.borrow_mut().smn.insert((0x40, lc), 0x10);
        let board = [Engine::pcie(
            0,
            Lanes::sp3(Group::P1, 0, 16),
            PcieEngine::new().slot(5),
        )];
        let engines = [FwEngine {
            kind: kind::PCIE,
            start_lane: 0x3A,
            mac_port_id: 0,
            ..Default::default()
        }];
        let nbios = [n];
        let mut s = PortSetup::new(Generation::Milan, &nbios, &board, NoReset(0));
        s.mapped(&view(&engines, &[0])).unwrap();
        s.configured(&view(&engines, &[0])).unwrap();
        s.finish();
        assert!(s.ports()[0].trained);
        let st = m.0.borrow();
        let q = 1 << 20;
        assert_eq!(st.smn[&(0x40, 0x13B3_1004 + q)], 0x40000, "bridge 0 shown");
        assert_eq!(st.smn[&(0x40, 0x13B3_1404 + q)], 0x40005, "bridge 1 hidden");
        assert_eq!(
            st.smn[&(0x40, 0x1114_0084 + q)],
            0x40 << 8 | 0x09,
            "TX_ID bus/devfn"
        );
        assert_eq!(st.smn[&(0x40, 0x1110_006C + q)], 5 << 19, "slot number");
        assert_eq!(
            st.smn[&(0x40, 0x1118_018C + q)],
            2,
            "Milan SDP port id = core + 2"
        );
        assert_eq!(
            st.smn[&(0x40, 0x1118_0080 + q)],
            1 << 31,
            "Milan CI_CTL: bit 31 only"
        );
    }

    #[test]
    fn naples_apertures_and_straps() {
        let (m, n) = nbio(Generation::Naples, 2, 0x20);
        let c = PcieCore::new(&n, 1);
        assert_eq!(c.port_addr(3, port::TX_CTL), 0x1124_3080);
        assert_eq!(c.core_addr(0), 0x1128_0000);
        c.write_strap(0x68, 1);
        assert_eq!(m.0.borrow().smn[&(0x20, 0x4A3C8)], 0xFFFE_0068);
        let (_, r) = nbio(Generation::Rome, 3, 0);
        let c = PcieCore::new(&r, 2);
        assert_eq!(
            c.port_addr(1, port::LC_STATE0),
            0x11C4_1294,
            "WAFL core of NBIO3"
        );
    }

    #[test]
    fn xgmi_and_perst() {
        use crate::dxio::tests::FakeSmu;
        let smu = FakeSmu::default();
        xgmi_init(Generation::Milan, &Dxio::new(&smu, 8), 0x23, 0b1111, true).unwrap();
        assert_eq!(smu.args()[0][0], op::XGMI_BER_ADAPT);
        assert_eq!(
            smu.args()[1],
            [
                op::SET_RUNTIME_PROP,
                0x22,
                0,
                (0x03 << 8 | 0xF) << 8 | 0xF,
                1,
                0
            ]
        );

        let model = Model::default();
        let mut fchs = Vec::new();
        let _ = fchs.push(((1, 0), Smn::new(model.clone(), 0x80)));
        // SAFETY: model.
        let mut p = unsafe { FchPerst::new(Generation::Rome, fchs, None) };
        p.release(1, 0);
        p.release(1, 0);
        let st = model.0.borrow();
        assert_eq!(st.log.len(), 4, "four pins, once");
        assert_eq!(st.smn[&(0x80, 0x02D0_2568)], 0xC4_0000);
        assert_eq!(st.smn[&(0x80, 0x02D0_222C)], 0xC4_0000);
        drop(st);

        let model = Model::default();
        model
            .0
            .borrow_mut()
            .smn
            .insert((0, 0x02D0_1D18), 0x0102_0304);
        let mut fchs = Vec::new();
        let _ = fchs.push(((0, 1), Smn::new(model.clone(), 0)));
        // SAFETY: model.
        let mut z = unsafe {
            FchPerst::new(
                Generation::Naples,
                fchs,
                Some(Smn::new(model.clone(), 0x40)),
            )
        };
        z.release(0, 1);
        let st = model.0.borrow();
        assert_eq!(st.smn[&(0, 0x02D0_2568)], 0xD2_0000);
        assert_eq!(
            st.smn[&(0, 0x02D0_1D18)],
            0x0100_0304,
            "IOMUX26 = 0 (native PCIE_RST0_L)"
        );
        assert_eq!(
            st.smn[&(0x40, 0x02D0_2568)],
            0xD2_0000,
            "and the hard-coded socket-1 bus"
        );
    }
}
