// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native src/soc/intel/skylake/sata.c: cold PCH-H AHCI without RST remapping.
//! Source SHA256: 5bb1cdea8d9d29f5b8e2201b646012d5f70abb5ff93124efaf38d81e22ea76a7.
//! No LP, RAID, retained resume or electrical test-mode path is advertised.
//! Ordinary SATA lanes are observed through the existing HSIO/FIA transport;
//! no lane-owner overrides, DMA rings, disk writes or fabricated link success.
mod registers;
mod temporary;
#[cfg(test)]
mod tests;
use super::{
    hsio,
    pcr::{self, Pcr, Port},
    pmc::{self, PmcRegisters},
};
use crate::southbridge::ahci::{AhciRegs, CAP, CAP2, GHC, PXCMD, PXDEVSLP};
use embedded_hal::delay::DelayNs;
use fstart_core::{indexed::Indexed, mmio::MmioRegister};
use fstart_pci::{EcamDevice, PCI_COMMAND_BITS, PciWindow, PciWindowKind};
use registers::*;
use tock_registers::{
    RegisterLongName,
    interfaces::{ReadWriteable, Readable, Writeable},
};

/// Native PCH preserved window starts at FC800000; SATA borrows the next MiB.
/// The NB PCI pool ends BEFORE FC800000. This is reserved UC MMIO, never RAM.
pub const TEMP_BASE: usize = 0xfc90_0000;
// SATAGC.ASSEL=0 selects a 2 KiB BAR5. MSS describes BAR0, not BAR5.
const ABAR_BYTES: u64 = 0x800;
const TEMP_BYTES: usize = 0x2000;
const _: () = assert!(core::mem::size_of::<AhciRegs<8>>() as u64 <= ABAR_BYTES);
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SpeedLimit {
    Gen1 = 1,
    Gen2 = 2,
    Gen3 = 3,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortPolicy {
    pub enabled: bool,
    pub hotplug: bool,
    pub spin_up: bool,
    pub devslp: bool,
}
impl PortPolicy {
    pub const DISABLED: Self = Self {
        enabled: false,
        hotplug: false,
        spin_up: false,
        devslp: false,
    };
    pub const ENABLED: Self = Self {
        enabled: true,
        ..Self::DISABLED
    };
}
#[derive(Debug, Clone, Copy)]
pub struct AhciPolicy {
    pub ports: [PortPolicy; 8],
    pub speed: SpeedLimit,
    pub salp: bool,
}
impl AhciPolicy {
    fn maps(self) -> Result<(u8, u8, bool), Error> {
        let mut enabled = 0u8;
        let mut hotplug = 0u8;
        let mut spin = false;
        for (port, policy) in self.ports.into_iter().enumerate() {
            if !policy.enabled && (policy.hotplug || policy.spin_up || policy.devslp) {
                return Err(Error::Policy);
            }
            if policy.enabled {
                enabled |= 1 << port;
            }
            if policy.hotplug {
                hotplug |= 1 << port;
            }
            spin |= policy.spin_up;
        }
        Ok((enabled, hotplug, spin))
    }
    fn final_ports(self, present: u8) -> Result<u8, Error> {
        let (enabled, hotplug, _) = self.maps()?;
        Ok(enabled & (hotplug | present))
    }
}
#[derive(Debug, Clone, Copy)]
pub enum Policy {
    Disabled,
    Ahci(AhciPolicy),
}
impl Policy {
    pub const fn enabled(self) -> bool {
        matches!(self, Self::Ahci(_))
    }
    pub(crate) fn validate(self) -> Result<(), Error> {
        match self {
            Self::Disabled => Ok(()),
            Self::Ahci(policy) => policy.maps().map(|_| ()),
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    Resource,
    Locked,
    Busy,
    Readback,
    RemappingActive,
    Pcr(pcr::Error),
    Hsio(hsio::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<hsio::Error> for Error {
    fn from(e: hsio::Error) -> Self {
        Self::Hsio(e)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisabledReason {
    Policy,
    NoSataLane,
    Absent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Disabled(DisabledReason),
    /// Hardware presence sample and actual final configuration, not a claim
    /// of negotiated speed, device readiness, media health or working DMA.
    Ahci {
        configured: u8,
        present: u8,
        final_ports: u8,
        cycle_capabilities: u8,
    },
}
/// Produced only after real early programming/COMRESET and BAR cleanup. Move
/// once into finish after the canonical enumeration, never copy/replay it.
pub(crate) enum Early {
    Disabled(DisabledReason),
    Ahci(Prepared),
}
pub(crate) struct Prepared {
    kind: super::dmi::Kind,
    policy: AhciPolicy,
    device_id: u16,
    cycle_capabilities: u8,
}

fn require(device: &EcamDevice) -> Result<(), Error> {
    if device.address().segment() != 0
        || device.bus() != 0
        || device.dev() != 0x17
        || device.func() != 0
        || device.vendor_id() != 0x8086
        || device.read8(0x0b) != 1
        || !matches!(device.read8(0x0a), 4 | 6)
    {
        return Err(Error::Device);
    }
    Ok(())
}
fn require_unlocked(device: &EcamDevice) -> Result<(), Error> {
    if unsafe { device.register::<u8, RegisterLock::Register>(0x9f) }.is_set(RegisterLock::LOCK) {
        return Err(Error::Locked);
    }
    Ok(())
}
struct Controller {
    device: EcamDevice,
    kind: super::dmi::Kind,
}
impl Controller {
    // This binds the existing generic index/data transport to SATA SIR; no
    // independent PCI/MMIO access suite or opaque policy callback is involved.
    fn sir<R: RegisterLongName>(
        &self,
        index: u8,
    ) -> Indexed<MmioRegister<u8>, MmioRegister<u32>, R> {
        Indexed::new(
            unsafe { self.device.register(0xa0) },
            unsafe { self.device.register(0xa4) },
            index,
        )
    }
    fn cycle<R: RegisterLongName>(
        &self,
        index: u8,
        offset: u16,
    ) -> Indexed<MmioRegister<u8>, MmioRegister<u32>, R> {
        Indexed::new(
            unsafe { self.device.register(0xfc0) },
            unsafe { self.device.register(offset) },
            index,
        )
    }
    fn preflight_cycles(&self) -> Result<(), Error> {
        for router in 0..3 {
            let control = self.cycle::<Cycle::Register>(router, 0x300);
            if control.is_set(Cycle::ENABLE) {
                return Err(Error::RemappingActive);
            }
            if control.is_set(Cycle::ENABLE_LOCK)
                || control.is_set(Cycle::CONFIG_LOCK)
                || control.is_set(Cycle::PNCAIDL)
            {
                return Err(Error::Locked);
            }
        }
        Ok(())
    }
    fn discover_cycles(&self) -> Result<u8, Error> {
        let selector = unsafe { self.device.register::<u32, CycleSelect::Register>(0xfc0) };
        let mut capabilities = 0u8;
        for router in 0..3u8 {
            let status = self.cycle::<CycleStatus::Register>(router, 0x304);
            status.modify(CycleStatus::PCCD::SET + CycleStatus::PCCS::CLEAR);
            let control = Indexed::<_, _, CycleLow::Register>::new(
                unsafe { self.device.register::<u8, ()>(0xfc0) },
                unsafe { self.device.register::<u8, ()>(0x300) },
                router,
            );
            control.modify(CycleLow::ENABLE::SET);
            let supported = u8::from(control.is_set(CycleLow::ENABLE));
            capabilities |= supported << router;
            selector
                .modify(CycleSelect::CAPABILITIES.val(
                    selector.read(CycleSelect::CAPABILITIES) | u32::from(supported << router),
                ));
            control.modify(CycleLow::ENABLE::CLEAR);
            status.modify(CycleStatus::PCCD::CLEAR + CycleStatus::PCCS::CLEAR);
            if control.is_set(CycleLow::ENABLE) {
                return Err(Error::RemappingActive);
            }
        }
        Ok(capabilities)
    }
    fn cycle_power(&self, enabled: bool) {
        for router in 0..3 {
            self.cycle::<CyclePower::Register>(router, 0x354).modify(
                CyclePower::CLOCK_DATA::SET
                    + CyclePower::CLOCK_TRUNK::SET
                    + CyclePower::ICAS::SET
                    + CyclePower::TSCAS::SET,
            );
            if enabled {
                self.cycle::<Cycle::Register>(router, 0x300)
                    .modify(Cycle::CLOCK_GATE::SET);
            }
        }
    }
    fn lock_cycles(&self) -> Result<(), Error> {
        for router in 0..3 {
            let control = self.cycle::<Cycle::Register>(router, 0x300);
            if control.is_set(Cycle::ENABLE) {
                return Err(Error::RemappingActive);
            }
            control.modify(Cycle::ENABLE_LOCK::SET + Cycle::CONFIG_LOCK::SET + Cycle::PNCAIDL::SET);
            if !control.matches_all(
                Cycle::ENABLE::CLEAR
                    + Cycle::ENABLE_LOCK::SET
                    + Cycle::CONFIG_LOCK::SET
                    + Cycle::PNCAIDL::SET,
            ) {
                return Err(Error::Readback);
            }
        }
        // No remapped endpoints: native last_enabled_cr remains zero.
        unsafe { self.device.register::<u8, ()>(0xfc0) }.set(0);
        Ok(())
    }
    /// Trusted opaque native SIR clock/event recipe words. Typed Indexed is
    /// the transport; these are not analog calibration or measured margins.
    fn recipe(&self, steps: &[(u8, u32, u32)]) {
        for &(index, clear, set) in steps {
            let register = self.sir::<()>(index);
            register.set((register.get() & !clear) | set);
        }
    }
    fn early_pci(&self, lpc: &EcamDevice, delay: &mut impl DelayNs) -> Result<(), Error> {
        match self.kind {
            super::dmi::Kind::H => {
                self.recipe(&[(0x98, 0x1ff, 0x183), (0xa4, 0, 1 << 6)]);
                if !matches!(
                    lpc.device_id(),
                    0xa149
                        | 0xa14a
                        | 0xa150
                        | 0xa14b
                        | 0xa2d2
                        | 0xa2d3
                        | 0xa2ce
                        | 0xa2cf
                        | 0xa2d4
                        | 0xa2d0
                        | 0xa2d1
                ) {
                    self.recipe(&[(0x9c, 0, 1 << 22)]);
                }
                self.recipe(&[(0x9c, 0x1fec, 0x209c_0224)]);
                let decode = unsafe { self.device.register::<u8, Decode::Register>(0x9c) };
                // Only the low write-once byte. Do NOT consume the REGLOCK byte.
                decode.modify(Decode::ASSEL.val(0) + Decode::MSS.val(2) + Decode::INIT::SET);
                if !decode
                    .matches_all(Decode::ASSEL.val(0) + Decode::MSS.val(2) + Decode::INIT::SET)
                {
                    return Err(Error::Readback);
                }
                let pcs = unsafe { self.device.register::<u16, PortEnable::Register>(0x94) };
                pcs.modify(PortEnable::PORTS.val(255));
                delay.delay_us(2);
                // SMS is byte 2; a dword write would prematurely write REGLOCK=0.
                let mode = unsafe { self.device.register::<u8, Mode::Register>(0x9e) };
                mode.modify(Mode::RAID::CLEAR);
                if mode.is_set(Mode::RAID) {
                    return Err(Error::Readback);
                }
            }
            super::dmi::Kind::Lp => {
                unsafe { self.device.register::<u32, LpClockGate::Register>(0x94) }
                    .modify(LpClockGate::LOW.val(0x183));
                let pcs = unsafe { self.device.register::<u16, LpPcs::Register>(0x92) };
                pcs.modify(LpPcs::OOB_RETRY::SET);
                let r98 = unsafe { self.device.register::<u32, ()>(0x98) };
                r98.set((r98.get() & !0x1fec) | 0x20dc_0224);
                let decode = unsafe { self.device.register::<u8, Decode::Register>(0x9c) };
                decode.modify(Decode::ASSEL.val(0) + Decode::MSS.val(2) + Decode::INIT::SET);
                if !decode
                    .matches_all(Decode::ASSEL.val(0) + Decode::MSS.val(2) + Decode::INIT::SET)
                {
                    return Err(Error::Readback);
                }
                pcs.modify(LpPcs::PORTS.val(0x7));
                delay.delay_us(2);
                let mode = unsafe { self.device.register::<u8, LpMode::Register>(0x90) };
                mode.modify(LpMode::RAID::CLEAR);
                if mode.is_set(LpMode::RAID) {
                    return Err(Error::Readback);
                }
            }
        }
        Ok(())
    }
    fn present(&self) -> u8 {
        match self.kind {
            super::dmi::Kind::H => {
                unsafe { self.device.register::<u32, Pcs::Register>(0x94) }.read(Pcs::PRESENT) as u8
            }
            super::dmi::Kind::Lp => unsafe { self.device.register::<u16, LpPcs::Register>(0x92) }
                .read(LpPcs::PRESENT) as u8,
        }
    }
    fn power_optimization(&self, hotplug: u8) {
        match self.kind {
            super::dmi::Kind::H => self.recipe(&[
                (0x90, 0, u32::from(!hotplug)),
                (0x80, 0, u32::from(hotplug) << 16),
                (0x8c, 0, 0x00ff00ff),
                (
                    0xa0,
                    (1 << 23) | (1 << 21) | (1 << 18),
                    (1 << 22) | (1 << 20) | (1 << 19) | (1 << 15),
                ),
                (0x84, 0, 0x00ff00ff),
                (0xa4, 0, 1 << 14),
                (0xcc, u32::MAX, 0x883c9001),
                (0xd0, 0xffff, 0x880a),
                (0xc8, 0, 1 << 3),
                (0xc8, 0, 1),
                (0xc8, 0, 1 << 1),
            ]),
            super::dmi::Kind::Lp => self.recipe(&[
                (0x50, 0, (0x7 << 16) & !(u32::from(hotplug & 0x7) << 16)),
                (
                    0x70,
                    (1 << 23) | (1 << 21) | (1 << 18),
                    0x0700_8700 | (1 << 22) | (1 << 20) | (1 << 19),
                ),
                (0x54, 0, 0x4700_0700 | u32::from(hotplug & 0x7)),
                (0x64, u32::MAX, 0x883c_9001),
                (0x68, 0xffff, 0x880a),
                (0x60, 0, (1 << 3) | (1 << 1) | 1),
            ]),
        }
    }
    fn late_sir(&self) {
        match self.kind {
            super::dmi::Kind::H => self.recipe(&[
                (0xa8, 0, (1 << 18) | (1 << 17) | (1 << 16)),
                (0xd4, 0x1321_0e17, 0x2c1e_1108),
            ]),
            super::dmi::Kind::Lp => self.recipe(&[
                (0x5c, 0, (1 << 18) | (1 << 17) | (1 << 16)),
                (0x6c, 0x1321_0e17, 0x2c1e_1108),
            ]),
        }
    }
    fn thermal(&self) {
        // Literal source SATA-device throttling recipe, NOT a mainboard fan
        // policy. Re-read actual presence; hot-plug alone is not a device.
        let present = self.present();
        let (pairs, base, stride) = match self.kind {
            super::dmi::Kind::H => (4u8, 0x0cu8, 0x10u8),
            super::dmi::Kind::Lp => (2u8, 0xa0u8, 0x04u8),
        };
        for pair in 0..pairs {
            let low = if present & (1 << (pair * 2)) != 0 {
                0x0039
            } else {
                0
            };
            let high = if present & (1 << (pair * 2 + 1)) != 0 {
                0x0039_0000
            } else {
                0
            };
            self.sir::<()>(base + stride * pair).set(low | high);
        }
    }
    fn disable(&self, hba: &AhciRegs<8>) -> Result<(), Error> {
        match self.kind {
            super::dmi::Kind::H => {
                unsafe { self.device.register::<u16, PortEnable::Register>(0x94) }
                    .modify(PortEnable::PORTS.val(0));
            }
            super::dmi::Kind::Lp => {
                unsafe { self.device.register::<u16, LpPcs::Register>(0x92) }
                    .modify(LpPcs::PORTS.val(0));
            }
        }
        hba.pi.set(0);
        let _ = hba.pi.get();
        let _ = hba.pi.get();
        if hba.pi.get() != 0 {
            return Err(Error::Readback);
        }
        unsafe { self.device.register::<u16, PCI_COMMAND_BITS::Register>(4) }
            .modify(PCI_COMMAND_BITS::MEMORY_SPACE::CLEAR + PCI_COMMAND_BITS::IO_SPACE::CLEAR);
        match self.kind {
            super::dmi::Kind::H => {
                unsafe { self.device.register::<u32, Map::Register>(0x90) }
                    .modify(Map::DISABLE.val(255));
                self.recipe(&[
                    (0x9c, 0, (1 << 29) | (1 << 23)),
                    (0x8c, 0, 0x00ff00ff),
                    (0xa0, 0, 1 << 15),
                    (0x84, 0, 0x00ff0000),
                    (0xa4, (7 << 24) | (1 << 14) | (1 << 5), (1 << 14) | (1 << 5)),
                    (0xa8, 0, (1 << 17) | (1 << 16)),
                ]);
            }
            super::dmi::Kind::Lp => {
                unsafe { self.device.register::<u32, LpClockGate::Register>(0x94) }
                    .modify(LpClockGate::DISABLE.val(0x7));
                let r98 = unsafe { self.device.register::<u32, ()>(0x98) };
                r98.set(r98.get() | (1 << 29) | (1 << 23));
                self.recipe(&[
                    (0x70, 0, 0x0700_8700),
                    (
                        0x54,
                        0,
                        (1 << 30) | (1 << 26) | (1 << 25) | (1 << 24) | (1 << 21),
                    ),
                    (0x58, (1 << 26) | (1 << 25) | (1 << 24), 0),
                    (0x5c, 0, (1 << 17) | (1 << 16)),
                ]);
            }
        }
        self.cycle_power(false);
        Ok(())
    }
}
fn quiescent(hba: &AhciRegs<8>) -> Result<(), Error> {
    if hba.ghc.is_set(GHC::HR) {
        return Err(Error::Busy);
    }
    hba.ghc.modify(GHC::AE::SET);
    if !hba.ghc.is_set(GHC::AE) {
        return Err(Error::Readback);
    }
    let count = hba.cap.read(CAP::NP) + 1;
    let implemented = hba.pi.get();
    if count > 8 || implemented & !0xff != 0 || implemented.count_ones() > count {
        return Err(Error::Device);
    }
    // PI can be sparse. Until it is programmed, also check the reset
    // port windows described by CAP.NP, as on the controller disable path.
    let ports = if implemented == 0 {
        (1 << count) - 1
    } else {
        implemented
    };
    for (index, port) in hba.ports.iter().enumerate() {
        if ports & (1 << index) == 0 {
            continue;
        }
        if port.cmd.is_set(PXCMD::ST)
            || port.cmd.is_set(PXCMD::FRE)
            || port.cmd.is_set(PXCMD::FR)
            || port.cmd.is_set(PXCMD::CR)
            || port.ci.get() != 0
            || port.sact.get() != 0
        {
            return Err(Error::Busy);
        }
    }
    Ok(())
}
fn any_lane(silicon: hsio::Silicon, pcr: &Pcr) -> Result<bool, Error> {
    let candidates: &[[Option<u8>; 2]] = match silicon.kind() {
        super::dmi::Kind::H => &[
            [Some(18), Some(22)],
            [Some(19), Some(23)],
            [Some(24), None],
            [Some(25), None],
            [Some(26), None],
            [Some(27), None],
            [Some(28), None],
            [Some(29), None],
        ],
        super::dmi::Kind::Lp => &[[Some(10), None], [Some(11), Some(14)], [Some(15), None]],
    };
    for &pair in candidates {
        for lane in pair.into_iter().flatten() {
            if silicon.lane_owner(pcr, lane)? == 2 {
                return Ok(true);
            }
        }
    }
    Ok(false)
}
/// Initial CAP/PI programming precedes COMRESET; presence is NOT sampled here.
fn early_ahci(
    controller: &Controller,
    hba: &AhciRegs<8>,
    policy: AhciPolicy,
    base: usize,
) -> Result<AhciPolicy, Error> {
    let (requested, _, _) = policy.maps()?;
    // Like the native PI write, allow SKU-reserved port bits to read zero.
    // CAP.NP counts ports; it does not identify their physical indices.
    let before = hba.pi.get();
    hba.pi.set(before | u32::from(requested));
    let _ = hba.pi.get();
    let implemented = hba.pi.get();
    let configured = requested & implemented as u8;
    if implemented & before != before || configured == 0 {
        return Err(Error::Readback);
    }
    quiescent(hba)?;
    let mut policy = policy;
    for (index, port) in policy.ports.iter_mut().enumerate() {
        if configured & (1 << index) == 0 {
            *port = PortPolicy::DISABLED;
        }
    }
    if configured != requested {
        fstart_log::debug!(
            "SATA: board port mask {:#x}, controller accepted {:#x}",
            requested,
            configured
        );
    }
    let (_, _, spin) = policy.maps()?;
    hba.cap.modify(
        CAP::SIS::CLEAR
            + CAP::SSS.val(u32::from(spin))
            + CAP::SALP.val(u32::from(policy.salp))
            + CAP::PMS::CLEAR
            + CAP::SSC::SET
            + CAP::PSC::SET
            + CAP::SXS::CLEAR
            + CAP::ISS.val(policy.speed as u32)
            + CAP::SCLO::SET
            + CAP::PMD::SET,
    );
    hba.cap2
        .modify(CAP2::APST::SET + CAP2::SDS::SET + CAP2::SADM::SET + CAP2::DESO::SET);
    match controller.kind {
        super::dmi::Kind::H => {
            let pcs = unsafe {
                controller
                    .device
                    .register::<u16, PortEnable::Register>(0x94)
            };
            pcs.modify(PortEnable::PORTS.val(0));
            pcs.modify(PortEnable::PORTS.val(255));
            if pcs.read(PortEnable::PORTS) & u16::from(configured) != u16::from(configured) {
                return Err(Error::Readback);
            }
        }
        super::dmi::Kind::Lp => {
            let pcs = unsafe { controller.device.register::<u16, LpPcs::Register>(0x92) };
            pcs.modify(LpPcs::PORTS.val(0));
            pcs.modify(LpPcs::PORTS.val(0x7));
            if pcs.read(LpPcs::PORTS) & u16::from(configured) != u16::from(configured) {
                return Err(Error::Readback);
            }
        }
    }
    // RSTF is a literal 16-bit write-once access, even in AHCI mode.
    unsafe { MmioRegister::<u16, RstFeatures::Register>::new(base + 0xc8) }
        .write(RstFeatures::HDD_UNLOCK::SET + RstFeatures::LED_LOCATE::SET);
    Ok(policy)
}
/// # Safety
/// Actual PCH-H/LP engines are exclusively owned in the authenticated cold
/// successor, before enumeration. Own/reserve/map the complete TEMP_BASE 8K
/// aperture as UC, disjoint from DRAM and other decodes. No DMA/SMI/OS/AP
/// competitor exists. Partial errors are fatal; no repeated early attempt.
pub(crate) unsafe fn prepare(
    pcr: &Pcr,
    pmc: &PmcRegisters,
    policy: Policy,
    delay: &mut impl DelayNs,
) -> Result<Early, Error> {
    policy.validate()?;
    let lpc = EcamDevice::new(0, 0x1f, 0);
    let silicon = hsio::Silicon::read(&lpc)?;
    let kind = silicon.kind();
    if let Policy::Ahci(ahci) = policy
        && kind == super::dmi::Kind::Lp
        && ahci.ports[3..].iter().any(|p| p.enabled)
    {
        return Err(Error::Policy);
    }
    if pmc.function_disable.is_set(pmc::FunctionDisable::LOCK) {
        return Err(Error::Locked);
    }
    let device = EcamDevice::new(0, 0x17, 0);
    if !device.is_present() {
        return if policy.enabled() {
            Err(Error::Device)
        } else {
            Ok(Early::Disabled(DisabledReason::Absent))
        };
    }
    require(&device)?;
    if pmc.non_static_disable.is_set(pmc::NonStaticDisable::SATA) {
        return Err(Error::Locked);
    }
    temporary::preflight(&device)?;
    let controller = Controller { device, kind };
    controller.preflight_cycles()?;
    let disable = match policy {
        Policy::Disabled => Some(DisabledReason::Policy),
        Policy::Ahci(_) => {
            if any_lane(silicon, pcr)? {
                None
            } else {
                Some(DisabledReason::NoSataLane)
            }
        }
    };
    let capabilities = controller.discover_cycles()?;
    let lease = unsafe { temporary::Temporary::bind(device, 0x24, TEMP_BASE, TEMP_BYTES) }?;
    let result = (|| {
        let hba = lease.registers();
        if let Some(reason) = disable {
            quiescent(hba)?;
            controller.disable(hba)?;
            return Ok(Early::Disabled(reason));
        }
        let Policy::Ahci(policy) = policy else {
            return Err(Error::Policy);
        };
        controller.early_pci(&lpc, delay)?;
        quiescent(hba)?;
        let policy = early_ahci(&controller, hba, policy, TEMP_BASE)?;
        Ok(Early::Ahci(Prepared {
            kind,
            policy,
            device_id: device.device_id(),
            cycle_capabilities: capabilities,
        }))
    })();
    // Explicit cleanup readback is required even when actual setup failed.
    lease.close()?;
    let result = result?;
    if matches!(result, Early::Disabled(_)) {
        // Close/read back BAR/command BEFORE SCFD/FUNDIS can hide config.
        unsafe { device.register::<u32, General::Register>(0x9c) }.modify(General::SCFD::SET);
        let shadow_offset = match kind {
            super::dmi::Kind::H => 0xc1c,
            super::dmi::Kind::Lp => 0x101c,
        };
        let shadow = pcr.register::<Shadow::Register>(Port::Psf1, shadow_offset)?;
        shadow.modify(Shadow::DISABLE::SET);
        pmc.non_static_disable
            .modify(pmc::NonStaticDisable::SATA::SET);
        if !shadow.is_set(Shadow::DISABLE)
            || !pmc.non_static_disable.is_set(pmc::NonStaticDisable::SATA)
        {
            return Err(Error::Readback);
        }
    }
    Ok(result)
}
/// # Safety
/// Consume this owner's real early producer only AFTER canonical PCI resource
/// assignment, so drives had the enumeration interval to answer COMRESET.
/// The entire actual ABAR must be mapped UC and still exclusively owned.
/// No all-ones resource probe, HBA reset, ring allocation or DMA is performed.
pub(crate) unsafe fn finish(early: Early, window: Option<PciWindow>) -> Result<Outcome, Error> {
    let prepared = match early {
        Early::Ahci(prepared) => prepared,
        Early::Disabled(reason) => return Ok(Outcome::Disabled(reason)),
    };
    let device = EcamDevice::new(0, 0x17, 0);
    require(&device)?;
    require_unlocked(&device)?;
    if device.device_id() != prepared.device_id || device.read8(0xa) != 6 || device.read8(9) != 1 {
        return Err(Error::Device);
    }
    let window = window.ok_or(Error::Resource)?;
    let decode = unsafe { device.register::<u8, Decode::Register>(0x9c) };
    if window.kind != PciWindowKind::Mmio
        || decode.read(Decode::ASSEL) != 0
        || fstart_pci::PCI_MEMORY_BAR::PREFETCHABLE.is_set(device.read32(0x24))
        || window.base == 0
        || window.base & (ABAR_BYTES - 1) != 0
        || window.size != ABAR_BYTES
        || window.base.checked_add(window.size).is_none()
        || window.base + window.size > 1u64 << 32
        || device.memory_bar(0x24) != Some(window.base)
    {
        return Err(Error::Resource);
    }
    let base = usize::try_from(window.base).map_err(|_| Error::Resource)?;
    let command = unsafe { device.register::<u16, PCI_COMMAND_BITS::Register>(4) };
    command.modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET + PCI_COMMAND_BITS::IO_SPACE::SET);
    // Defer the source BME enable until after checking idle engines and
    // completing policy. No DMA rings or transfer requests are installed.
    let hba = unsafe { &*(base as *const AhciRegs<8>) };
    quiescent(hba)?;
    let controller = Controller {
        device,
        kind: prepared.kind,
    };
    let (configured, hotplug, _) = prepared.policy.maps()?;
    if hba.pi.get() & u32::from(configured) != u32::from(configured) {
        return Err(Error::Readback);
    }
    let present = controller.present() & configured;
    let final_ports = prepared.policy.final_ports(present)?;
    let spin = hba.cap.is_set(CAP::SSS);
    for (index, policy) in prepared.policy.ports.into_iter().enumerate() {
        if !policy.enabled {
            continue;
        }
        let port = &hba.ports[index];
        // Source clears only these policy bits; SUD is ORed only when SSS.
        port.cmd.modify(
            PXCMD::ASP::CLEAR
                + PXCMD::ALPE::SET
                + PXCMD::FBSCP::CLEAR
                + PXCMD::ESP::CLEAR
                + PXCMD::MPSP::CLEAR
                + PXCMD::HPCP.val(u32::from(policy.hotplug)),
        );
        if spin {
            port.cmd.modify(PXCMD::SUD::SET);
        }
        if policy.devslp {
            port.devslp
                .modify(PXDEVSLP::DSP::SET + PXDEVSLP::DM.val(15) + PXDEVSLP::DITO.val(625));
        } else {
            port.devslp.modify(PXDEVSLP::DSP::CLEAR);
        }
    }
    // No RST remapping: source disables ABAR decode and locks every unused CR.
    unsafe { MmioRegister::<u32, RemapDecode::Register>::new(base + 0xa4) }
        .write(RemapDecode::OFFSET.val(0xf));
    controller.lock_cycles()?;
    controller.power_optimization(hotplug);
    controller.late_sir();
    controller.thermal();
    controller.cycle_power(true);
    match prepared.kind {
        super::dmi::Kind::H => {
            let map = unsafe { device.register::<u32, Map::Register>(0x90) };
            map.modify(
                Map::SPEED_DISABLE.val(map.read(Map::SPEED_DISABLE) | u32::from(!final_ports)),
            );
            let pcs = unsafe { device.register::<u16, PortEnable::Register>(0x94) };
            pcs.modify(PortEnable::PORTS.val(pcs.read(PortEnable::PORTS) | u16::from(final_ports)));
            let gc = unsafe { device.register::<u32, General::Register>(0x9c) };
            gc.modify(General::LOCK::SET);
            if !gc.is_set(General::LOCK)
                || map.read(Map::SPEED_DISABLE) & u32::from(!final_ports) != u32::from(!final_ports)
                || pcs.read(PortEnable::PORTS) & u16::from(final_ports) != u16::from(final_ports)
            {
                return Err(Error::Readback);
            }
        }
        super::dmi::Kind::Lp => {
            let unused = !final_ports & 0x7;
            let map = unsafe { device.register::<u16, LpMap::Register>(0x90) };
            map.modify(
                LpMap::SPEED_DISABLE.val(map.read(LpMap::SPEED_DISABLE) | u16::from(unused)),
            );
            let pcs = unsafe { device.register::<u16, LpPcs::Register>(0x92) };
            pcs.modify(LpPcs::PORTS.val(pcs.read(LpPcs::PORTS) | u16::from(final_ports & 0x7)));
            let gc = unsafe { device.register::<u32, General::Register>(0x9c) };
            gc.modify(General::LOCK::SET);
            if !gc.is_set(General::LOCK)
                || map.read(LpMap::SPEED_DISABLE) & u16::from(unused) != u16::from(unused)
                || pcs.read(LpPcs::PORTS) & u16::from(final_ports & 0x7)
                    != u16::from(final_ports & 0x7)
            {
                return Err(Error::Readback);
            }
        }
    }
    command.modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
    if !command.matches_all(
        PCI_COMMAND_BITS::BUS_MASTER::SET
            + PCI_COMMAND_BITS::MEMORY_SPACE::SET
            + PCI_COMMAND_BITS::IO_SPACE::SET,
    ) {
        return Err(Error::Readback);
    }
    Ok(Outcome::Ahci {
        configured,
        present,
        final_ports,
        cycle_capabilities: prepared.cycle_capabilities,
    })
}
