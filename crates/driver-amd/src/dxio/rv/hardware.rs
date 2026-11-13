// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native cold-boot DXIO sequencing.
use super::super::wire::Heaps;
use super::super::{
    DmaArea, Dxio, DxioError, Hooks, LoopRules, TIMEOUT_US, Unit, UnitView, run, state, var,
};
use super::{
    ANCILLARY_PSPP, Descriptor, Error, Hotplug, LinkSpeed, PowerPolicy, Topology, engines,
};
use crate::nbio::{
    iohc::{IohcBridgeCntl, NbProgDeviceRemap, iohc_bridge_cntl, nb_prog_device_remap},
    pcie,
};
use crate::smn::{Reg, Smn, SmnReg, apply, rmw};
use crate::smu::v10 as smu;
use crate::soc::Soc;
use crate::{acp, df::Die};
use fstart_core::indexed::Indexed;
use fstart_core::services::{Timer, TimerDelay};
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

const PHY_INDEX: SmnReg = SmnReg::new(0x0004_a34c);
const PHY_DATA: SmnReg = SmnReg::new(0x0004_a350);
/// PCIe strap `index` of the core, behind the PHY index/data pair.
fn phy(smn: &Smn<impl ConfigRegionAccess>, index: u32, value: u32) {
    Indexed::<_, _, ()>::new(smn.reg(PHY_INDEX), smn.reg(PHY_DATA), index).set(value);
}

register_bitfields![u32,
    /// NBIF function strap.
    NbifStrap [ENABLE OFFSET(28) NUMBITS(1) []],
    /// NBIF function enables, bit n = function n.
    NbifFunctionEnable [
        HDA OFFSET(6) NUMBITS(1) [],
        F8 OFFSET(8) NUMBITS(1) [],
        F9 OFFSET(9) NUMBITS(1) [],
        F10 OFFSET(10) NUMBITS(1) []
    ]
];
/// The NBIF straps of the functions of device 8.1 (NBIF0 function n).
const fn nbif_strap(function: u32) -> SmnReg<NbifStrap::Register> {
    SmnReg::new(0x1013_4000 + function * 0x200)
}
const NBIF_FUNCTION_ENABLE: SmnReg<NbifFunctionEnable::Register> = SmnReg::new(0x1013_a008);
const HDA: u32 = 6;

#[derive(Debug, Clone, Copy)]
pub struct Options {
    pub power: PowerPolicy,
    pub am4: bool,
    pub silicon_id: u32,
    pub graphics: bool,
    pub hda_enabled: bool,
    pub acp_enabled: bool,
}

/// Raw vendor records of the pre-training sequence.
fn nbif_vendor(graphics: bool) -> impl Iterator<Item = Reg> {
    let acs = core::iter::once(rmw(0x1013_1200, 0x1c, 0x1c))
        .chain((8..=10).map(|function| rmw(0x1013_4008 + function * 0x200, 0x13_0000, 0x13_0000)));
    let graphics = [
        rmw(0x1012_349c, 1 << 2, 1 << 2),
        rmw(0x1012_34c8, 1 << 17, 1 << 17),
    ]
    .into_iter()
    .filter(move |_| graphics);
    acs.chain(graphics)
}

