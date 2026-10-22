// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native cold-boot DXIO sequencing.
use super::{
    acp,
    dxio::{
        self, Descriptor, EngineConfig, Error, Event, Hotplug, LinkSpeed, Operation, PowerPolicy,
        Topology,
    },
    registers::{Reg, Smn, SmnReg, apply, rmw},
    smu,
};
use crate::fam17h::nbio::{
    IohcBridgeCntl, NbProgDeviceRemap, iohc_bridge_cntl, nb_prog_device_remap, pcie,
};
use fstart_core::indexed::Indexed;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
use core::{
    cell::UnsafeCell,
    sync::atomic::{Ordering, compiler_fence},
};
use fstart_pci::{ConfigRegionAccess, PciAddress};
use zerocopy::{FromBytes, IntoBytes};

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

/// Resident identity-mapped coherent DRAM, borrowed exclusively for the
/// entire transaction. UnsafeCell permits firmware DMA without holding a
/// Rust reference to the active transfer. No initialized writable global.
#[repr(C, align(16))]
pub struct Buffers {
    engines: UnsafeCell<[u8; 352]>,
    ancillary: UnsafeCell<[u8; 16]>,
}
impl Default for Buffers {
    fn default() -> Self {
        Self::new()
    }
}
impl Buffers {
    pub const fn new() -> Self {
        Self {
            engines: UnsafeCell::new([0; 352]),
            ancillary: UnsafeCell::new([0; 16]),
        }
    }
    fn address(pointer: *mut u8, size: usize) -> Result<u32, Error> {
        let start = pointer as usize;
        let end = start.checked_add(size).ok_or(Error::DmaAddress)?;
        if start & 15 != 0 || end > u32::MAX as usize {
            return Err(Error::DmaAddress);
        }
        Ok(start as u32)
    }
    fn prepare(&mut self, config: &EngineConfig) {
        // SAFETY: exclusive borrow; no transfer is in flight yet.
        unsafe {
            let engines = &mut *self.engines.get();
            engines.fill(0);
            engines[..size_of::<EngineConfig>()].copy_from_slice(config.as_bytes());
            for (bytes, word) in (*self.ancillary.get())
                .as_chunks_mut::<4>()
                .0
                .iter_mut()
                .zip(dxio::ANCILLARY_PSPP)
            {
                bytes.copy_from_slice(&word.to_le_bytes());
            }
        }
        compiler_fence(Ordering::SeqCst);
    }
    fn clear(&mut self) {
        // SAFETY: the prior synchronous DXIO command completed its DMA.
        unsafe {
            (*self.engines.get()).fill(0);
        }
        compiler_fence(Ordering::SeqCst);
    }
    fn snapshot(&self, capacity: usize) -> Result<EngineConfig, Error> {
        compiler_fence(Ordering::SeqCst);
        // SAFETY: caller waits for the synchronous command's DMA completion.
        // This is DRAM, not MMIO; register accesses use the typed transports.
        let bytes = unsafe { self.engines.get().read_volatile() };
        let config = EngineConfig::read_from_bytes(&bytes[..size_of::<EngineConfig>()])
            .map_err(|_| Error::EngineCount)?;
        if config.transfer_size()? > capacity {
            return Err(Error::EngineCount);
        }
        Ok(config)
    }
}
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
    let graphics = [rmw(0x1012_349c, 1 << 2, 1 << 2), rmw(0x1012_34c8, 1 << 17, 1 << 17)]
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
    let hidden =
        IohcBridgeCntl::BRIDGE_DIS::SET + IohcBridgeCntl::CFG_DIS::SET + IohcBridgeCntl::CRS_ENABLE::SET;
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
    apply(smn, [rmw(0x0310_1800, 1 << 3, 1 << 3), rmw(0x0310_1800, 1 << 3, 0)]);
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
        smn.reg(pcie::port_008(i))
            .modify(pcie::Port008::BIT15::SET);
    }
    smn.reg(pcie::cntl(0))
        .modify(pcie::Cntl::RX_RCB_INVALID_SIZE_DIS::CLEAR);
    smn.reg(pcie::p_cntl(0))
        .modify(pcie::PCntl::P_IGNORE_EDB_ERR::SET);
    apply(smn, [rmw(pcie::core_raw(0, 0x100), 1 << 14, 1 << 14)]);
    phy(smn, 0xfffe0083, 0x1453);
    phy(smn, 0xfffe0082, 0x1022);
}

