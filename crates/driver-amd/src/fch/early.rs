// SPDX-License-Identifier: MIT OR Apache-2.0
//! Family 17h FCH pre-console mechanisms shared by Picasso and Zeppelin:
//! LPC decodes and SPI BAR, SMBus, legacy/CF9 decode and the power-failure
//! state.
use super::{
    Bus, Decode, Fch, LpcGating, PM_DECODE, PM_LPC_GATING, PM_RTC_SHADOW, RtcShadow, SPI_BASE, asf,
    smbus,
};
use crate::fch::lpc::{self, Id, IoMemDecode, SpiBase};
use tock_registers::{
    fields::FieldValue,
    interfaces::{ReadWriteable, Readable, Writeable},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Unavailable,
    WrongVendor,
    WrongSpiBase,
    SpiBusy,
}
pub(crate) fn lpc_present(fch: &Fch<impl Bus>) -> Result<(), Error> {
    let id = fch.bus().reg(lpc::ID).extract();
    if id.get() == u32::MAX {
        return Err(Error::Unavailable);
    }
    if !id.matches_all(Id::VENDOR::Amd) {
        return Err(Error::WrongVendor);
    }
    Ok(())
}
/// Enable LPC, clear its legacy decodes/ranges, establish the fixed SPI/eSPI
/// aperture. Call BEFORE new cold eSPI configuration, not over an active link.
/// No SPI mode/clock changes here. Existing BAR low-byte flags are retained.
pub fn prepare_lpc(fch: &Fch<impl Bus>) -> Result<(), Error> {
    lpc_present(fch)?;
    let bus = fch.bus();
    let mut gating = fch.reg(PM_LPC_GATING).extract();
    let decode = bus.reg(lpc::IO_MEM_DECODE).extract();
    let mut bar = bus.reg(lpc::SPI_BASE_ADDRESS).extract();
    if gating.get() == u8::MAX || decode.get() == u32::MAX || bar.get() == u32::MAX {
        return Err(Error::Unavailable);
    }
    gating.modify(LpcGating::LPC_ENABLE::SET);
    fch.reg(PM_LPC_GATING).set(gating.get());
    // lpc_disable_decodes: only the sync timeout stays. The vendor code
    // writes IO_PORT_DECODE twice.
    bus.reg(lpc::IO_PORT_DECODE).set(0);
    bus.reg(lpc::IO_MEM_DECODE).write(
        IoMemDecode::SYNC_TIMEOUT_EN.val(decode.read(IoMemDecode::SYNC_TIMEOUT_EN))
            + IoMemDecode::SYNC_TIMEOUT_COUNT.val(decode.read(IoMemDecode::SYNC_TIMEOUT_COUNT)),
    );
    bus.reg(lpc::IO_PORT_DECODE).set(0);
    bus.reg(lpc::MEM_PORT1).set(0);
    bus.reg(lpc::MEM_PORT0).set(0);
    bus.reg(lpc::WIDEIO2_GENERIC_PORT).set(0);
    bar.modify(SpiBase::BASE.val(SPI_BASE >> 8));
    bus.reg(lpc::SPI_BASE_ADDRESS).set(bar.get());
    Ok(())
}

/// SMBus registers of the SMBus and ASF controllers.
const SMB_HOST_STATUS: u32 = 0x00;
const SMB_SLAVE_STATUS: u32 = 0x01;
const SMB_TIMING: u32 = 0x0e;
/// Native nominal 400-kHz divider 41 (66MHz/(400kHz*4), integer truncation).
const SMB_SPEED_400KHZ: u8 = 41;

/// The SMBus/ASF decode at [`super::SMBUS_ASF_IO_BASE`], the nominal 400 kHz
/// and the status cleared. This preserves native timing; it does not attest a
/// measured 400-kHz ceiling.
pub fn initialize_smbus(fch: &Fch<impl Bus>) -> Result<(), Error> {
    let mut decode = fch.reg(PM_DECODE).extract();
    if decode.get() == u32::MAX {
        return Err(Error::Unavailable);
    }
    decode.modify(
        Decode::SMBUS_ASF_IO_BASE.val(u32::from(super::SMBUS_ASF_IO_BASE >> 8))
            + Decode::SMBUS_ASF_IO::SET,
    );
    fch.reg(PM_DECODE).set(decode.get());
    fch.reg(smbus::at::<u8, ()>(SMB_TIMING))
        .set(SMB_SPEED_400KHZ);
    for (offset, clear) in [(SMB_HOST_STATUS, 0xff), (SMB_SLAVE_STATUS, 0x1f)] {
        fch.reg(smbus::at::<u8, ()>(offset)).set(clear);
    }
    for (offset, clear) in [(SMB_HOST_STATUS, 0xff), (SMB_SLAVE_STATUS, 0x1f)] {
        fch.reg(asf::at::<u8, ()>(offset)).set(clear);
    }
    Ok(())
}
fn modify_decode(fch: &Fch<impl Bus>, field: FieldValue<u32, Decode::Register>) {
    fch.reg(PM_DECODE).modify(field);
}
/// fch_enable_cf9_io: the CF9 reset port.
pub fn enable_cf9_io(fch: &Fch<impl Bus>) {
    modify_decode(fch, Decode::CF9::SET);
}
/// fch_enable_legacy_io: the legacy I/O ranges (PIC, PIT, RTC, ...).
pub fn enable_legacy_io(fch: &Fch<impl Bus>) {
    modify_decode(fch, Decode::LEGACY::SET);
}

/// What the system does when AC power returns (board policy).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerFailure {
    Off,
    On,
    Previous,
}
/// pm_set_power_failure_state.
pub fn set_power_failure_state(fch: &Fch<impl Bus>, state: PowerFailure) {
    let policy = match state {
        PowerFailure::Off => RtcShadow::POWER_FAILURE::Off,
        PowerFailure::On => RtcShadow::POWER_FAILURE::On,
        PowerFailure::Previous => RtcShadow::POWER_FAILURE::Previous,
    };
    fch.reg(PM_RTC_SHADOW)
        .write(policy + RtcShadow::POWER_STATE::SET);
}
