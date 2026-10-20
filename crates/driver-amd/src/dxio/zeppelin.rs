// SPDX-License-Identifier: MIT OR Apache-2.0
//! DXIO (PCIe/SATA lane) training and the PCIe setup around it, following
//! the default path of the vendor AmdNbioPcieZPPei (sp4r2 report
//! NBIO_PCIE_SMU_ZP section 3).
//!
//! The DXIO firmware client is [`crate::dxio`]; this is the Zeppelin
//! lane mapping and the PCIe and NBIF setup around the training.
//!
//! Boards describe the engines of each die with [`Descriptor`]s in die
//! lanes: on every die lanes 0-15 belong to PCIe core 0 (devices 1.x and
//! 2.1), lanes 16-31 to PCIe core 1 (devices 3.x and 4.1). For example the
//! MJ11-EC1 riser-less default x8/x8 split of its reversed x16 slot:
//!
//! ```
//! use fstart_driver_amd::dxio::{Descriptor, SataChannel};
//! const fn pcie(start: u8, end: u8, device: u8, function: u8) -> Descriptor {
//!     Descriptor {
//!         turn_off_unused_lanes: true,
//!         ..Descriptor::pcie(start, end, device, function)
//!     }
//! }
//! pub const MJ11_EC1: &[Descriptor] = &[
//!     Descriptor::sata(0, 3, SataChannel::Other),
//!     pcie(4, 7, 1, 2),   // SlimSAS
//!     pcie(8, 11, 1, 3),  // M.2
//!     pcie(12, 13, 1, 4), // I210 #1
//!     pcie(14, 14, 1, 5), // I210 #2
//!     pcie(15, 15, 1, 1), // AST2500
//!     pcie(31, 24, 3, 1),
//!     pcie(23, 16, 3, 3),
//! ];
//! ```
use super::{
    self as dxio, Descriptor, Engine, EngineCountError, EngineType, Hotplug, LinkSpeed, Operation,
};
use crate::ccx::tsc::{Clock, DelayNs};
use crate::nbio::{
    iohc::{self, IohcBridgeCntl, IohcFeatureCntl, IohcRbBusRoute, NbProgDeviceRemap},
    pcie::{self, PCIE_PORTS_PER_CORE},
    zeppelin::{NBIF_COUNT, PCIE_CORE_COUNT, PCIE_PORT_COUNT, nbif_reg, strap},
};
use crate::smn::{Reg, Smn, SmnReg, apply, rmw};
use crate::smu::v9 as smu;
use crate::zeppelin::{Die, ECAM_BUSES, MAX_DIES};
use fstart_log::{debug, error, info};
use fstart_pci::{ConfigRegionAccess, ConfigRegister, PciAddress, devfn, devfn_parts};
use tock_registers::{
    LocalRegisterCopy,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

/// Engine records of a die's platform descriptor.
pub const ENGINES: usize = 16;
pub type EngineConfig = dxio::EngineConfig<ENGINES>;

/// Default devfn of the PCIe ports of a die, also the order of the bridge
/// control registers.
pub const DEFAULT_DEVFNS: [u8; PCIE_PORT_COUNT] = [
    devfn(1, 1),
    devfn(1, 2),
    devfn(1, 3),
    devfn(1, 4),
    devfn(1, 5),
    devfn(1, 6),
    devfn(1, 7),
    devfn(2, 1),
    devfn(3, 1),
    devfn(3, 2),
    devfn(3, 3),
    devfn(3, 4),
    devfn(3, 5),
    devfn(3, 6),
    devfn(3, 7),
    devfn(4, 1),
];
fn bridge_index(devfn: u8) -> Option<usize> {
    DEFAULT_DEVFNS.iter().position(|&d| d == devfn)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// A descriptor has an invalid lane, device or function, or there are
    /// more than [`ENGINES`].
    Descriptors,
    /// More dies than [`MAX_DIES`].
    Dies,
    /// The firmware placed an engine on lanes or a port no descriptor owns.
    Unmapped {
        start: u8,
        end: u8,
        port: u8,
    },
    DuplicatePort,
    Firmware(dxio::Error),
}
impl From<dxio::Error> for Error {
    fn from(error: dxio::Error) -> Self {
        Self::Firmware(error)
    }
}
impl From<EngineCountError> for Error {
    fn from(error: EngineCountError) -> Self {
        Self::Firmware(error.into())
    }
}

/// A PCIe root port of a die and its training result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Port {
    /// Die lanes.
    pub start_lane: u8,
    pub end_lane: u8,
    pub device: u8,
    pub function: u8,
    /// The descriptor's speed capability.
    pub speed: LinkSpeed,
    pub hotplug: Hotplug,
    pub trained: bool,
}
impl Port {
    pub const fn devfn(&self) -> u8 {
        devfn(self.device, self.function)
    }
}
/// The PCIe ports of a die by port number (core * 8 + port of the core);
/// `None` for ports without an engine.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PcieTopology {
    pub ports: [Option<Port>; PCIE_PORT_COUNT],
}