/// Train cold-boot PCIe/SATA without FSP. Failure after submission owns the
/// partially programmed silicon: stop/reset, never fall back to FSP.
///
/// # Safety
/// Buffers must be permanently reserved coherent, identity-mapped,
/// DMA-accessible DRAM. Their static exclusive borrow is consumed even on
/// failure: a timeout does NOT establish DMA quiescence. Keep them mapped,
/// resident and unmodified until platform reset; do not reclaim or retry on
/// any post-submission error. The caller exclusively owns SMN/PHY/PCI access,
/// supplies a working monotonic microsecond clock, and has released board PERST
/// and power sequencing before training. Firmware must honor DMA completion.
#[allow(clippy::too_many_arguments)]
pub unsafe fn train(
    smn: &Smn<impl ConfigRegionAccess>,
    pci: &impl ConfigRegionAccess,
    buffers: &'static mut Buffers,
    descriptors: &[Descriptor],
    options: Options,
    now_us: &mut impl FnMut() -> u64,
    delay_us: &mut impl FnMut(u32),
) -> Result<Topology, Error> {
    let config = EngineConfig::build(descriptors, options.power)?;
    if options.hda_enabled && !options.acp_enabled {
        return Err(Error::AudioPolicy);
    }
    let size = config.transfer_size()?;
    let dma = Buffers::address(buffers.engines.get().cast(), size)?;
    let ancillary = Buffers::address(buffers.ancillary.get().cast(), 16)?;
    buffers.prepare(&config);
    smu::request(smn, smu::TEST, [0xaa55aa55, 0, 0, 0, 0, 0], delay_us).map_err(Error::Smu)?;
    pre_training(smn, options);
    acp::power_off(smn, options.acp_enabled, delay_us).map_err(Error::Acp)?;
    let version = dxio::rpc(smn, Operation::Version, [0; 6], delay_us)?;
    if version[0] & 0xff != 3 {
        return Err(Error::Result(version[0]));
    }
    dxio::rpc(smn, Operation::Init, [0; 6], delay_us)?;
    for (variable, value) in [(0x0e, 1), (0x0f, 1), (0x10, 1)] {
        dxio::rpc(
            smn,
            Operation::SetVariable,
            [0, variable, value, 0, 0, 0],
            delay_us,
        )?;
    }
    if options.am4 {
        dxio::rpc(smn, Operation::SetVariable, [0, 0x0d, 0, 0, 0, 0], delay_us)?;
    }
    dxio::rpc(
        smn,
        Operation::SetVariable,
        [
            0,
            0x12,
            u32::from(options.silicon_id & (1 << 31) != 0),
            0,
            0,
            0,
        ],
        delay_us,
    )?;
    dxio::rpc(smn, Operation::LoadCapabilities, [0; 6], delay_us)?;
    if options.power != PowerPolicy::Disabled {
        dxio::rpc(
            smn,
            Operation::LoadData,
            [0, 0, ancillary, 4, 0, 6],
            delay_us,
        )?;
    }
    for kind in [2, 5] {
        dxio::rpc(smn, Operation::LoadData, [0, 0, 0, 0, 1, kind], delay_us)?;
    }
    dxio::rpc(
        smn,
        Operation::LoadData,
        [0, 0, dma, size as u32 / 4, 0, 3],
        delay_us,
    )?;
    let started = now_us();
    dxio::rpc(smn, Operation::Start, [0; 6], delay_us)?;
    let mut mapped = None;
    let mut configured_once = false;
    for _ in 0..60_000 {
        if now_us().wrapping_sub(started) >= 60_000_000 {
            return Err(Error::Timeout);
        }
        let reply = dxio::rpc(smn, Operation::State, [0; 6], delay_us)?;
        match dxio::event(reply)? {
            Event::Mapped => {
                if mapped.is_some() {
                    return Err(Error::StateMachine);
                }
                buffers.clear();
                dxio::rpc(
                    smn,
                    Operation::EngineConfig,
                    [0, 0, dma, size as u32 / 4, 0, 0],
                    delay_us,
                )?;
                let snapshot = buffers.snapshot(size)?;
                let (topology, map) = Topology::mapped(&snapshot, descriptors)?;
                remap(smn, map);
                mapped = Some(topology);
            }
            Event::Configured => {
                if configured_once {
                    return Err(Error::StateMachine);
                }
                configured(smn, pci, mapped.as_ref().ok_or(Error::StateMachine)?);
                configured_once = true;
            }
            Event::Done => {
                // The two requested return events must precede success.
                let mut topology = mapped.ok_or(Error::StateMachine)?;
                if !configured_once {
                    return Err(Error::StateMachine);
                }
                buffers.clear();
                dxio::rpc(
                    smn,
                    Operation::EngineConfig,
                    [0, 0, dma, size as u32 / 4, 0, 0],
                    delay_us,
                )?;
                let snapshot = buffers.snapshot(size)?;
                let inhibit = smn.reg(pcie::link_status(0)).get();
                topology.training_done(&snapshot, descriptors, inhibit)?;
                if now_us().wrapping_sub(started) >= 60_000_000 {
                    return Err(Error::Timeout);
                }
                post_training(smn, &topology);
                return Ok(topology);
            }
            Event::Resume => {}
        }
        dxio::rpc(smn, Operation::Resume, [0; 6], delay_us)?;
        // Also bound attempts if the supplied clock malfunctions.
        delay_us(1000);
    }
    Err(Error::Timeout)
}
