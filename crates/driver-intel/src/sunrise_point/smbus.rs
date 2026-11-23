// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native enabled PCH-H SMBus power policy, skylake/pch.c SHA256
//! da2f5460aa14d21335852802f08ba2cea163f98bcbf630a8b71cd90c07e736c6.
//! Receiver setup: common/block/smbus/smbus.c
//! 7d8be9d44fbafc6419bdd424ea0c86cafa7819a8849f6a38a3f12e8d8afad90c;
//! common/pch/include/intelpch/smbus.h
//! f09761162b067abd6ac59f075d96343794240ae3634933dd1f3dbb2964a8b6b1.
//! Transactions and PIO registers remain single-owned by the shared I801
//! driver. No disabled mode: this owner keeps the actual TCO decode/lock PCI
//! function accessible through finalization rather than hiding it early.
use super::pcr::{self, Pcr, Port};
use crate::southbridge::smbus::I801SmBus;
use fstart_core::services::ServiceError;
use fstart_pci::{EcamDevice, PciAddress, PciEcam};
use tock_registers::{
    interfaces::{ReadWriteable, Readable},
    register_bitfields,
};
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device,
    Owned,
    Readback,
    Controller(ServiceError),
    Pcr(pcr::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<ServiceError> for Error {
    fn from(e: ServiceError) -> Self {
        Self::Controller(e)
    }
}
register_bitfields![u32,
    Clock [
        DYNAMIC OFFSET(5) NUMBITS(1) [],
        DISABLE8 OFFSET(8) NUMBITS(1) [],
        DISABLE10 OFFSET(10) NUMBITS(1) [],
        DISABLE12 OFFSET(12) NUMBITS(1) [],
        DISABLE14 OFFSET(14) NUMBITS(1) [],
        DISABLE16 OFFSET(16) NUMBITS(1) [],
        PGCB_DISABLE OFFSET(18) NUMBITS(1) []
    ],
    Power [
        PMC_GATING OFFSET(0) NUMBITS(3) [],
        HAE OFFSET(5) NUMBITS(1) []
    ],
    General [ DISABLE OFFSET(0) NUMBITS(1) [] ],
    Shadow [ DISABLE OFFSET(8) NUMBITS(1) [] ],Visibility [ HIDE OFFSET(0) NUMBITS(1) [] ]
];
fn validate(device: &EcamDevice, bus: &I801SmBus) -> Result<(), Error> {
    let base = bus.base();
    if device.address() != PciAddress::new(0, 0, 0x1f, 4)
        || device.vendor_id() != 0x8086
        || device.read32(8) >> 16 != 0x0c05
        || base == 0
        || base & 31 != 0
        || base.checked_add(31).is_none()
        || device.read32(0x20) != (u32::from(base) | 1)
        || device.read16(4) & 1 == 0
        || device.read8(0x40) & 1 == 0
    {
        return Err(Error::Device);
    }
    Ok(())
}
/// # Safety
/// Exclusive actual cold H SMBus/PCR and decoded I801 ports, AFTER SPD/board
/// transactions completed and board pads were configured. No CSE/AP/SMI/OS
/// host transaction competes. Never abort/reset a foreign busy controller.
pub(crate) unsafe fn prepare(device: &EcamDevice, pcr: &Pcr, bus: &I801SmBus) -> Result<(), Error> {
    validate(device, bus)?;
    bus.ensure_idle()?;
    let gc = pcr.register::<General::Register>(Port::Smbus, 0x0c)?;
    let shadow = pcr.register::<Shadow::Register>(Port::Psf3, 0x51c)?;
    let visibility = pcr.register::<Visibility::Register>(Port::Psf3, 0x538)?;
    let power = pcr.register::<Power::Register>(Port::Smbus, 0x10)?;
    let clock = unsafe { device.register::<u32, Clock::Register>(0x80) };
    if [
        gc.get(),
        shadow.get(),
        visibility.get(),
        power.get(),
        clock.get(),
    ]
    .contains(&u32::MAX)
    {
        return Err(Error::Device);
    }
    if gc.is_set(General::DISABLE)
        || shadow.is_set(Shadow::DISABLE)
        || visibility.is_set(Visibility::HIDE)
    {
        return Err(Error::Owned);
    }
    let power_fields = Power::PMC_GATING.val(0) + Power::HAE::SET;
    power.modify(power_fields);
    // One owner for native's [16,14,12,10,8] clear + bit18/5 set and common
    // SMBus driver's subset clear. Do not perform the recipe twice.
    let fields = Clock::DISABLE8::CLEAR
        + Clock::DISABLE10::CLEAR
        + Clock::DISABLE12::CLEAR
        + Clock::DISABLE14::CLEAR
        + Clock::DISABLE16::CLEAR
        + Clock::PGCB_DISABLE::SET
        + Clock::DYNAMIC::SET;
    clock.modify(fields);
    if !power.matches_all(power_fields) || !clock.matches_all(fields) {
        return Err(Error::Readback);
    }
    bus.acknowledge_alert()?;
    Ok(())
}
/// # Safety
/// Canonical PCI enumeration completed with the I/O BAR preserved; same real
/// controller/ports as prepare. No transfer, reset, KILL or status blanket-clear.
pub(crate) unsafe fn finish(
    device: &EcamDevice,
    pci: &PciEcam,
    bus: &I801SmBus,
) -> Result<(), Error> {
    validate(device, bus)?;
    if !pci.devices().any(|address| address == device.address()) {
        return Err(Error::Device);
    }
    bus.set_native_receive_address(0x44)?;
    Ok(())
}
