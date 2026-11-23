// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sunrise Point LPC decode and required DMI mirrors.
//!
//! Derived from coreboot common/block/lpc/lpc_lib.c and gpmr/gpmr.c.
//! This is not the older ICH RCBA/BIOS-control register block: flash control
//! on this PCH belongs to the separate SPI function.

use super::pcr::{self, Pcr, Port};
use crate::southbridge::lpc::encode_generic_io_window;
use fstart_pci::ecam::EcamDevice;
use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u16,
    pub IoDecode [
        COM_A OFFSET(0) NUMBITS(3) [],
        COM_B OFFSET(4) NUMBITS(3) [],
        LPT OFFSET(8) NUMBITS(2) [],
        FDD OFFSET(12) NUMBITS(1) []
    ],
    pub IoEnable [
        COM_A OFFSET(0) NUMBITS(1) [],
        COM_B OFFSET(1) NUMBITS(1) [],
        LPT OFFSET(2) NUMBITS(1) [],
        FDD OFFSET(3) NUMBITS(1) [],
        GAME_LOW OFFSET(8) NUMBITS(1) [],
        GAME_HIGH OFFSET(9) NUMBITS(1) [],
        KBC OFFSET(10) NUMBITS(1) [],
        EC OFFSET(11) NUMBITS(1) [],
        SUPERIO_2E OFFSET(12) NUMBITS(1) [],
        EC_4E OFFSET(13) NUMBITS(1) []
    ]
];