/// The DXIO lane numbering skips four lanes after die lane 15.
const fn die_to_wire_lane(lane: u8) -> u8 {
    if lane > 15 { lane + 4 } else { lane }
}
const fn wire_to_die_lane(lane: u8) -> u8 {
    if lane > 15 { lane - 4 } else { lane }
}

/// The platform descriptor of a die for the DXIO firmware.
pub fn engine_config(descriptors: &[Descriptor]) -> Result<EngineConfig, Error> {
    let valid = |d: &Descriptor| d.lanes().1 < 32 && d.device < 32 && d.function < 8;
    if descriptors.len() > ENGINES || !descriptors.iter().all(valid) {
        return Err(Error::Descriptors);
    }
    Ok(EngineConfig::from_engines(descriptors.iter().map(|d| {
        let (start, end) = d.lanes();
        // Single-step training with the descriptor speed (vendor PCD 0xe6 =
        // 0). The vendor code turns off unused lanes on every engine.
        let mut engine = Engine::new(
            d,
            (die_to_wire_lane(start), die_to_wire_lane(end)),
            d.speed,
            true,
        );
        engine.gpio_group2 = d.gpio_group;
        engine
    })))
}

/// The DXIO firmware numbers the ports per PCIe core.
fn engine_port(engine: &Engine) -> usize {
    let core = if wire_to_die_lane(engine.start_lane) >= 16 {
        PCIE_PORTS_PER_CORE
    } else {
        0
    };
    core + usize::from(engine.port & 7)
}

/// Place the PCIe engines the firmware mapped at the devfn of their
/// descriptors. Returns the topology and the devfn of every port: the ports
/// without an engine get the remaining default devfns in order.
fn map_ports(
    config: &EngineConfig,
    descriptors: &[Descriptor],
) -> Result<(PcieTopology, [u8; PCIE_PORT_COUNT]), Error> {
    let mut topology = PcieTopology::default();
    let mut used = [false; PCIE_PORT_COUNT];
    for engine in config.engines()? {
        if engine.kind() != EngineType::Pcie as u8 {
            continue;
        }
        let start = wire_to_die_lane(engine.start_lane);
        let end = wire_to_die_lane(engine.end_lane);
        let p = engine_port(engine);
        let unmapped = Error::Unmapped {
            start,
            end,
            port: p as u8,
        };
        let d = descriptors
            .iter()
            .find(|d| d.lanes().0 <= start && d.lanes().1 >= end)
            .ok_or(unmapped)?;
        let index = bridge_index(d.devfn()).ok_or(unmapped)?;
        if used[index] {
            return Err(unmapped);
        }
        // A malformed firmware reply would otherwise leave a devfn unassigned.
        if topology.ports[p].is_some() {
            return Err(Error::DuplicatePort);
        }
        used[index] = true;
        topology.ports[p] = Some(Port {
            start_lane: start,
            end_lane: end,
            device: d.device,
            function: d.function,
            speed: d.speed,
            hotplug: d.hotplug,
            trained: false,
        });
    }

    let mut spare = DEFAULT_DEVFNS
        .iter()
        .zip(used)
        .filter(|(_, used)| !used)
        .map(|(&devfn, _)| devfn);
    let mut map = [0; PCIE_PORT_COUNT];
    for (devfn, port) in map.iter_mut().zip(&topology.ports) {
        *devfn = match port {
            Some(port) => port.devfn(),
            None => spare.next().ok_or(Error::DuplicatePort)?,
        };
    }
    Ok((topology, map))
}

register_bitfields![u16,
    /// Lane Equalization Control of the secondary PCIe capability, per lane.
    LaneEq [
        DS_TX_PRESET OFFSET(0) NUMBITS(4) [],
        DS_RX_HINT OFFSET(4) NUMBITS(3) [],
        US_TX_PRESET OFFSET(8) NUMBITS(4) [],
        US_RX_HINT OFFSET(12) NUMBITS(3) []
    ]
];
/// Lanes 0 and 1; the registers of lanes 2-15 follow.
const LANE_EQ_CONTROL: u16 = 0x27c;

/// The value of every lane equalization dword: TX preset 7 and RX hint 2
/// in both directions for both lanes, with the reserved bits of lane 0.
fn lane_equalization(lane0: u16) -> u32 {
    let mut lane = LocalRegisterCopy::<u16, LaneEq::Register>::new(lane0);
    lane.modify(
        LaneEq::DS_TX_PRESET.val(7)
            + LaneEq::DS_RX_HINT.val(2)
            + LaneEq::US_TX_PRESET.val(7)
            + LaneEq::US_RX_HINT.val(2),
    );
    u32::from(lane.get()) * 0x10001
}

