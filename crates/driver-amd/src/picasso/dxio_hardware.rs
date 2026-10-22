// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native cold-boot DXIO sequencing.
use super::{
    acp,
    dxio::{
        self, Descriptor, EngineConfig, Error, Event, Hotplug, LinkSpeed, Operation, PowerPolicy,
        Topology,
    },
    registers::Smn,
    smu,
};
use core::{
    cell::UnsafeCell,
    sync::atomic::{Ordering, compiler_fence},
};
use fstart_pci::{ConfigRegionAccess, PciAddress};
use zerocopy::{FromBytes, IntoBytes};

const PHY_INDEX: u32 = 0x0004a34c;
const PHY_DATA: u32 = 0x0004a350;
fn port(i: usize, offset: u32) -> u32 {
    0x11140000 + i as u32 * 0x1000 + offset
}
fn core_reg(offset: u32) -> u32 {
    0x11180000 + offset
}
fn bridge(i: u8) -> u32 {
    0x13b31004 + u32::from(i) * 0x400
}
fn phy(smn: &mut impl Smn, index: u32, value: u32) {
    smn.write(PHY_INDEX, index);
    smn.write(PHY_DATA, value);
}

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

fn pre_training(smn: &mut impl Smn, options: Options) {
    for function in [9, 10] {
        smn.update(0x10134000 + function * 0x200, 1 << 28, 0);
        smn.update(0x1013a008, 1 << function, 0);
    }
    smn.update(
        0x10134c00,
        1 << 28,
        if options.hda_enabled { 1 << 28 } else { 0 },
    );
    smn.update(
        0x1013a008,
        1 << 6,
        if options.hda_enabled { 1 << 6 } else { 0 },
    );
    for address in [0x10100004, 0x10101004] {
        let value = smn.read(address);
        smn.write(address, (value & 0xff) | 4);
    }
    for i in (0..8).rev() {
        smn.update(port(i, 0x080), 1 << 15, 0);
        smn.update(port(i, 0x288), 0, 0x600000);
    }
    phy(smn, 0xfffe0002, 1);
    for index in 0xfffe0069..0xfffe006f {
        phy(smn, index, 1);
    }
    phy(smn, 0xfffe0068, 1);
    smn.update(0x10131200, 0x1c, 0x1c);
    for function in 8..=10 {
        smn.update(0x10134008 + function * 0x200, 0x130000, 0x130000);
    }
    if options.graphics {
        smn.update(0x1012349c, 1 << 2, 1 << 2);
        smn.update(0x101234c8, 1 << 17, 1 << 17);
    }
    for index in (0xfffe00c5..0xfffe02fc).step_by(0x51) {
        phy(smn, index, 7);
    }
    smn.update(0x11a0b014, 0xf0, 0x40);
    smn.update(0x10135000, 1 << 28, 0);
    smn.update(0x1013a008, 1 << 8, 0);
}
fn remap(smn: &mut impl Smn, map: [u8; 7]) {
    for (i, devfn) in map.into_iter().enumerate() {
        smn.update(0x13b100c0 + i as u32 * 4, 0xff, u32::from(devfn));
        smn.update(port(i, 0x084), 0xfff8, u32::from(devfn & 0xf8));
        smn.update(port(i, 0x008), 7, u32::from(devfn & 7));
    }
}
fn configured(smn: &mut impl Smn, pci: &impl ConfigRegionAccess, topology: &Topology) {
    for p in topology.ports.iter().rev() {
        let Some(d) = p.descriptor else {
            continue;
        };
        let preset: u32 = if matches!(d.speed, LinkSpeed::Maximum | LinkSpeed::Gen3) {
            0x1bb7
        } else {
            0
        };
        let address = PciAddress::new(0, 0, p.devfn >> 3, p.devfn & 7);
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
fn post_training(smn: &mut impl Smn, topology: &Topology) {
    for i in 0..7 {
        smn.update(bridge(i), 0x40005, 0x40005);
    }
    for p in topology.ports.iter().rev() {
        let hotplug = p
            .descriptor
            .is_some_and(|d| !matches!(d.hotplug, Hotplug::Disabled | Hotplug::Inboard));
        if p.trained || hotplug {
            smn.update(bridge(p.devfn_index), 0x40005, 1 << 18);
        }
    }
    smn.update(0x03101800, 1 << 3, 1 << 3);
    smn.update(0x03101800, 1 << 3, 0);
    for i in (0..8).rev() {
        smn.update(port(i, 0x080), 1 << 15, 0);
    }
    for i in (0..8).rev() {
        smn.update(port(i, 0x280), 1 << 23, 0);
        smn.update(port(i, 0x2d8), 0x10310, 0x100);
        smn.update(port(i, 0x2f0), 0x07000000, 0);
        smn.update(port(i, 0x2e0), 1 << 19, 1 << 19);
        smn.update(port(i, 0x2ec), 0x15, 0x14);
        smn.update(port(i, 0x2ec), 1 << 21, 0);
        if topology.ports[i]
            .descriptor
            .is_some_and(|d| d.hotplug == Hotplug::Enhanced)
        {
            smn.update(port(i, 0x074), 0, 1 << 3);
        }
    }
    smn.update(core_reg(0x2c0), 0, 1 << 18);
    for i in 0..7 {
        smn.update(port(i, 0x304), 6, 6);
        smn.update(0x11100080 + i as u32 * 0x1000, 0xf, 6);
        smn.update(port(i, 0x280), 1 << 23, 1 << 23);
        smn.update(port(i, 0x288), 1 << 19, 1 << 19);
        smn.update(port(i, 0x2c4), 0xc000, 0x4000);
        smn.update(port(i, 0x008), 1 << 15, 1 << 15);
    }
    smn.update(core_reg(0x040), 1 << 17, 0);
    smn.update(core_reg(0x100), 0, 1 << 6);
    smn.update(core_reg(0x100), 1 << 14, 1 << 14);
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
    smn: &mut impl Smn,
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
                let inhibit = smn.read(core_reg(0x428));
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
