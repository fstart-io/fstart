// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native systemagent.c post-memory egress, PEG, DMI and CID topology.

use super::{
    cpu::Cpu,
    dmi_registers::{Nfts, TopologyControl, TopologyLink, TopologyPortId, VcControl},
    early_sa::{self, EP_BASE},
    memory_map::ProgrammedMap,
    registers::SaPmControl,
    sa_dmi, sa_peg,
};
use crate::sunrise_point::pmc::MemoryBootMode;
use embedded_hal::delay::DelayNs;
use fstart_core::mmio::{MmioReadOnly as RO, MmioReadWrite as RW};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
};

register_bitfields![u16,
    PegCapability [ SLOT_IMPLEMENTED OFFSET(8) NUMBITS(1) [] ],
    PegDeviceControl [ PAYLOAD OFFSET(5) NUMBITS(3) [] ],
    PegLinkControl2 [ DEEMPHASIS OFFSET(6) NUMBITS(1) [] ]
];
register_bitfields![u32,
    EgressCapability [ ARBITRATION OFFSET(0) NUMBITS(3) [] ],
    PegSlotCapability [
        POWER_VALUE OFFSET(7) NUMBITS(8) [],
        POWER_SCALE OFFSET(15) NUMBITS(2) [],
        SLOT OFFSET(19) NUMBITS(13) []
    ],
    PegDeviceCapability [ PAYLOAD OFFSET(0) NUMBITS(3) [] ],
    PegDeviceCapability2 [ OBFF OFFSET(19) NUMBITS(1) [] ],
    PegVc [ TRAFFIC_CLASSES OFFSET(1) NUMBITS(7) [] ],
    PegErrorSeverity [ COMPLETION_TIMEOUT OFFSET(14) NUMBITS(1) [] ],
    PegLatency [ LOCK OFFSET(31) NUMBITS(1) [] ],
    PegConfig2 [ FIELD OFFSET(20) NUMBITS(3) [] ],
    PegConfig4 [ FIELD OFFSET(24) NUMBITS(2) [] ],
    Peg238 [ BIT29 OFFSET(29) NUMBITS(1) [] ],
    Peg1f8 [ BIT16 OFFSET(16) NUMBITS(1) [] ],
    PegCd0 [ BIT11 OFFSET(11) NUMBITS(1) [] ],
    PegAfeTimer [ VALUE OFFSET(0) NUMBITS(5) [] ],
    PegCid [ CID OFFSET(16) NUMBITS(8) [] ],
    VtdEngineControl [ LOCK OFFSET(31) NUMBITS(1) [] ],
    DeviceEnable [
        THERMAL OFFSET(7) NUMBITS(1) [],
        IMAGING OFFSET(10) NUMBITS(1) [],
        CHAP OFFSET(14) NUMBITS(1) [],
        GMM OFFSET(15) NUMBITS(1) []
    ],
    CapabilityB [
        GMM_DISABLED OFFSET(8) NUMBITS(1) [],
        IMAGING_DISABLED OFFSET(31) NUMBITS(1) []
    ],
    Pavp [
        PCM_ENABLE OFFSET(0) NUMBITS(1) [],
        ENABLE OFFSET(1) NUMBITS(1) [],
        LOCK OFFSET(2) NUMBITS(1) [],
        PCM_BASE OFFSET(20) NUMBITS(12) []
    ]
];
register_structs! {
    pub EgressRegisters {
        (0 => _reserved0),
        (0x04 => capability: RW<u32, EgressCapability::Register>),
        (0x08 => _reserved1),
        (0x14 => vc0: RW<u32, VcControl::Register>),
        (0x18 => _reserved2),
        (0x20 => vc1: RW<u32, VcControl::Register>),
        (0x24 => _reserved3),
        (0x26 => status: RO<u8>),
        (0x27 => _reserved4),
        (0x44 => port_id: RW<u32, TopologyPortId::Register>),
        (0x48 => _reserved5),
        (0x50 => links: [TopologyLink; 4]),
        (0x90 => _reserved6),
        (0x1000 => @END),
    }
}