register_bitfields![u32,
    /// Four IOMUX bytes accessed as one dword.
    IomuxDword [PIN26 OFFSET(16) NUMBITS(8) []]
];
/// GPIO26 (PCIE_RST_L) control and the IOMUX dword of pins 24-27 of the
/// die's FCH, through the SMN alias of its ACPI MMIO (0x02d01000).
const FCH_GPIO26: SmnReg = SmnReg::new(0x02d0_2568);
const FCH_IOMUX_24: SmnReg<IomuxDword::Register> = SmnReg::new(0x02d0_1d18);

/// Link status of a port after the training (vendor ffde7de3).
fn port_trained(smn: &Smn<impl ConfigRegionAccess>, p: usize) -> bool {
    let i = p % PCIE_PORTS_PER_CORE;
    if i == 0 {
        !smn.reg(pcie::lc_port_2dc(p))
            .is_set(pcie::LcPort2dc::NOT_TRAINED)
    } else {
        let status = smn.reg(pcie::link_status(p / PCIE_PORTS_PER_CORE));
        status.read(pcie::LinkStatus::NOT_TRAINED) & 1 << i == 0
    }
}

/// Training results of the final engine configuration.
fn training_done(
    smn: &Smn<impl ConfigRegionAccess>,
    config: &EngineConfig,
    topology: &mut PcieTopology,
) -> Result<(), Error> {
    for engine in config.engines()? {
        if engine.kind() != EngineType::Pcie as u8 {
            continue;
        }
        let p = engine_port(engine);
        let port = topology.ports[p].as_mut().ok_or(Error::Unmapped {
            start: wire_to_die_lane(engine.start_lane),
            end: wire_to_die_lane(engine.end_lane),
            port: p as u8,
        })?;
        port.trained = engine.present() && port_trained(smn, p);
    }
    Ok(())
}

/// The DXIO firmware reads and writes the platform descriptor of each die
/// with DMA: one slot per die.
pub struct Buffers([dxio::Slot<ENGINES>; MAX_DIES]);
impl Default for Buffers {
    fn default() -> Self {
        Self::new()
    }
}
impl Buffers {
    pub const fn new() -> Self {
        Self([const { dxio::Slot::new() }; MAX_DIES])
    }
}

/// The DXIO training of one die.
struct Training<'a, P> {
    die: &'a Die<P>,
    descriptors: &'a [Descriptor],
    buffer: &'a dxio::Slot<ENGINES>,
    /// Transfer size of the platform descriptor.
    size: usize,
    topology: PcieTopology,
}
impl<P: ConfigRegionAccess + Copy> dxio::Training for Training<'_, P> {
    type Error = Error;

    fn mapped(&mut self, delay: &mut impl DelayNs) -> Result<(), Error> {
        let config = self.buffer.read_back(self.die, self.size, delay)?;
        let (topology, map) = map_ports(&config, self.descriptors).inspect_err(|e| {
            if let Error::Unmapped { start, end, port } = e {
                error!("DXIO: can't map lanes {}-{} (port {})", start, end, port);
            }
        })?;
        let smn = self.die.smn();
        for (p, devfn) in map.into_iter().enumerate() {
            smn.reg(iohc::nb_prog_device_remap(p))
                .modify(NbProgDeviceRemap::DEVFN.val(devfn.into()));
            let (device, function) = devfn_parts(devfn);
            debug!(
                "DXIO {}: port {} -> {}.{}",
                self.die.index(),
                p,
                device,
                function
            );
        }
        self.topology = topology;
        Ok(())
    }

    /// The vendor ffdeb04b/ffdeb296.
    fn configured(&mut self) -> Result<(), Error> {
        let smn = self.die.smn();
        for (p, port) in self.topology.ports.iter().enumerate() {
            let Some(port) = port else { continue };
            // Vendor PCDs 0xe0 (0), 0xe1 (1) and 0xe5 (0).
            smn.reg(pcie::lc_cntl6(p)).modify(
                pcie::LcCntl6::LC_SPC_MODE_2P5GT::CLEAR + pcie::LcCntl6::LC_SPC_MODE_5GT::SET,
            );
            smn.reg(pcie::lc_cntl7(p))
                .modify(pcie::LcCntl7::LC_SCHEDULED_RXEQEVAL_INTERVAL.val(0));

            // Lane equalization control of the secondary PCIe capability for
            // ports that may train at gen 3: TX preset 7 and RX hint 2 in
            // both directions.
            if !matches!(port.speed, LinkSpeed::Maximum | LinkSpeed::Gen3) {
                continue;
            }
            let address = PciAddress::new(0, self.die.bus(), port.device, port.function);
            let pci = self.die.pci();
            // SAFETY: the mapped root port on the die's root bus.
            let control = |offset| unsafe { ConfigRegister::<_, ()>::new(pci, address, offset) };
            let value = lane_equalization(control(LANE_EQ_CONTROL).get() as u16);
            for offset in (LANE_EQ_CONTROL..LANE_EQ_CONTROL + 0x20).step_by(4) {
                control(offset).set(value);
            }
        }
        Ok(())
    }

    /// The vendor code answers both assert and deassert by giving GPIO26
    /// (PCIE_RST_L) back to its native function on the FCH of that die,
    /// through the SMN alias of the FCH ACPI MMIO (0x02d01000).
    fn perst(&mut self, _deassert: bool) {
        let smn = self.die.smn();
        smn.reg(FCH_GPIO26).set(0x00d2_0000);
        smn.reg(FCH_IOMUX_24)
            .modify(IomuxDword::PIN26.val(0));
    }
}
impl<P: ConfigRegionAccess + Copy> Training<'_, P> {
    fn train(&mut self, clock: &mut impl Clock) -> Result<(), Error> {
        let address = self.buffer.address()?;
        let die = self.die;
        dxio::init(die, clock)?;
        for variable in [
            dxio::VAR_RET_AFTER_MAP,
            dxio::VAR_RET_AFTER_CONF,
            dxio::VAR_ANCILLARY_V1,
        ] {
            dxio::set_variable(die, variable, 1, clock)?;
        }
        dxio::rpc(die, Operation::LoadCapabilities, [0; 6], clock)?;
        // No ancillary data: the descriptors have no tuning records.
        for kind in [dxio::DATA_MACPCS, dxio::DATA_GPIO] {
            dxio::load_data(die, kind, None, clock)?;
        }
        let dwords = (self.size / 4) as u32;
        dxio::load_data(die, dxio::DATA_ENGINE_CONFIG, Some((address, dwords)), clock)?;

        dxio::run(die, self, clock)?;
        let config = self.buffer.read_back(die, self.size, clock)?;
        training_done(&die.smn(), &config, &mut self.topology)?;
        for port in self.topology.ports.iter().flatten() {
            info!(
                "PCIe {}:{}.{} lanes {}-{}: {}",
                die.index(),
                port.device,
                port.function,
                port.start_lane,
                port.end_lane,
                if port.trained {
                    "trained"
                } else {
                    "not trained"
                }
            );
        }
        Ok(())
    }
}