fn pre_training(smn: &Smn<impl ConfigRegionAccess>, options: Options) {
    for function in [9, 10] {
        smn.reg(nbif_strap(function))
            .modify(NbifStrap::ENABLE::CLEAR);
        let enable = smn.reg(NBIF_FUNCTION_ENABLE);
        enable.modify(if function == 9 {
            NbifFunctionEnable::F9::CLEAR
        } else {
            NbifFunctionEnable::F10::CLEAR
        });
    }
    smn.reg(nbif_strap(HDA))
        .modify(NbifStrap::ENABLE.val(options.hda_enabled.into()));
    smn.reg(NBIF_FUNCTION_ENABLE)
        .modify(NbifFunctionEnable::HDA.val(options.hda_enabled.into()));
    // [7:0] kept, the rest replaced by 4.
    for address in [0x1010_0004, 0x1010_1004] {
        let reg = smn.reg(SmnReg::<()>::new(address));
        reg.set((reg.get() & 0xff) | 4);
    }
    for i in (0..8).rev() {
        smn.reg(pcie::tx_cntl(i))
            .modify(pcie::TxCntl::TX_FLUSH_TLP_DIS::CLEAR);
        smn.reg(pcie::lc_link_width_cntl(i))
            .modify(pcie::LcLinkWidthCntl::LC_DYN_LANES_PWR_STATE.val(3));
    }
    phy(smn, 0xfffe0002, 1);
    for index in 0xfffe0069..0xfffe006f {
        phy(smn, index, 1);
    }
    phy(smn, 0xfffe0068, 1);
    apply(smn, nbif_vendor(options.graphics));
    for index in (0xfffe00c5..0xfffe02fc).step_by(0x51) {
        phy(smn, index, 7);
    }
    apply(smn, [rmw(0x11a0_b014, 0xf0, 0x40)]);
    smn.reg(nbif_strap(8)).modify(NbifStrap::ENABLE::CLEAR);
    smn.reg(NBIF_FUNCTION_ENABLE)
        .modify(NbifFunctionEnable::F8::CLEAR);
}
fn remap(smn: &Smn<impl ConfigRegionAccess>, map: [u8; 7]) {
    for (i, devfn) in map.into_iter().enumerate() {
        let (device, function) = fstart_pci::devfn_parts(devfn);
        smn.reg(nb_prog_device_remap(i))
            .modify(NbProgDeviceRemap::DEVFN.val(devfn.into()));
        smn.reg(pcie::port_084(i))
            .modify(pcie::Port084::DEVICE.val(device.into()));
        smn.reg(pcie::port_008(i))
            .modify(pcie::Port008::FUNCTION.val(function.into()));
    }
}
fn configured(
    smn: &Smn<impl ConfigRegionAccess>,
    pci: &impl ConfigRegionAccess,
    topology: &Topology,
) {
    for p in topology.ports.iter().rev() {
        let Some(d) = p.descriptor else {
            continue;
        };
        let preset: u32 = if matches!(d.speed, LinkSpeed::Maximum | LinkSpeed::Gen3) {
            0x1bb7
        } else {
            0
        };
        let (device, function) = fstart_pci::devfn_parts(p.devfn);
        let address = PciAddress::new(0, 0, device, function);
        // SAFETY: live mapped root-port config space, aligned valid offsets.
        let mut eq = unsafe { pci.read(address, 0x27c) } & 0x8080;
        eq |= preset & 0x7f;
        eq |= (((preset >> 8 & 7) * 2) | (preset >> 7 & 1) | (preset >> 7 & 0x70)) << 8;
        for reg in (0x27c..=0x298).step_by(4) {
            // SAFETY: same validated config-space mapping.
            unsafe {
                pci.write(address, reg, eq * 0x10001);
            }
        }
        phy(smn, 0xfffe0088, preset >> 4 & 7);
        phy(smn, 0xfffe0089, preset >> 11 & 7);
        phy(smn, 0xfffe008a, preset & 0xf);
        phy(
            smn,
            0xfffe008b,
            ((preset >> 8 & 7) << 1) | (preset >> 7 & 1),
        );
    }
}
fn post_training(smn: &Smn<impl ConfigRegionAccess>, topology: &Topology) {
    let hidden = IohcBridgeCntl::BRIDGE_DIS::SET
        + IohcBridgeCntl::CFG_DIS::SET
        + IohcBridgeCntl::CRS_ENABLE::SET;
    for i in 0..7 {
        smn.reg(iohc_bridge_cntl(i)).modify(hidden);
    }
    for p in topology.ports.iter().rev() {
        let hotplug = p
            .descriptor
            .is_some_and(|d| !matches!(d.hotplug, Hotplug::Disabled | Hotplug::Inboard));
        if p.trained || hotplug {
            smn.reg(iohc_bridge_cntl(p.devfn_index.into())).modify(
                IohcBridgeCntl::BRIDGE_DIS::CLEAR
                    + IohcBridgeCntl::CFG_DIS::CLEAR
                    + IohcBridgeCntl::CRS_ENABLE::SET,
            );
        }
    }
    apply(
        smn,
        [
            rmw(0x0310_1800, 1 << 3, 1 << 3),
            rmw(0x0310_1800, 1 << 3, 0),
        ],
    );
    for i in (0..8).rev() {
        smn.reg(pcie::tx_cntl(i))
            .modify(pcie::TxCntl::TX_FLUSH_TLP_DIS::CLEAR);
    }
    for i in (0..8).rev() {
        smn.reg(pcie::lc_cntl(i))
            .modify(pcie::LcCntl::LC_L1_IMMEDIATE_ACK::CLEAR);
        apply(
            smn,
            [
                rmw(pcie::port_raw(i, 0x2d8), 0x10310, 0x100),
                rmw(pcie::port_raw(i, 0x2f0), 0x0700_0000, 0),
                rmw(pcie::port_raw(i, 0x2e0), 1 << 19, 1 << 19),
            ],
        );
        let lc_cntl6 = smn.reg(pcie::lc_cntl6(i));
        lc_cntl6.modify(
            pcie::LcCntl6::LC_SPC_MODE_2P5GT::CLEAR
                + pcie::LcCntl6::LC_SPC_MODE_5GT::SET
                + pcie::LcCntl6::LC_SPC_MODE_8GT::SET,
        );
        lc_cntl6.modify(pcie::LcCntl6::LC_L1_POWERDOWN::CLEAR);
        if topology.ports[i]
            .descriptor
            .is_some_and(|d| d.hotplug == Hotplug::Enhanced)
        {
            apply(smn, [rmw(pcie::port_raw(i, 0x074), 0, 1 << 3)]);
        }
    }
    smn.reg(pcie::strap_f0(0))
        .modify(pcie::StrapF0::PCD_D1::SET);
    for i in 0..7 {
        smn.reg(pcie::port_304(i))
            .modify(pcie::Port304::BITS.val(3));
        smn.reg(pcie::device_control2(i))
            .modify(pcie::DeviceControl2::COMPLETION_TIMEOUT.val(6));
        smn.reg(pcie::lc_cntl(i))
            .modify(pcie::LcCntl::LC_L1_IMMEDIATE_ACK::SET);
        smn.reg(pcie::lc_link_width_cntl(i))
            .modify(pcie::LcLinkWidthCntl::LC_DUAL_END_RECONFIG_EN::SET);
        smn.reg(pcie::lc_cntl2(i))
            .modify(pcie::LcCntl2::LC_ELEC_IDLE_MODE.val(1));
        smn.reg(pcie::port_008(i)).modify(pcie::Port008::BIT15::SET);
    }
    smn.reg(pcie::cntl(0))
        .modify(pcie::Cntl::RX_RCB_INVALID_SIZE_DIS::CLEAR);
    smn.reg(pcie::p_cntl(0))
        .modify(pcie::PCntl::P_IGNORE_EDB_ERR::SET);
    apply(smn, [rmw(pcie::core_raw(0, 0x100), 1 << 14, 1 << 14)]);
    phy(smn, 0xfffe0083, 0x1453);
    phy(smn, 0xfffe0082, 0x1022);
}

