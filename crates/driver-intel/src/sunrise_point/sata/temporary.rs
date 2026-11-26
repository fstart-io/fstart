// SPDX-License-Identifier: MIT OR Apache-2.0
//! SATA-specific lock preflight and typed view of the shared BAR lease.
//! Cleanup restores resources, not irreversible SIR/capability/COMRESET work.
use super::{Error, registers::*};
use crate::{generic::temporary_bar, southbridge::ahci::AhciRegs};
use fstart_pci::EcamDevice;
use tock_registers::interfaces::Readable;
pub(super) type Temporary = temporary_bar::TemporaryBar<AhciRegs<8>>;
impl From<temporary_bar::Error> for Error {
    fn from(error: temporary_bar::Error) -> Self {
        match error {
            temporary_bar::Error::Readback => Self::Readback,
            _ => Self::Resource,
        }
    }
}
pub(super) fn preflight(device: &EcamDevice) -> Result<u16, Error> {
    let command = device.read16(4);
    // No foreign decode/DMA owner and no all-ones sizing probe.
    if device.read32(0x24) != 0 || command & 7 != 0 {
        return Err(Error::Resource);
    }
    if unsafe { device.register::<u8, RegisterLock::Register>(0x9f) }.is_set(RegisterLock::LOCK)
        || unsafe { device.register::<u8, Decode::Register>(0x9c) }.is_set(Decode::INIT)
    {
        return Err(Error::Locked);
    }
    Ok(command)
}