/// PCIe straps of a core (see [`strap`]).
mod straps {
    /// ACS (vendor CBS "ACS Enable", default enabled), in the vendor order.
    pub const ACS: [u32; 8] = [
        0xfffe_0002,
        0xfffe_0069,
        0xfffe_006a,
        0xfffe_006b,
        0xfffe_006c,
        0xfffe_006d,
        0xfffe_006e,
        0xfffe_0068,
    ];
    pub const SUBSYSTEM_VENDOR: u32 = 0xfffe_0082;
    pub const SUBSYSTEM_DEVICE: u32 = 0xfffe_0083;
    /// 0 or 5: the PLL may be off in L1.
    pub const PLL_OFF_IN_L1: u32 = 0xfffe_0084;
}

register_bitfields![u32,
    /// NBIF function enables, bit n = function n.
    NbifFunctionEnable [SATA OFFSET(2) NUMBITS(1) []],
    /// The PCI Express capability header (the root ports' at 0x58).
    PcieCapability [SLOT_IMPLEMENTED OFFSET(24) NUMBITS(1) []]
];
/// NBIF1 of a die: device 8.1 with SATA as function 2.
const NBIF1_FUNCTION_ENABLE: SmnReg<NbifFunctionEnable::Register> = SmnReg::new(0x1023_a008);
const PCIE_CAPABILITY: u16 = 0x58;
const DEVICE_CONTROL2: u16 = PCIE_CAPABILITY + 0x28;

/// ACS-related settings of the NBIFs: raw vendor values.
fn nbif_acs() -> impl Iterator<Item = Reg> {
    (0..NBIF_COUNT).flat_map(|n| {
        let functions =
            (0..8).map(move |function| rmw(nbif_reg(n, 0x34008 + 0x200 * function), 0x13_0000, 0x13_0000));
        core::iter::once(rmw(nbif_reg(n, 0x31000), 0x1c, 0x1c)).chain(functions)
    })
}

fn pre_training(smn: &Smn<impl ConfigRegionAccess>) {
    for p in 0..PCIE_PORT_COUNT {
        smn.reg(pcie::tx_cntl(p))
            .modify(pcie::TxCntl::TX_FLUSH_TLP_DIS::CLEAR);
    }
    for core in 0..PCIE_CORE_COUNT {
        for acs in straps::ACS {
            strap(smn, core, acs).set(1);
        }
    }
    apply(smn, nbif_acs());
}