register_bitfields![u8,
    SerialIrqControl [
        CONTINUOUS OFFSET(6) NUMBITS(1) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    ClockControl [ CLKRUN OFFSET(0) NUMBITS(1) [] ],
    PpiControl [
        SERR_DISABLE OFFSET(2) NUMBITS(1) [],
        STATUS OFFSET(4) NUMBITS(4) []
    ]
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SerialIrq {
    Disabled,
    Continuous,
    Quiet,
}

register_bitfields![u32,
    GenericIo [
        ENABLE OFFSET(0) NUMBITS(1) [],
        BASE OFFSET(2) NUMBITS(14) [],
        LENGTH OFFSET(18) NUMBITS(6) []
    ],
    PowerControl [
        LOW OFFSET(0) NUMBITS(4) [],
        CLEAR OFFSET(8) NUMBITS(3) [],
        BIT13 OFFSET(13) NUMBITS(1) []
    ]
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    WrongFunction,
    Unavailable,
    InvalidConfig,
    Readback,
    Pcr(pcr::Error),
}
impl From<pcr::Error> for Error {
    fn from(value: pcr::Error) -> Self {
        Self::Pcr(value)
    }
}

/// Native pch.c LPC PRC power policy; the post-PCI legacy initializer owns
/// CLKRUN, so it is not programmed a second time here.
/// # Safety
/// Own the actual cold H LPC PCR, before final endpoint masks/locks.
pub(crate) unsafe fn configure_power(pcr: &Pcr) -> Result<(), Error> {
    use tock_registers::interfaces::ReadWriteable;
    let r = pcr.register::<PowerControl::Register>(Port::Lpc, 0x341c)?;
    if r.get() == u32::MAX {
        return Err(Error::Unavailable);
    }
    let fields = PowerControl::LOW.val(15) + PowerControl::CLEAR.val(0) + PowerControl::BIT13::SET;
    r.modify(fields);
    if !r.matches_all(fields) {
        return Err(Error::Readback);
    }
    Ok(())
}

/// Open one additional LPC I/O window without discarding the board's existing
/// generic decodes. Use a covering range or the first disabled slot, and keep
/// the PCH DMI mirror synchronized through the existing PCR transport.
pub fn open_io_window(device: &EcamDevice, pcr: &Pcr, base: u16, size: u16) -> Result<(), Error> {
    let wanted = encode_generic_io_window(base, size).ok_or(Error::InvalidConfig)?;
    if device.address().segment() != 0
        || device.bus() != 0
        || device.dev() != 0x1f
        || device.func() != 0
    {
        return Err(Error::WrongFunction);
    }
    if device.vendor_id() != 0x8086 {
        return Err(Error::Unavailable);
    }
    let mut available = None;
    let mut covering = None;
    for index in 0..4u16 {
        let current =
            LocalRegisterCopy::<u32, GenericIo::Register>::new(device.read32(0x84 + index * 4));
        if !current.is_set(GenericIo::ENABLE) {
            if available.is_none() {
                available = Some(index);
            }
            continue;
        }
        let start = current.read(GenericIo::BASE) << 2;
        let length = current.read(GenericIo::LENGTH) * 4 + 4;
        if current.get() & !0x00fc_fffd != 0
            || !crate::southbridge::lpc::valid_generic_io_window(start as u16, length as u16)
        {
            return Err(Error::InvalidConfig);
        }
        if start <= u32::from(base) && u32::from(base) + u32::from(size) <= start + length {
            covering = Some((index, current.get()));
        }
    }
    let (index, value) = covering
        .or_else(|| available.map(|index| (index, wanted)))
        .ok_or(Error::InvalidConfig)?;
    let register = unsafe { device.register::<u32, GenericIo::Register>(0x84 + index * 4) };
    if register.get() != value {
        register.set(value);
    }
    let mirror = pcr.register::<()>(Port::Dmi, 0x2730 + index * 4)?;
    mirror.set(value);
    if register.get() != value || mirror.get() != value {
        return Err(Error::InvalidConfig);
    }
    Ok(())
}

/// Board-owned ranges. All four generic slots and the optional 64-KiB memory
/// window are authoritative; omitted entries disable stale previous decodes.
#[derive(Debug, Clone, Copy)]
pub struct DecodeConfig {
    pub io_decode: u16,
    /// ORed into existing fixed-range enables, matching the native source.
    pub io_enable: u16,
    pub generic_io: [Option<(u16, u16)>; 4],
    pub generic_memory: Option<u32>,
}
impl DecodeConfig {
    fn encode(&self) -> Result<([u32; 4], u32), Error> {
        if self.io_decode & !0x1377 != 0 || self.io_enable & !0x3f0f != 0 {
            return Err(Error::InvalidConfig);
        }
        let mut encoded = [0; 4];
        for (index, window) in self.generic_io.iter().enumerate() {
            let Some((base, size)) = *window else {
                continue;
            };
            encoded[index] = encode_generic_io_window(base, size).ok_or(Error::InvalidConfig)?;
            for other in self.generic_io[..index].iter().flatten() {
                if u32::from(base) < u32::from(other.0) + u32::from(other.1)
                    && u32::from(other.0) < u32::from(base) + u32::from(size)
                {
                    return Err(Error::InvalidConfig);
                }
            }
        }
        let memory = match self.generic_memory {
            Some(base) if base != 0 && base & 0xffff == 0 => base | 1,
            None => 0,
            _ => return Err(Error::InvalidConfig),
        };
        Ok((encoded, memory))
    }
}

/// Configure the bound 00:1f.0 Intel LPC function after P2SB is enabled.
/// The platform owns/reserves all ranges and serializes this operation.
pub fn configure_decodes(
    device: &EcamDevice,
    pcr: &Pcr,
    config: &DecodeConfig,
) -> Result<(), Error> {
    let (generic, memory) = config.encode()?;
    if device.address().segment() != 0
        || device.bus() != 0
        || device.dev() != 0x1f
        || device.func() != 0
    {
        return Err(Error::WrongFunction);
    }
    if device.vendor_id() != 0x8086 {
        return Err(Error::Unavailable);
    }
    let iod = unsafe { device.register::<u16, IoDecode::Register>(0x80) };
    // Retain reserved fields outside the source's documented mask. The source
    // lpc_set_fixed_io_ranges masks its preserved value a second time and loses
    // it; do not reproduce that accidental destruction of unrelated bits.
    let mut decode = LocalRegisterCopy::<u16, IoDecode::Register>::new(iod.get());
    decode.set((decode.get() & !0x1377) | config.io_decode);
    iod.set(decode.get());
    pcr.register::<()>(Port::Dmi, 0x2770)?
        .set(u32::from(decode.get()));
    let ioe = unsafe { device.register::<u16, IoEnable::Register>(0x82) };
    let enabled = ioe.get() | config.io_enable;
    ioe.set(enabled);
    pcr.register::<()>(Port::Dmi, 0x2774)?
        .set(u32::from(enabled));
    for (index, value) in generic.into_iter().enumerate() {
        unsafe {
            device
                .register::<u32, ()>(0x84 + 4 * index as u16)
                .set(value)
        };
        pcr.register::<()>(Port::Dmi, 0x2730 + 4 * index as u16)?
            .set(value);
    }
    unsafe { device.register::<u32, ()>(0x98).set(memory) };
    pcr.register::<()>(Port::Dmi, 0x2740)?.set(memory);
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct LegacyPolicy {
    pub serial_irq: SerialIrq,
    pub pcat: bool,
    pub interrupts: super::irq::Policy,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitError {
    Device,
    Readback,
    Espi(super::espi::Error),
    Irq(super::irq::Error),
}

/// Native LPC device initialization, AFTER canonical PCI resource assignment.
/// Flash control belongs to the separately owned SPI function; never reuse an
/// ICH BIOS-control/RCBA programming path here.
/// # Safety
/// Own actual PCH-H PCI/PCR, IOAPIC/LAPIC and decoded PC/AT ports. Run only in
/// the authenticated cold successor, with interrupts disabled and no AP/OS
/// competitors. Any partial failure is fatal, not a retry/rollback request.
pub unsafe fn initialize_legacy(
    lpc: &EcamDevice,
    pcr: &Pcr,
    pci: &fstart_pci::PciEcam,
    policy: LegacyPolicy,
    delay: &mut impl embedded_hal::delay::DelayNs,
) -> Result<super::espi::Interface, InitError> {
    use fstart_arch::x86::{ioapic::IoApic, lapic::Lapic, legacy_pc};
    use fstart_core::pio::PioRegister;
    use tock_registers::interfaces::ReadWriteable;
    let silicon = super::hsio::Silicon::read(lpc).map_err(|_| InitError::Device)?;
    if silicon.kind() != super::dmi::Kind::H {
        return Err(InitError::Device);
    }
    policy.interrupts.validate().map_err(InitError::Irq)?;
    legacy_pc::initialize_isa_dma();
    let ppi = PioRegister::<u8, PpiControl::Register>::new(0x61);
    let status = ppi.read(PpiControl::STATUS);
    ppi.write(PpiControl::STATUS.val(status) + PpiControl::SERR_DISABLE::SET);
    PioRegister::<u8>::new(0x70).set(0x80); // Native NMI disable, CMOS index 0.
    unsafe { lpc.register::<u8, ClockControl::Register>(0xe0) }.modify(ClockControl::CLKRUN::SET);
    let interface = unsafe { super::espi::initialize(lpc, pcr, delay) }.map_err(InitError::Espi)?;
    let serial = unsafe { lpc.register::<u8, SerialIrqControl::Register>(0x64) };
    if policy.serial_irq == SerialIrq::Disabled {
        serial.modify(SerialIrqControl::ENABLE::CLEAR);
    } else {
        // Quiet mode requires the initial continuous frame on this PCH.
        serial.modify(SerialIrqControl::ENABLE::SET + SerialIrqControl::CONTINUOUS::SET);
        if policy.serial_irq == SerialIrq::Quiet {
            serial.modify(SerialIrqControl::CONTINUOUS::CLEAR);
        }
    }
    let p2sb = EcamDevice::new(0, 0x1f, 1);
    unsafe { super::irq::initialize_ioapic(pcr, &p2sb) }.map_err(InitError::Irq)?;
    super::irq::assign_pic_lines(pcr, pci, policy.interrupts).map_err(InitError::Irq)?;
    if policy.pcat {
        let lapic = Lapic::from_msr();
        let previous = lapic.set_extint_masked(true);
        legacy_pc::initialize_pic(0x20, 0x28);
        PioRegister::<u8>::new(legacy_pc::PIC_SLAVE_MASK).set(0xff);
        PioRegister::<u8>::new(legacy_pc::PIC_MASTER_MASK).set(0xfb);
        if !legacy_pc::configure_pic_trigger(9, true) {
            return Err(InitError::Readback);
        }
        let bsp_id = u8::try_from(lapic.id()).map_err(|_| InitError::Device)?;
        unsafe { IoApic::new(super::irq::IOAPIC_BASE) }.route_i8259(bsp_id);
        if let Some(masked) = previous {
            lapic.set_extint_masked(masked);
        }
    }
    Ok(interface)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sunrise_point::pcr::testing::Image;

    #[test]
    fn lpc_invalid_or_overlapping_ranges_fail_before_hardware_access() {
        let image = Image::new();
        let pcr = image.pcr();
        let device = EcamDevice::new(0, 0x1f, 0);
        let valid = DecodeConfig {
            io_decode: 0,
            io_enable: 0,
            generic_io: [None; 4],
            generic_memory: None,
        };
        for config in [
            DecodeConfig {
                io_decode: 0x8000,
                ..valid
            },
            DecodeConfig {
                io_enable: 0x8000,
                ..valid
            },
            DecodeConfig {
                generic_memory: Some(0),
                ..valid
            },
            DecodeConfig {
                generic_memory: Some(0xfe20_0001),
                ..valid
            },
            DecodeConfig {
                generic_io: [Some((0x1600, 12)), None, None, None],
                ..valid
            },
            DecodeConfig {
                generic_io: [Some((0x1600, 0x80)), Some((0x1640, 0x40)), None, None],
                ..valid
            },
        ] {
            assert_eq!(
                configure_decodes(&device, &pcr, &config),
                Err(Error::InvalidConfig)
            );
        }
        assert_eq!(
            configure_decodes(&EcamDevice::new(0, 0x1f, 3), &pcr, &valid),
            Err(Error::WrongFunction)
        );
        assert_eq!(pcr.register::<()>(Port::Dmi, 0x2730).unwrap().get(), 0);
    }
}
