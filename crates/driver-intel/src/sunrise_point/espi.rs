// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native Skylake lpc.c eSPI master/slave bus mastering, bounded PCR protocol.
use super::pcr::{self, Pcr, Port};
use embedded_hal::delay::DelayNs;
use fstart_pci::{EcamDevice, PCI_COMMAND_BITS};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};
register_bitfields![u32,
    Strap [ ENABLE OFFSET(0) NUMBITS(1) [] ],
    SoftStraps [ SECOND_SLAVE OFFSET(12) NUMBITS(1) [] ],
    SlaveControl [
        ADDRESS OFFSET(0) NUMBITS(12) [],
        COMMAND OFFSET(16) NUMBITS(2) [
            Read=0,
            Write=1
        ],
        SLAVE OFFSET(19) NUMBITS(2) [],
        WRITE_LOCK OFFSET(27) NUMBITS(1) [],
        STATUS OFFSET(28) NUMBITS(3) [],
        BUSY OFFSET(31) NUMBITS(1) []
    ],
    LinkError [ CONFIG_RECOVERY OFFSET(31) NUMBITS(1) [] ],
    Channel [ BUS_MASTER OFFSET(2) NUMBITS(1) [] ]
];
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
    Lpc,
    Espi { slaves: u8 },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Device,
    Input,
    Busy,
    Locked,
    Timeout,
    Status(u8),
    Readback,
    Pcr(pcr::Error),
}
impl From<pcr::Error> for Error {
    fn from(e: pcr::Error) -> Self {
        Self::Pcr(e)
    }
}

fn transaction(
    pcr: &Pcr,
    slave: u8,
    address: u16,
    write: Option<u32>,
    delay: &mut impl DelayNs,
) -> Result<u32, Error> {
    if slave >= 2 || address > 0xfff || address & 3 != 0 {
        return Err(Error::Input);
    }
    let control = pcr.register::<SlaveControl::Register>(Port::EspiSpi, 0x4000)?;
    if control.is_set(SlaveControl::BUSY) {
        return Err(Error::Busy);
    }
    if write.is_some()
        && address <= 0x7ff
        && control.is_set(SlaveControl::WRITE_LOCK)
        && !pcr
            .register::<LinkError::Register>(Port::EspiSpi, 0x4050 + 4 * u16::from(slave))?
            .is_set(LinkError::CONFIG_RECOVERY)
    {
        return Err(Error::Locked);
    }
    // Clear old RW1C status before starting a NEW operation, not alongside it.
    control.modify(SlaveControl::STATUS.val(7));
    let data = pcr.register::<()>(Port::EspiSpi, 0x4004)?;
    if let Some(value) = write {
        data.set(value);
    }
    control.modify(
        SlaveControl::ADDRESS.val(u32::from(address))
            + SlaveControl::SLAVE.val(u32::from(slave))
            + SlaveControl::COMMAND.val(u32::from(write.is_some()))
            + SlaveControl::STATUS.val(0)
            + SlaveControl::BUSY::SET,
    );
    for tick in 0..=10_000 {
        let status = control.extract();
        if !status.is_set(SlaveControl::BUSY) {
            let result = status.read(SlaveControl::STATUS) as u8;
            if result != 7 {
                return Err(Error::Status(result));
            }
            return Ok(data.get());
        }
        if tick == 10_000 {
            break;
        }
        delay.delay_us(10);
    }
    Err(Error::Timeout)
}

/// # Safety
/// Own actual PCH-H LPC/eSPI config and PCR engines. No competing slave
/// transaction or external PCI reconfiguration. Any partial failure is fatal;
/// unlike the reference's warning/continue loop, it does not claim completion.
pub unsafe fn initialize(
    lpc: &EcamDevice,
    pcr: &Pcr,
    delay: &mut impl DelayNs,
) -> Result<Interface, Error> {
    if lpc.address().segment() != 0
        || lpc.bus() != 0
        || lpc.dev() != 0x1f
        || lpc.func() != 0
        || lpc.vendor_id() != 0x8086
    {
        return Err(Error::Device);
    }
    if !pcr
        .register::<Strap::Register>(Port::EspiSpi, 0xc00c)?
        .is_set(Strap::ENABLE)
    {
        return Ok(Interface::Lpc);
    }
    let second = pcr
        .register::<SoftStraps::Register>(Port::EspiSpi, 0xc210)?
        .is_set(SoftStraps::SECOND_SLAVE);
    let command = unsafe { lpc.register::<u16, PCI_COMMAND_BITS::Register>(0x04) };
    command.modify(PCI_COMMAND_BITS::BUS_MASTER::SET);
    if !command.is_set(PCI_COMMAND_BITS::BUS_MASTER) {
        return Err(Error::Readback);
    }
    let slaves = if second { 2 } else { 1 };
    for slave in 0..slaves {
        let value = transaction(pcr, slave, 0x10, None, delay)?;
        transaction(
            pcr,
            slave,
            0x10,
            Some(value | Channel::BUS_MASTER::SET.value),
            delay,
        )?;
        if transaction(pcr, slave, 0x10, None, delay)? & Channel::BUS_MASTER::SET.value == 0 {
            return Err(Error::Readback);
        }
    }
    Ok(Interface::Espi { slaves })
}