/// Enable the SATA function of the NBIF.
fn nbif_sata_enable(smn: &Smn<impl ConfigRegionAccess>) {
    smn.reg(crate::fch::taishan::sata::NBIF_STRAP)
        .modify(crate::fch::taishan::NbifStrap::ENABLE::SET);
    smn.reg(NBIF1_FUNCTION_ENABLE)
        .modify(NbifFunctionEnable::SATA::SET);
}

/// Hide all bridges, then show the ones with a link or a hotplug slot.
fn bridges(smn: &Smn<impl ConfigRegionAccess>, topology: &PcieTopology) {
    for bridge in 0..PCIE_PORT_COUNT {
        smn.reg(iohc::iohc_bridge_cntl(bridge)).modify(
            IohcBridgeCntl::BRIDGE_DIS::SET
                + IohcBridgeCntl::CFG_DIS::SET
                + IohcBridgeCntl::CRS_ENABLE::SET,
        );
    }
    for port in topology.ports.iter().flatten() {
        let Some(bridge) = bridge_index(port.devfn()) else {
            continue;
        };
        if port.trained || !matches!(port.hotplug, Hotplug::Disabled | Hotplug::Inboard) {
            smn.reg(iohc::iohc_bridge_cntl(bridge)).modify(
                IohcBridgeCntl::BRIDGE_DIS::CLEAR
                    + IohcBridgeCntl::CFG_DIS::CLEAR
                    + IohcBridgeCntl::CRS_ENABLE::SET,
            );
        }
    }
}

/// PcieInterfaceAfterDxioDoneSetting of the vendor code.
fn interface_after_dxio<P: ConfigRegionAccess + Copy>(die: &Die<P>, topology: &PcieTopology) {
    let smn = die.smn();
    let pci = die.pci();
    for core in 0..PCIE_CORE_COUNT {
        let ports = core * PCIE_PORTS_PER_CORE..(core + 1) * PCIE_PORTS_PER_CORE;
        let pll = strap(&smn, core, straps::PLL_OFF_IN_L1).get();
        for p in ports.clone() {
            if let Some(port) = &topology.ports[p] {
                let address = PciAddress::new(0, die.bus(), port.device, port.function);
                // SAFETY: a mapped root port on the die's root bus.
                let (control2, capability) = unsafe {
                    (
                        ConfigRegister::<_, pcie::DeviceControl2::Register>::new(
                            pci,
                            address,
                            DEVICE_CONTROL2,
                        ),
                        ConfigRegister::<_, PcieCapability::Register>::new(
                            pci,
                            address,
                            PCIE_CAPABILITY,
                        ),
                    )
                };
                control2.modify(pcie::DeviceControl2::COMPLETION_TIMEOUT.val(6));
                capability.modify(PcieCapability::SLOT_IMPLEMENTED::SET);
            }
            if pll == 0 || pll == 5 {
                smn.reg(pcie::lc_cntl6(p))
                    .modify(pcie::LcCntl6::LC_L1_POWERDOWN::SET);
            }
        }

        // Vendor PCDs 0xd1/0xd2 (1) and 0xdf (3).
        let strap_f0 = smn.reg(pcie::strap_f0(core));
        strap_f0.modify(pcie::StrapF0::PCD_D1::SET);
        strap_f0.modify(pcie::StrapF0::PCD_DF.val(3));

        for p in ports {
            smn.reg(pcie::port_304(p))
                .modify(pcie::Port304::BITS.val(3));
            smn.reg(pcie::device_control2(p))
                .modify(pcie::DeviceControl2::COMPLETION_TIMEOUT.val(6));
            smn.reg(pcie::lc_cntl(p))
                .modify(pcie::LcCntl::LC_L1_IMMEDIATE_ACK::SET);
            smn.reg(pcie::lc_link_width_cntl(p))
                .modify(pcie::LcLinkWidthCntl::LC_DUAL_END_RECONFIG_EN::SET);
            smn.reg(pcie::lc_cntl2(p))
                .modify(pcie::LcCntl2::LC_ELEC_IDLE_MODE.val(1));
            smn.reg(pcie::port_008(p))
                .modify(pcie::Port008::BIT15::SET);
        }

        smn.reg(pcie::cntl(core))
            .modify(pcie::Cntl::RX_RCB_INVALID_SIZE_DIS::CLEAR);
        // P_IGNORE_EDB_ERR = !vendor PCD 0xde (0).
        let p_cntl = smn.reg(pcie::p_cntl(core));
        p_cntl.modify(pcie::PCntl::P_IGNORE_EDB_ERR::SET);
        p_cntl.modify(pcie::PCntl::P_ELEC_IDLE_MODE.val(1));
    }
}