/// The events of the training: the remap after the mapping, the
/// equalization presets once configured. Each happens once, in that order.
struct Training<'a, S, P> {
    die: &'a Die<S, P>,
    descriptors: &'a [Descriptor],
    mapped: Option<Topology>,
    configured: bool,
}
impl<S: Soc, P: ConfigRegionAccess + Copy> Hooks for Training<'_, S, P> {
    type Error = Error;

    fn mapped(&mut self, unit: &UnitView<'_>) -> Result<(), Error> {
        if self.mapped.is_some() {
            return Err(DxioError::StateMachine {
                state: state::MAPPED,
            }
            .into());
        }
        let (topology, map) = Topology::mapped(unit.engines, self.descriptors)?;
        remap(&self.die.smn(), map);
        self.mapped = Some(topology);
        Ok(())
    }
    fn configured(&mut self, _unit: &UnitView<'_>) -> Result<(), Error> {
        let state = DxioError::StateMachine {
            state: state::CONFIGURED,
        };
        if self.configured {
            return Err(state.into());
        }
        let mapped = self.mapped.as_ref().ok_or(state)?;
        configured(&self.die.smn(), &self.die.pci(), mapped);
        self.configured = true;
        Ok(())
    }
    // The board released PERST before the training.
}

/// Resumes are followed by 1 ms; the board released PERST before.
const RULES: LoopRules = LoopRules {
    pause_at: &[],
    perst: false,
    perst_deassert_delay_ms: 0,
    resume_delay_us: 1000,
};

