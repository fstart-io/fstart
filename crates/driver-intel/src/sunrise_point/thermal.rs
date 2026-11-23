// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native PCH-H PCI thermal sensor, skylake/thermal.c SHA256
//! d19af4bc51a453d872225369ac2dd5bcd3f0f402880d39e8f5c9d31280ac5928.
//! PCI mode only: LP cross-throttling, ACPI/disabled-device modes and optional
//! PMSYNC memory-throttling wiring are not offered by this owner. This PCH
//! sensor recipe is NOT a board fan/EC policy or calibrated temperature proof.
use super::{
    gpio::{Group, PadDw0},
    hsio,
    pcr::{self, Pcr, Port},
};
use fstart_core::mmio::MmioReadWrite as RW;
use fstart_pci::{EcamDevice, PCI_COMMAND_BITS, PciWindow, PciWindowKind};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields, register_structs,
};
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Native common/block/thermal: zero selects 50 C; maximum is 205 C.
    pub low_trip_c: u8,
}
impl Config {
    pub const NATIVE: Self = Self { low_trip_c: 0 };
    fn encoded(self) -> Result<u16, Error> {
        let c = if self.low_trip_c == 0 {
            50
        } else {
            self.low_trip_c
        };
        if c > 205 {
            return Err(Error::Policy);
        }
        Ok((u16::from(c) + 50) * 2)
    }
    pub(crate) fn validate(self) -> Result<(), Error> {
        self.encoded().map(|_| ())
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Policy,
    Device,
    Resource,
    Locked,
    Readback,
    Pcr(pcr::Error),
    Gpio(super::gpio::Error),
    Hsio(hsio::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}
impl From<super::gpio::Error> for Error {
    fn from(e: super::gpio::Error) -> Self {
        Self::Gpio(e)
    }
}
impl From<hsio::Error> for Error {
    fn from(e: hsio::Error) -> Self {
        Self::Hsio(e)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct Outcome {
    pub low_trip_c: u8,
    pub pchhot_native: bool,
}
register_bitfields![u8,
    SensorControl [
        CATASTROPHIC OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(7) NUMBITS(1) []
    ],
    SensorEnable [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(7) NUMBITS(1) []
    ],
    SmiControl [ LOCK OFFSET(7) NUMBITS(1) [] ],
    HotControl [ LOCK OFFSET(0) NUMBITS(1) [] ],
    DeviceDisable [ DISABLE OFFSET(0) NUMBITS(1) [] ],
    Misc0 [ ENABLE OFFSET(0) NUMBITS(1) [] ]
];
register_bitfields![u16,
    SensorPower [
        TRIP OFFSET(0) NUMBITS(9) [],
        NATIVE_CONFIG OFFSET(9) NUMBITS(6) [],
        LOCK OFFSET(15) NUMBITS(1) []
    ],
    Levels2 [
        PMC_ENABLE OFFSET(14) NUMBITS(1) [],
        LOCK OFFSET(15) NUMBITS(1) []
    ],
    HotLevel [ ENABLE OFFSET(15) NUMBITS(1) [] ],
    MiscA4 [ ENABLE OFFSET(0) NUMBITS(3) [] ]
];
register_bitfields![u32,
    Levels [
        T0 OFFSET(0) NUMBITS(9) [],
        T1 OFFSET(10) NUMBITS(9) [],
        T2 OFFSET(20) NUMBITS(9) [],
        ENABLE OFFSET(29) NUMBITS(1) [],
        LOCK OFFSET(31) NUMBITS(1) []
    ],
    DmiThermal [
        BIT0 OFFSET(0) NUMBITS(1) [],
        CLEAR OFFSET(4) NUMBITS(4) [],
        BIT8 OFFSET(8) NUMBITS(1) [],
        CLEAR2 OFFSET(9) NUMBITS(2) [],
        BIT11 OFFSET(11) NUMBITS(1) []
    ]
];
register_structs! {
    Registers {
        (0 => _r0), (4 => control:RW<u8,SensorControl::Register>), (5 => _r1),
        (6 => status:RW<u8>), (7 => _r2), (8 => enable:RW<u8,SensorEnable::Register>), (9 => _r3),
        (0xa => report_lock:RW<u8>), (0xb => _r4), (0xc => smi:RW<u8,SmiControl::Register>), (0xd => _r5),
        (0x10 => catastrophic:RW<u16>), (0x12 => _r6), (0x1c => power:RW<u16,SensorPower::Register>), (0x1e => _r7),
        (0x40 => levels:RW<u32,Levels::Register>), (0x44 => _r8), (0x50 => levels2:RW<u16,Levels2::Register>), (0x52 => _r9),
        (0x60 => hot:RW<u16,HotLevel::Register>), (0x62 => hot_control:RW<u8,HotControl::Register>), (0x63 => _r10),
        (0x80 => auxiliary_status:RW<u8>), (0x81 => _r11), (0xa4 => misc_a4:RW<u16,MiscA4::Register>), (0xa6 => _r12),
        (0xc0 => misc0:RW<u8,Misc0::Register>), (0xc1 => _r13), (0xc4 => misc4:RW<u32>), (0xc8 => misc8:RW<u32>),
        (0xcc => calibration: fstart_core::mmio::MmioReadOnly<u16>), (0xce => _r14), (0xd0 => misc_d0:RW<u32>), (0xd4 => _r15),
        (0xe0 => misc_e0:RW<u32>), (0xe4 => misc_e4:RW<u32>), (0xe8 => misc_e8:RW<u32>), (0xec => _r16),
        (0xf0 => disabled: fstart_core::mmio::MmioReadOnly<u8,DeviceDisable::Register>), (0xf1 => _r17), (0x1000 => @END),
    }
}
/// # Safety
/// Actual cold PCH-H, exclusive assigned PCI thermal BAR mapped UC, GPIO B23
/// and DMI thermal controls. No AP/SMI/OS owner. The canonical allocator owns
/// this exact BAR0 window; no temporary or invented mapping is accepted.
/// Partial locks are irreversible and any failure forbids replay.
pub(crate) unsafe fn initialize(
    config: Config,
    pcr: &Pcr,
    window: PciWindow,
) -> Result<Outcome, Error> {
    let trip = config.encoded()?;
    if hsio::Silicon::read(&EcamDevice::new(0, 0x1f, 0))?.kind() != super::dmi::Kind::H {
        return Err(Error::Device);
    }
    let device = EcamDevice::new(0, 0x14, 2);
    if device.vendor_id() != 0x8086 || device.read16(0xa) != 0x1180 {
        return Err(Error::Device);
    }
    let base = usize::try_from(window.base).map_err(|_| Error::Resource)?;
    if window.kind != PciWindowKind::Mmio
        || fstart_pci::PCI_MEMORY_BAR::PREFETCHABLE.is_set(device.read32(0x10))
        || base == 0
        || base & 0xfff != 0
        || window.size < 0x1000
        || window.base.checked_add(window.size).is_none()
        || usize::try_from(window.base + window.size - 1).is_err()
        || device.memory_bar(0x10) != Some(window.base)
    {
        return Err(Error::Resource);
    }
    unsafe { device.register::<u16, PCI_COMMAND_BITS::Register>(4) }
        .modify(PCI_COMMAND_BITS::MEMORY_SPACE::SET);
    if device.read16(4) & 2 == 0 {
        return Err(Error::Readback);
    }
    let r = unsafe { &*(base as *const Registers) };
    if r.disabled.is_set(DeviceDisable::DISABLE)
        || r.control.is_set(SensorControl::LOCK)
        || r.enable.is_set(SensorEnable::LOCK)
        || r.smi.is_set(SmiControl::LOCK)
        || r.hot_control.is_set(HotControl::LOCK)
        || r.power.is_set(SensorPower::LOCK)
        || r.levels.is_set(Levels::LOCK)
        || r.levels2.is_set(Levels2::LOCK)
    {
        return Err(Error::Locked);
    }
    let pad = Group::B.pad(23)?;
    let pchhot = pcr
        .register::<PadDw0::Register>(pad.group().info().port, pad.config_offset())?
        .read(PadDw0::MODE)
        == 2;
    r.catastrophic.set(0x14a);
    // Explicit boot thermal-status acknowledgement: only observed RW1C bits,
    // not a blanket FF write that also consumes newly arriving events.
    r.status.set(r.status.get());
    r.auxiliary_status.set(r.auxiliary_status.get());
    r.report_lock.set(1);
    if pchhot {
        r.hot.write(HotLevel::ENABLE::SET);
    }
    // Native PCH-H internal throttle words, not invented external fan policy.
    r.levels
        .write(Levels::T0.val(0x13c) + Levels::T1.val(0x142) + Levels::T2.val(0x148));
    r.levels.modify(Levels::LOCK::SET + Levels::ENABLE::SET);
    r.power
        .write(SensorPower::NATIVE_CONFIG.val(0x25) + SensorPower::TRIP.val(trip));
    r.misc0.modify(Misc0::ENABLE::SET);
    r.misc4
        .set((r.misc4.get() & !((1 << 12) | (1 << 11))) | 0x7c0);
    r.misc8.set((r.misc8.get() & 0xffff) | 0x00100000);
    r.misc_d0
        .set((r.misc_d0.get() & !((1 << 25) | (1 << 23) | (1 << 22) | (1 << 21))) | (1 << 24));
    r.misc_e4.set((r.misc_e4.get() & !0x0600070e) | 0x19000810);
    r.misc_e8.set((r.misc_e8.get() & !0xff00) | 0x8000);
    if r.calibration.get() == 0 {
        r.misc_e0.set(0x800002be);
    }
    r.misc_a4.modify(MiscA4::ENABLE.val(7));
    r.enable.modify(SensorEnable::ENABLE::SET);
    pcr.register::<DmiThermal::Register>(Port::Dmi, 0x2238)?
        .modify(
            DmiThermal::CLEAR.val(0)
                + DmiThermal::CLEAR2.val(0)
                + DmiThermal::BIT11::SET
                + DmiThermal::BIT8::SET
                + DmiThermal::BIT0::SET,
        );
    r.control
        .modify(SensorControl::LOCK::SET + SensorControl::CATASTROPHIC::SET);
    r.smi.modify(SmiControl::LOCK::SET);
    r.hot_control.modify(HotControl::LOCK::SET);
    r.levels2.modify(Levels2::LOCK::SET);
    r.enable.modify(SensorEnable::LOCK::SET);
    r.power.modify(SensorPower::LOCK::SET);
    if !r
        .control
        .matches_all(SensorControl::LOCK::SET + SensorControl::CATASTROPHIC::SET)
        || !r
            .enable
            .matches_all(SensorEnable::LOCK::SET + SensorEnable::ENABLE::SET)
        || !r.levels.matches_all(
            Levels::LOCK::SET
                + Levels::ENABLE::SET
                + Levels::T0.val(0x13c)
                + Levels::T1.val(0x142)
                + Levels::T2.val(0x148),
        )
        || !r.power.matches_all(
            SensorPower::LOCK::SET
                + SensorPower::TRIP.val(trip)
                + SensorPower::NATIVE_CONFIG.val(0x25),
        )
        || !r.smi.is_set(SmiControl::LOCK)
        || !r.hot_control.is_set(HotControl::LOCK)
        || !r.levels2.is_set(Levels2::LOCK)
        || pchhot && !r.hot.is_set(HotLevel::ENABLE)
    {
        return Err(Error::Readback);
    }
    Ok(Outcome {
        low_trip_c: ((trip / 2) - 50) as u8,
        pchhot_native: pchhot,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_default_and_highest_representable_trip_are_valid_but_higher_policy_is_rejected() {
        assert_eq!(Config::NATIVE.encoded(), Ok(200));
        assert_eq!(Config { low_trip_c: 205 }.encoded(), Ok(510));
        assert_eq!(Config { low_trip_c: 206 }.encoded(), Err(Error::Policy));
    }
}