/// Subsystem ID (vendor in bits 15:0) of the NBIO functions and the PCIe
/// ports, vendor PCDs.
fn set_subsystem_ids(smn: &Smn<impl ConfigRegionAccess>, subsystem_id: u32) {
    const REGISTERS: [u32; 17] = [
        0x13b0_0050,
        0x13f0_0078,
        0x1014_204c,
        0x1024_104c,
        0x1024_404c,
        0x1024_504c,
        0x1024_604c,
        0x1024_704c,
        0x1014_004c,
        0x1024_004c,
        0x1034_004c,
        0x1013_1004,
        0x1023_1004,
        0x1033_1004,
        0x1014_104c,
        0x1034_104c,
        0x1024_304c,
    ];
    for address in REGISTERS {
        smn.reg(SmnReg::<()>::new(address)).set(subsystem_id);
    }
    for core in 0..PCIE_CORE_COUNT {
        strap(smn, core, straps::SUBSYSTEM_DEVICE).set(subsystem_id >> 16);
        strap(smn, core, straps::SUBSYSTEM_VENDOR).set(subsystem_id & 0xffff);
    }
}

fn post_training<P: ConfigRegionAccess + Copy>(
    die: &Die<P>,
    topology: &PcieTopology,
    subsystem_id: u32,
    delay: &mut impl DelayNs,
) {
    bridges(&die.smn(), topology);
    // Not on AM4: meaning unknown, the argument depends on the socket count
    // (0xf: one socket). The vendor code ignores the result.
    let _ = dxio::rpc(die, Operation::PostTraining, [0, 0, 0xf, 0, 0, 0], delay);
    interface_after_dxio(die, topology);
    set_subsystem_ids(&die.smn(), subsystem_id);
}

/// Each IOHC learns the bus range of every die: `buses` are the root buses
/// of all dies in order.
fn bus_routing(smn: &Smn<impl ConfigRegionAccess>, die: usize, buses: &[u8]) {
    for (rb, &base) in buses.iter().enumerate() {
        let limit = buses
            .get(rb + 1)
            .map_or(ECAM_BUSES - 1, |&next| u16::from(next) - 1);
        smn.reg(iohc::iohc_rb_bus_route(rb)).write(
            IohcRbBusRoute::BASE.val(base.into())
                + IohcRbBusRoute::LIMIT.val(limit.into())
                + IohcRbBusRoute::REMOTE.val(u32::from(rb != die)),
        );
    }
    // Vendor PCD 0x1d (ARI support), default enabled
    smn.reg(iohc::IOHC_FEATURE_CNTL)
        .modify(IohcFeatureCntl::ARI::SET);
}