/// Train cold-boot PCIe/SATA without FSP. Failure after submission owns the
/// partially programmed silicon: stop/reset, never fall back to FSP.
///
/// # Safety
/// Buffers must be permanently reserved coherent, identity-mapped,
/// DMA-accessible DRAM. Their static exclusive borrow is consumed even on
/// failure: a timeout does NOT establish DMA quiescence. Keep them mapped,
/// resident and unmodified until platform reset; do not reclaim or retry on
/// any post-submission error. The caller exclusively owns SMN/PHY/PCI access,
/// supplies a working monotonic microsecond timer, and has released board PERST
/// and power sequencing before training. Firmware must honor DMA completion.
pub unsafe fn train<S: Soc>(
    die: &Die<S, impl ConfigRegionAccess + Copy>,
    buffers: &'static mut DmaArea,
    descriptors: &[Descriptor],
    options: Options,
    timer: &impl Timer,
) -> Result<Topology, Error> {
    let delay = &mut TimerDelay(timer);
    let engines = engines(descriptors, options.power)?;
    if options.hda_enabled && !options.acp_enabled {
        return Err(Error::AudioPolicy);
    }
    let pspp: [u8; 16] = zerocopy::transmute!(ANCILLARY_PSPP.map(u32::to_le));
    let ancillary: &[u8] = if options.power == PowerPolicy::Disabled {
        &[]
    } else {
        &pspp
    };
    let heaps = Heaps::from_engines(&engines, ancillary).ok_or(DxioError::Buffer)?;
    // SAFETY: the caller's contract.
    let dma = unsafe { buffers.buffer() }?;
    let smn = &die.smn();
    smu::MAILBOX.test(smn, delay).map_err(DxioError::Smu)?;
    pre_training(smn, options);
    acp::power_off(smn, options.acp_enabled, delay).map_err(Error::Acp)?;
    let mailbox = die.mailbox(TimerDelay(timer));
    let dxio = Dxio::new(&mailbox, S::DXIO_SERVICE);
    dxio.init()?;
    dxio.version()?;
    for variable in [var::RET_AFTER_MAP, var::RET_AFTER_CONF, var::ANCILLARY_V1] {
        dxio.set_var(variable, 1, 0)?;
    }
    if options.am4 {
        dxio.set_var(var::SKIP_PSP, 0, 0)?;
    }
    let silicon = u32::from(options.silicon_id & (1 << 31) != 0);
    dxio.set_var(var::MAP_EXACT_MATCH, silicon, 0)?;
    dxio.load_caps()?;
    let mut unit = Unit::with_heaps(0, 0, dxio, dma, &heaps, 0..engines.len() as u8)?
        .ok_or(DxioError::Buffer)?;
    unit.load_heaps()?;
    let started = timer.timestamp_us();
    unit.dxio.sm_start()?;
    let mut training = Training {
        die,
        descriptors,
        mapped: None,
        configured: false,
    };
    run(&mut unit, &mut training, timer, &RULES, TIMEOUT_US)?;
    // The two requested return events must precede success.
    let mut topology = training.mapped.ok_or(DxioError::StateMachine {
        state: state::MAPPED,
    })?;
    if !training.configured {
        return Err(DxioError::StateMachine {
            state: state::CONFIGURED,
        }
        .into());
    }
    unit.read_back()?;
    let inhibit = smn.reg(pcie::link_status(0)).get();
    topology.training_done(unit.engines(), descriptors, inhibit)?;
    if timer.timestamp_us().wrapping_sub(started) >= TIMEOUT_US {
        return Err(DxioError::Timeout.into());
    }
    post_training(smn, &topology);
    Ok(topology)
}