impl EgressRegisters {
    /// # Safety
    /// Map the entire native EPBAR UC and own its registers exclusively for 'a.
    pub unsafe fn at<'a>(base: usize) -> &'a Self {
        assert!(base != 0 && base & 0xfff == 0);
        unsafe { &*(base as *const Self) }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Policy {
    pub dmi: sa_dmi::Policy,
    pub devices: sa_peg::Devices,
    /// Native encoding: 0 leaves the existing payload, 1=128, 2=256 bytes.
    pub peg_max_payload: [u8; 3],
    pub legacy_io_low_latency: bool,
    pub dmi_extended_sync: bool,
    pub dmi_iot: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Resources(sa_peg::Error),
    Map(super::memory_map::Error),
    Decode,
    Policy,
    Port(u8),
    EgressTimeout,
    Readback,
}

fn topology_link(link: &TopologyLink, cid: u32, port: u32, address: u32) {
    link.address.set(address);
    link.address_high.set(0);
    link.control.modify(
        TopologyControl::CID.val(cid)
            + TopologyControl::PORT.val(port)
            + TopologyControl::ENABLE::SET,
    );
}

/// Execute the native link recipes after actual memory initialization.
/// # Safety
/// Caller owns the same live SA, EPBAR, DMIBAR and PCI hierarchy exclusively;
/// DRAM must actually be trained, with the supplied map still programmed.
/// A failure after the first write is fatal; do not replay the sequence.
pub unsafe fn init_links(
    resources: &sa_peg::Resources<'_>,
    dmi: &sa_dmi::Dmi<'_>,
    egress: &EgressRegisters,
    map: &ProgrammedMap,
    cpu: Cpu,
    mode: MemoryBootMode,
    policy: Policy,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    if policy.peg_max_payload.iter().any(|&p| p > 2) {
        return Err(Error::Policy);
    }
    resources
        .validate(dmi, cpu, mode, policy.dmi)
        .map_err(Error::Resources)?;
    let sa = resources.sa().map_err(Error::Resources)?;
    map.validate_live(&sa).map_err(Error::Map)?;
    if sa.read32(0x40) != EP_BASE | 1 || sa.read32(0x44) != 0 {
        return Err(Error::Decode);
    }
    let mut ports = [None; 3];
    for (function, enabled) in policy.devices.peg.iter().copied().enumerate() {
        if !enabled {
            continue;
        }
        let port = resources
            .dev(0, 1, function as u8)
            .map_err(Error::Resources)?;
        if port.vendor_id() == 0xffff {
            continue;
        }
        if port.vendor_id() != 0x8086 || port.read8(0x0e) & 0x7f != 1 || port.read8(0xa0) != 0x10 {
            return Err(Error::Port(function as u8));
        }
        ports[function] = Some(port);
    }
    // systemagent_native_egress_port_init, preserving only the native TC0 bit.
    egress.vc0.set(egress.vc0.get() & 1);
    egress
        .capability
        .modify(EgressCapability::ARBITRATION.val(1));
    egress
        .vc1
        .modify(VcControl::ID.val(1) + VcControl::MAP.val(1) + VcControl::ENABLE::SET);
    let mut settled = false;
    for _ in 0..100_000 {
        if egress.status.get() & 2 == 0 {
            settled = true;
            break;
        }
        delay.delay_us(1);
    }
    if !settled {
        return Err(Error::EgressTimeout);
    }

    for (function, port) in ports.iter().enumerate() {
        let Some(port) = port else {
            continue;
        };
        unsafe {
            port.register::<u16, PegCapability::Register>(0xa2)
                .modify(PegCapability::SLOT_IMPLEMENTED::SET);
            port.register::<u32, PegSlotCapability::Register>(0xb4)
                .modify(
                    PegSlotCapability::SLOT.val(function as u32 + 1)
                        + PegSlotCapability::POWER_VALUE.val(75)
                        + PegSlotCapability::POWER_SCALE.val(0),
                );
            // Actual W1C status writes; never echo status alongside command.
            port.register::<u16, ()>(0x06).set(0xffff);
            port.register::<u16, ()>(0x1e).set(0xffff);
            port.register::<u16, ()>(0xaa).set(0xffff);
            for offset in [0x1c4, 0x1d0, 0x1f0] {
                port.register::<u32, ()>(offset).set(u32::MAX);
            }
            port.register::<u32, PegDeviceCapability2::Register>(0xc4)
                .modify(PegDeviceCapability2::OBFF::SET);
            let payload = policy.peg_max_payload[function];
            let supported = port
                .register::<u32, PegDeviceCapability::Register>(0xa4)
                .read(PegDeviceCapability::PAYLOAD);
            if payload != 0 && u32::from(payload - 1) <= supported {
                port.register::<u16, PegDeviceControl::Register>(0xa8)
                    .modify(PegDeviceControl::PAYLOAD.val(u16::from(payload - 1)));
            }
            port.register::<u32, PegVc::Register>(0x114)
                .modify(PegVc::TRAFFIC_CLASSES.val(0x7f));
            port.register::<u32, PegErrorSeverity::Register>(0x1cc)
                .modify(PegErrorSeverity::COMPLETION_TIMEOUT::SET);
            port.register::<u16, PegLinkControl2::Register>(0xd0)
                .modify(PegLinkControl2::DEEMPHASIS::SET);
            port.register::<u32, PegLatency::Register>(0x22c)
                .modify(PegLatency::LOCK::SET);
            port.register::<u32, PegConfig2::Register>(0x250)
                .modify(PegConfig2::FIELD.val(2));
            port.register::<u32, Peg238::Register>(0x238)
                .modify(Peg238::BIT29::SET);
            port.register::<u32, Peg1f8::Register>(0x1f8)
                .modify(Peg1f8::BIT16::SET);
            port.register::<u32, PegAfeTimer::Register>(0xc28)
                .modify(PegAfeTimer::VALUE.val(0x13));
            if function != 0 {
                port.register::<u32, PegConfig4::Register>(0x258)
                    .modify(PegConfig4::FIELD.val(if function == 1 { 3 } else { 2 }));
            } else {
                port.register::<u32, PegCd0::Register>(0xcd0)
                    .modify(PegCd0::BIT11::SET);
            }
            port.register::<u8, ()>(0x3d).set(port.read8(0x3d));
            port.register::<u32, ()>(0xa4).set(port.read32(0xa4));
        }
    }

    let dm = dmi.registers;
    dm.uncorrectable_status.set(u32::MAX);
    dm.correctable_status.set(u32::MAX);
    dm.nfts.modify(Nfts::L0S_LOCK::SET);
    // The source writes only the low byte of LCTL, not adjacent status.
    let link = unsafe {
        fstart_core::mmio::MmioRegister::<u8>::new(core::ptr::addr_of!(dm.link_control) as usize)
    };
    let mut control = link.get() & !3;
    if !policy.legacy_io_low_latency {
        control |= 2;
    }
    link.set(control);
    if policy.dmi_extended_sync {
        link.set(link.get() | 0x10);
    }
    if policy.dmi_iot {
        dm.iot.set(0x44);
    }

    egress.port_id.modify(TopologyPortId::CID.val(1));
    topology_link(&egress.links[0], 1, 0, early_sa::DMI_BASE);
    for (function, port) in ports.iter().enumerate() {
        let Some(port) = port else {
            continue;
        };
        topology_link(
            &egress.links[function + 1],
            1,
            0,
            0x8000 + function as u32 * 0x1000,
        );
        unsafe {
            port.register::<u32, PegCid::Register>(0x144)
                .modify(PegCid::CID.val(1));
            port.register::<u32, ()>(0x158).set(EP_BASE);
            port.register::<u32, ()>(0x15c).set(0);
            // Native preserves the high target-port byte for these PEG links.
            let link = port.register::<u32, TopologyControl::Register>(0x150);
            link.set((link.get() & 0xff00ffff) | (1 << 16) | 1);
            port.register::<u32, ()>(0xc4).set(port.read32(0xc4));
        }
    }
    dm.topology_id.modify(TopologyPortId::CID.val(1));
    dm.topology_link[0].control.modify(
        TopologyControl::CID.val(2) + TopologyControl::PORT.val(0) + TopologyControl::ENABLE::SET,
    );
    dm.topology_link[0].address_high.set(0);
    topology_link(&dm.topology_link[1], 1, 0, EP_BASE);
    dm.capability.set(dm.capability.get());
    dm.link_cap.set(dm.link_cap.get());
    Ok(())
}

register_structs! {
    pub VtdRegisters {
        (0 => _reserved0),
        (0x100 => queue_address: RW<u32>),
        (0x104 => _reserved1),
        (0xf04 => configuration: RW<u32>),
        (0xf08 => _reserved2),
        (0xff0 => control: RW<u32, VtdEngineControl::Register>),
        (0xff4 => tracking: RW<u32>),
        (0xff8 => _reserved3),
        (0x1000 => @END),
    }
}
impl VtdRegisters {
    /// # Safety
    /// The whole page must map the selected native VT-d engine UC, exclusively.
    pub unsafe fn at<'a>(base: usize) -> &'a Self {
        assert!(base != 0 && base & 0xfff == 0);
        unsafe { &*(base as *const Self) }
    }

    fn initialize(&self, graphics: bool) -> Result<(), Error> {
        if self.control.is_set(VtdEngineControl::LOCK) {
            return Ok(());
        }
        let configuration = self.configuration.get();
        let tracking = self.tracking.get();
        let control = self.control.get() | VtdEngineControl::LOCK::SET.value;
        if graphics {
            self.queue_address.set(0x50a);
        }
        self.configuration.set(configuration);
        self.tracking.set(tracking);
        self.control.set(control);
        if !self.control.is_set(VtdEngineControl::LOCK) {
            return Err(Error::Readback);
        }
        Ok(())
    }
}

/// Native VT-d engine configuration, not address translation or DMA protection.
/// # Safety
/// Both engine pages must correspond to their actual fixed hardware addresses;
/// the caller owns their UC mappings and all SA/P2SB configuration exclusively.
pub unsafe fn init_vtd(
    resources: &sa_peg::Resources<'_>,
    ecam: early_sa::EcamConfig,
    mode: MemoryBootMode,
    graphics: &VtdRegisters,
    other: &VtdRegisters,
) -> Result<early_sa::VtdDecodeStatus, Error> {
    let sa = resources.sa().map_err(Error::Resources)?;
    let mch = resources.mch;
    let p2sb = resources.dev(0, 0x1f, 1).map_err(Error::Resources)?;
    let igd = resources.dev(0, 2, 0).map_err(Error::Resources)?;
    let present = igd.vendor_id() != 0xffff;
    if present && igd.vendor_id() != 0x8086 {
        return Err(Error::Decode);
    }
    let result = early_sa::prepare_vtd_decodes_cold(&sa, mch, &p2sb, ecam, mode, present)
        .map_err(|_| Error::Decode)?;
    if result == early_sa::VtdDecodeStatus::FusedOff {
        mch.gfx_vtd_bar.set(0);
        mch.gfx_vtd_bar_high.set(0);
        mch.vtvc0_bar.set(0);
        mch.vtvc0_bar_high.set(0);
    } else {
        if present {
            graphics.initialize(true)?;
        } else {
            mch.gfx_vtd_bar.set(0);
            mch.gfx_vtd_bar_high.set(0);
        }
        other.initialize(false)?;
    }
    if mch.gfx_vtd_bar_high.get() != 0
        || mch.vtvc0_bar_high.get() != 0
        || (!present || result == early_sa::VtdDecodeStatus::FusedOff) && mch.gfx_vtd_bar.get() != 0
        || result == early_sa::VtdDecodeStatus::FusedOff && mch.vtvc0_bar.get() != 0
    {
        return Err(Error::Readback);
    }
    Ok(result)
}

/// Native irreversible SA security locks. Compatible SMRAM and VT-d engine
/// locks do not substitute for CPU SMRR, SMM relocation or DMA translation.
pub fn lock(
    resources: &sa_peg::Resources<'_>,
    dmi: &sa_dmi::Dmi<'_>,
    map: &ProgrammedMap,
    thermal_device_enabled: bool,
    pch_h: bool,
) -> Result<(), Error> {
    let sa = resources.sa().map_err(Error::Resources)?;
    map.validate_live(&sa).map_err(Error::Map)?;
    if (sa.read32(0x54) & 0x80 != 0) != thermal_device_enabled {
        return Err(Error::Policy);
    }
    let mch = resources.mch;
    if !pch_h {
        for register in [&mch.edram_pm0, &mch.edram_pm1, &mch.edram_pm2] {
            register.set(register.get() & !1);
        }
    }
    for (offset, bits) in [
        (0x50, 1),
        (0x5c, 1),
        (0x78, 1 << 10),
        (0x90, 1),
        (0x98, 1),
        (0xa0, 1),
        (0xa8, 1),
        (0xb0, 1),
        (0xb4, 1),
        (0xb8, 1),
        (0xbc, 1),
    ] {
        let register = unsafe { sa.register::<u32, ()>(offset) };
        register.set(register.get() | bits);
        if register.get() & bits != bits {
            return Err(Error::Readback);
        }
    }
    for (register, bits) in [
        (&mch.pavp_message, 1),
        (
            &mch.mc_lock,
            if thermal_device_enabled { 0x87 } else { 0x8f },
        ),
        (&mch.bios_reset_lock, 1 << 31),
        (&mch.ipu_vtd_lock, 1),
        (&mch.itt_vtd_lock, 1),
        (&mch.edram_lock, 1 << 31),
        (&mch.remap_base_lock, 1),
        (&mch.vtd_track_lock, 1),
    ] {
        register.set(register.get() | bits);
        if register.get() & bits != bits {
            return Err(Error::Readback);
        }
    }
    mch.remap_base_readback.set(mch.remap_base_readback.get());
    mch.remap_limit_readback.set(mch.remap_limit_readback.get());
    mch.ddr_ptm.set(mch.ddr_ptm.get() | (1 << 5));
    dmi.registers
        .pcie_lock2
        .set(dmi.registers.pcie_lock2.get() | 1);
    if mch.ddr_ptm.get() & (1 << 5) == 0 || dmi.registers.pcie_lock2.get() & 1 == 0 {
        return Err(Error::Readback);
    }
    // PM synchronization was established before BIOS_RESET_CPL. Lockdown
    // verifies that producer's result rather than programming it a second time.
    if mch.sa_pm_control.get() == u32::MAX
        || !mch.sa_pm_control.matches_all(SaPmControl::PM_SYNC.val(3))
    {
        return Err(Error::Readback);
    }
    map.validate_live(&sa).map_err(Error::Map)?;
    Ok(())
}

/// Native pre-enumeration SA function policy. Returns the resulting DEVEN word.
pub fn configure_devices(
    resources: &sa_peg::Resources<'_>,
    devices: sa_peg::Devices,
    cio2_enabled: bool,
) -> Result<u32, Error> {
    let sa = resources.sa().map_err(Error::Resources)?;
    early_sa::validate_host(&sa).map_err(|_| Error::Decode)?;
    let caps = unsafe { sa.register::<u32, CapabilityB::Register>(0xe8) };
    let imaging = devices.imaging && cio2_enabled && !caps.is_set(CapabilityB::IMAGING_DISABLED);
    let gmm = devices.gmm && !caps.is_set(CapabilityB::GMM_DISABLED);
    let deven = unsafe { sa.register::<u32, DeviceEnable::Register>(0x54) };
    deven.modify(
        DeviceEnable::THERMAL.val(u32::from(devices.thermal))
            + DeviceEnable::IMAGING.val(u32::from(imaging))
            + DeviceEnable::CHAP.val(u32::from(devices.chap))
            + DeviceEnable::GMM.val(u32::from(gmm)),
    );
    if imaging {
        let imgu = resources.dev(0, 5, 0).map_err(Error::Resources)?;
        if imgu.vendor_id() != 0xffff {
            if imgu.vendor_id() != 0x8086 {
                return Err(Error::Decode);
            }
            unsafe {
                imgu.register::<u16, ()>(0x0a).set(0x0480);
                imgu.register::<u8, ()>(0x09).set(0);
                let mode = imgu.register::<u8, ()>(0x40);
                mode.set(mode.get() & !1);
            }
        }
    }
    let expected = (u32::from(devices.thermal) << 7)
        | (u32::from(imaging) << 10)
        | (u32::from(devices.chap) << 14)
        | (u32::from(gmm) << 15);
    if deven.get() & 0xc480 != expected {
        return Err(Error::Readback);
    }
    Ok(deven.get())
}

/// Native PAVP policy and its irreversible lock. PCM backing must be within
/// actual stolen graphics memory, not merely below an address bound.
pub fn configure_pavp(
    resources: &sa_peg::Resources<'_>,
    map: &ProgrammedMap,
    enabled: bool,
) -> Result<(), Error> {
    let sa = resources.sa().map_err(Error::Resources)?;
    map.validate_live(&sa).map_err(Error::Map)?;
    let pavp = unsafe { sa.register::<u32, Pavp::Register>(0x58) };
    let old = pavp.get();
    let mut desired = old;
    if enabled {
        if resources
            .dev(0, 2, 0)
            .map_err(Error::Resources)?
            .vendor_id()
            != 0x8086
        {
            return Err(Error::Policy);
        }
        let base = map.plan().tolud.checked_sub(1 << 20).ok_or(Error::Policy)?;
        if base < map.plan().bdsm {
            return Err(Error::Policy);
        }
        desired = (old & !0xfff0018b) | (base as u32) | 0x43;
    }
    desired |= Pavp::LOCK::SET.value;
    if pavp.is_set(Pavp::LOCK) && old != desired {
        return Err(Error::Policy);
    }
    pavp.set(desired);
    if pavp.get() != desired {
        return Err(Error::Readback);
    }
    Ok(())
}