/// Train the PCIe and SATA lanes of all dies (AmdNbioPcieZPPei) and set up
/// the PCIe ports around it. `descriptors[die]` are the engines of a die
/// (a die without an entry has none); `subsystem_id` is the PCI subsystem
/// ID of the NBIO functions and PCIe ports (vendor ID in bits 15:0, e.g.
/// 0x1000_1458 on the Gigabyte MJ11). Returns the trained ports per die.
///
/// # Safety
/// `buffers` is identity-mapped DRAM below 4 GiB that stays reserved: the
/// DXIO firmware accesses it with DMA, and a failure does not establish DMA
/// quiescence. `dies` are all dies in order.
pub unsafe fn init<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    descriptors: &[&[Descriptor]],
    subsystem_id: u32,
    buffers: &'static mut Buffers,
    clock: &mut impl Clock,
) -> Result<[PcieTopology; MAX_DIES], Error> {
    let mut topologies = [PcieTopology::default(); MAX_DIES];
    let board = |die: &Die<P>| -> &[Descriptor] {
        descriptors
            .get(usize::from(die.index()))
            .copied()
            .unwrap_or(&[])
    };
    if dies.len() > MAX_DIES {
        return Err(Error::Dies);
    }
    // Reject bad descriptors before touching the hardware.
    let configs = dies
        .iter()
        .map(|die| engine_config(board(die)))
        .collect::<Result<heapless::Vec<_, MAX_DIES>, _>>()?;
    let Some(first) = dies.first() else {
        return Ok(topologies);
    };
    smu::MAILBOX
        .test(&first.smn(), clock)
        .map_err(dxio::Error::Smu)?;
    for die in dies {
        pre_training(&die.smn());
    }

    let per_die = dies
        .iter()
        .zip(&configs)
        .zip(&mut buffers.0)
        .zip(&mut topologies);
    for (((die, config), buffer), topology) in per_die {
        let descriptors = board(die);
        let size = config.transfer_size()?;
        buffer.load(config);
        let buffer = &*buffer;
        if descriptors.iter().any(|d| d.engine == EngineType::Sata) {
            nbif_sata_enable(&die.smn());
        }

        let mut training = Training {
            die,
            descriptors,
            buffer,
            size,
            topology: PcieTopology::default(),
        };
        // Vendor PCD 0xcd/0xce/0xcf select die 0 for the early init cleanup.
        if die.index() == 0 {
            dxio::rpc(die, Operation::CleanupEarly, [0; 6], clock)?;
        }
        if descriptors.is_empty() {
            continue;
        }
        debug!("DXIO {}: {} engines", die.index(), descriptors.len());
        training.train(clock)?;
        *topology = training.topology;
    }

    for (die, topology) in dies.iter().zip(&topologies) {
        post_training(die, topology, subsystem_id, clock);
    }
    let buses: heapless::Vec<u8, MAX_DIES> = dies.iter().map(Die::bus).collect();
    for (i, die) in dies.iter().enumerate() {
        bus_routing(&die.smn(), i, &buses);
    }
    Ok(topologies)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::{dxio::SataChannel, smn::testing::SmnSpace};
    use zerocopy::{IntoBytes, byteorder::U32};

    const fn pcie(start: u8, end: u8, device: u8, function: u8) -> Descriptor {
        Descriptor {
            turn_off_unused_lanes: true,
            ..Descriptor::pcie(start, end, device, function)
        }
    }
    /// MJ11-EC1 without a riser.
    const MJ11_EC1: &[Descriptor] = &[
        Descriptor::sata(0, 3, SataChannel::Other),
        pcie(4, 7, 1, 2),
        pcie(8, 11, 1, 3),
        pcie(12, 13, 1, 4),
        pcie(14, 14, 1, 5),
        pcie(15, 15, 1, 1),
        pcie(31, 24, 3, 1),
        pcie(23, 16, 3, 3),
    ];

    #[test]
    fn mj11_ec1_x8x8_wire_format() {
        let config = engine_config(MJ11_EC1).unwrap();
        let bytes = config.as_bytes();
        assert_eq!(config.transfer_size(), Ok(16 + 8 * 40 + 8));
        assert_eq!(&bytes[12..14], &[8, 0]);
        let engine = |i: usize| &bytes[16 + i * 40..16 + (i + 1) * 40];
        // SATA: lanes, GPIO groups and nothing else.
        assert_eq!(
            &engine(0)[..0x14],
            &[3, 0, 0, 3, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        // x8 on lanes 31..24: wire lanes 28-35, 3.1, present, reversed,
        // flag 19 and lane power-off.
        assert_eq!(
            &engine(6)[..0x14],
            &[
                1, 0, 28, 35, 1, 1, 0, 0, 0, 0, 0, 0, 0x19, 0, 0, 0, 0x09, 0, 0x18, 0
            ]
        );
        // x8 on lanes 23..16: wire lanes 20-27, 3.3.
        assert_eq!(
            &engine(7)[..0x14],
            &[
                1, 0, 20, 27, 1, 1, 0, 0, 0, 0, 0, 0, 0x1b, 0, 0, 0, 0x09, 0, 0x18, 0
            ]
        );
        assert!(engine(7)[0x14..].iter().all(|&b| b == 0));
        assert!(matches!(
            engine_config(&[pcie(32, 32, 1, 1)]),
            Err(Error::Descriptors)
        ));
    }

    /// The engine configuration as the firmware returns it after mapping.
    fn mapped(ports: [u8; 7]) -> EngineConfig {
        let mut config = engine_config(MJ11_EC1).unwrap();
        for (engine, port) in config.engines[1..].iter_mut().zip(ports) {
            engine.port = port;
        }
        config
    }

    #[test]
    fn ports_get_their_descriptor_devfn_and_spares_the_rest() {
        let (topology, map) = map_ports(&mapped([0, 1, 2, 3, 4, 1, 0]), MJ11_EC1).unwrap();
        let devfns = [
            (1, 2),
            (1, 3),
            (1, 4),
            (1, 5),
            (1, 1),
            (1, 6),
            (1, 7),
            (2, 1),
            (3, 3),
            (3, 1),
            (3, 2),
            (3, 4),
            (3, 5),
            (3, 6),
            (3, 7),
            (4, 1),
        ];
        assert_eq!(map, devfns.map(|(d, f)| devfn(d, f)));
        let port = topology.ports[9].unwrap();
        assert_eq!(
            (port.start_lane, port.end_lane, port.devfn()),
            (24, 31, devfn(3, 1))
        );
        assert!(topology.ports[5].is_none());

        assert_eq!(
            map_ports(&mapped([0, 1, 2, 3, 3, 1, 0]), MJ11_EC1),
            Err(Error::DuplicatePort)
        );
        let mut config = mapped([0, 1, 2, 3, 4, 1, 0]);
        config.engines[7].start_lane = 18; // die lanes 14-23 belong to no descriptor
        assert!(matches!(
            map_ports(&config, MJ11_EC1),
            Err(Error::Unmapped { .. })
        ));
    }

    #[test]
    fn training_result_needs_present_engine_and_link() {
        let mut config = mapped([0, 1, 2, 3, 4, 1, 0]);
        let (mut topology, _) = map_ports(&config, MJ11_EC1).unwrap();
        let space = SmnSpace::default();
        let smn = space.smn();
        // Core 0: port 1 (M.2) has no link. Core 1: port 0 (3.3) has no link.
        space.set(pcie::link_status(0), 1 << 1);
        space.set(pcie::lc_port_2dc(8), 1 << 28);
        // The firmware dropped the AST2500.
        let flags = &mut config.engines[5].port_flags;
        *flags = U32::new(flags.get() & !1);
        training_done(&smn, &config, &mut topology).unwrap();
        let trained: [bool; 7] =
            core::array::from_fn(|i| topology.ports[[0, 1, 2, 3, 4, 9, 8][i]].unwrap().trained);
        assert_eq!(trained, [true, false, true, true, false, true, false]);

        // Hide everything but trained links and hotplug slots.
        topology.ports[8].as_mut().unwrap().hotplug = Hotplug::Server;
        topology.ports[1].as_mut().unwrap().hotplug = Hotplug::Inboard;
        bridges(&smn, &topology);
        let bridge = |d, f| space.get(iohc::iohc_bridge_cntl(bridge_index(devfn(d, f)).unwrap()));
        assert_eq!(bridge(3, 1), 1 << 18);
        assert_eq!(bridge(3, 3), 1 << 18);
        assert_eq!(bridge(1, 3), 0x40005);
        assert_eq!(bridge(1, 1), 0x40005);
        assert_eq!(bridge(4, 1), 0x40005);
    }

    /// The typed registers keep the vendor (clear, set) masks: every
    /// register starts as a pattern and must end as (pattern & !clear) | set.
    #[test]
    fn port_setup_keeps_the_vendor_masks() {
        const PATTERN: u32 = 0x5a5a_a5a5;
        let pci = crate::zeppelin::test_pci::Pci::new(&[0], |_| [0; 6]);
        let dies = pci.dies();
        let core = |c: u32, offset: u32| 0x1118_0000 + c * 0x10_0000 + offset;
        let port = |p: u32, offset: u32| 0x1114_0000 + (p / 8) * 0x10_0000 + (p % 8) * 0x1000 + offset;
        let mut expected = std::vec::Vec::new();
        for c in 0..2 {
            expected.extend([
                (core(c, 0x2c0), (1 << 18) | 0x6000, (1 << 18) | 0x6000),
                (core(c, 0x040), 1 << 17, 0),
                (core(c, 0x100), (1 << 6) | 0xc000, (1 << 6) | 0x4000),
            ]);
        }
        for p in 0..16 {
            expected.extend([
                (port(p, 0x2ec), 1 << 21, 1 << 21),
                (port(p, 0x304), 0x6, 0x6),
                (0x1110_0080 + (p / 8) * 0x10_0000 + (p % 8) * 0x1000, 0xf, 0x6),
                (port(p, 0x280), 1 << 23, 1 << 23),
                (port(p, 0x288), 1 << 19, 1 << 19),
                (port(p, 0x2c4), 0xc000, 0x4000),
                (port(p, 0x008), 1 << 15, 1 << 15),
            ]);
        }
        for &(address, ..) in &expected {
            pci.set_smn(0, address, PATTERN);
        }
        // The PLL-off strap reads 0: the PLL may be off in L1.
        interface_after_dxio(&dies[0], &PcieTopology::default());
        for (address, clear, set) in expected {
            assert_eq!(pci.smn(0, address), (PATTERN & !clear) | set, "{address:#x}");
        }

        let space = SmnSpace::default();
        let controls = (0..16).map(|p| (port(p, 0x080), 1 << 15, 0));
        let nbif = (0..3u32).flat_map(|n| {
            let base = 0x1010_0000 + n * 0x10_0000;
            core::iter::once((base + 0x31000, 0x1c, 0x1c))
                .chain((0..8).map(move |f| (base + 0x34008 + 0x200 * f, 0x13_0000, 0x13_0000)))
        });
        let expected: std::vec::Vec<_> = controls.chain(nbif).collect();
        for &(address, ..) in &expected {
            space.set(address, PATTERN);
        }
        pre_training(&space.smn());
        for (address, clear, set) in expected {
            assert_eq!(space.get(address), (PATTERN & !clear) | set, "{address:#x}");
        }
        // The ACS straps, through the strap index/data pair of each core.
        assert_eq!(space.get(0x0004_a3c8_u32), 0xfffe_0068);
        assert_eq!(space.get(0x0004_a3cc_u32), 1);
    }

    #[test]
    fn root_bridges_route_every_die_bus_range() {
        let space = SmnSpace::default();
        bus_routing(&space.smn(), 1, &[0x00, 0x40]);
        assert_eq!(space.get(iohc::iohc_rb_bus_route(0)), 0x8000_3f00);
        assert_eq!(space.get(iohc::iohc_rb_bus_route(1)), 0x0000_7f40);
        assert_eq!(space.get(iohc::IOHC_FEATURE_CNTL), 1 << 22);
    }
}
