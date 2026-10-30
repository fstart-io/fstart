// SPDX-License-Identifier: MIT OR Apache-2.0
//! The TPM 1.2 commands platform firmware sends (TPM Main part 3 and the
//! PC Client TSC ordinals).

use crate::{BUFFER_SIZE, Command, Error, Reader, Transport, execute};

const TAG_RQU_COMMAND: u16 = 0x00C1;

const ORD_EXTEND: u32 = 0x14;
const ORD_SELF_TEST_FULL: u32 = 0x50;
const ORD_CONTINUE_SELF_TEST: u32 = 0x53;
const ORD_FORCE_CLEAR: u32 = 0x5D;
const ORD_GET_CAPABILITY: u32 = 0x65;
const ORD_PHYSICAL_ENABLE: u32 = 0x6F;
const ORD_PHYSICAL_DISABLE: u32 = 0x70;
const ORD_SET_OWNER_INSTALL: u32 = 0x71;
const ORD_PHYSICAL_SET_DEACTIVATED: u32 = 0x72;
const ORD_STARTUP: u32 = 0x99;
const TSC_ORD_PHYSICAL_PRESENCE: u32 = 0x4000_000A;

const CAP_FLAG: u32 = 4;
const CAP_FLAG_PERMANENT: u32 = 0x108;
const CAP_FLAG_VOLATILE: u32 = 0x109;

/// Return codes firmware reacts to.
pub mod rc {
    pub const DEACTIVATED: u32 = 0x06;
    pub const DISABLED: u32 = 0x07;
    pub const INVALID_POSTINIT: u32 = 0x26;
    pub const BAD_LOCALITY: u32 = 0x3D;
}

/// `TPM_STARTUP_TYPE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum StartupType {
    Clear = 1,
    State = 2,
    Deactivated = 3,
}

/// `TPM_PHYSICAL_PRESENCE` bits for [`physical_presence`].
pub mod pp {
    pub const LOCK: u16 = 0x0004;
    pub const PRESENT: u16 = 0x0008;
    pub const NOTPRESENT: u16 = 0x0010;
    pub const CMD_ENABLE: u16 = 0x0020;
}

/// The `TPM_PERMANENT_FLAGS` firmware looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PermanentFlags {
    pub disable: bool,
    pub ownership: bool,
    pub deactivated: bool,
    pub physical_presence_lifetime_lock: bool,
    pub physical_presence_hw_enable: bool,
    pub physical_presence_cmd_enable: bool,
}

/// The `TPM_STCLEAR_FLAGS` firmware looks at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolatileFlags {
    pub deactivated: bool,
    pub physical_presence: bool,
    pub physical_presence_lock: bool,
}

fn run<T: Transport + ?Sized>(tpm: &mut T, command: &mut Command) -> Result<(), Error> {
    let mut response = [0; BUFFER_SIZE];
    execute(tpm, command.finish()?, &mut response).map(|_| ())
}

/// `TPM_Startup`; a started TPM answers [`rc::INVALID_POSTINIT`].
pub fn startup<T: Transport + ?Sized>(tpm: &mut T, kind: StartupType) -> Result<(), Error> {
    run(
        tpm,
        Command::new(TAG_RQU_COMMAND, ORD_STARTUP).u16(kind as u16),
    )
}

/// `TPM_ContinueSelfTest`.
pub fn continue_self_test<T: Transport + ?Sized>(tpm: &mut T) -> Result<(), Error> {
    run(
        tpm,
        &mut Command::new(TAG_RQU_COMMAND, ORD_CONTINUE_SELF_TEST),
    )
}

/// `TPM_SelfTestFull`.
pub fn self_test_full<T: Transport + ?Sized>(tpm: &mut T) -> Result<(), Error> {
    run(tpm, &mut Command::new(TAG_RQU_COMMAND, ORD_SELF_TEST_FULL))
}

/// `TPM_Extend` of `pcr` with a SHA-1 digest.
pub fn extend<T: Transport + ?Sized>(
    tpm: &mut T,
    pcr: u32,
    digest: &[u8; 20],
) -> Result<(), Error> {
    run(
        tpm,
        Command::new(TAG_RQU_COMMAND, ORD_EXTEND)
            .u32(pcr)
            .bytes(digest),
    )
}

/// `TSC_PhysicalPresence` with a [`pp`] combination.
pub fn physical_presence<T: Transport + ?Sized>(tpm: &mut T, bits: u16) -> Result<(), Error> {
    run(
        tpm,
        Command::new(TAG_RQU_COMMAND, TSC_ORD_PHYSICAL_PRESENCE).u16(bits),
    )
}

/// `TPM_PhysicalEnable` / `TPM_PhysicalDisable` (physical presence).
pub fn physical_enable<T: Transport + ?Sized>(tpm: &mut T, enable: bool) -> Result<(), Error> {
    let ordinal = if enable {
        ORD_PHYSICAL_ENABLE
    } else {
        ORD_PHYSICAL_DISABLE
    };
    run(tpm, &mut Command::new(TAG_RQU_COMMAND, ordinal))
}

/// `TPM_PhysicalSetDeactivated` (physical presence).
pub fn physical_set_deactivated<T: Transport + ?Sized>(
    tpm: &mut T,
    deactivated: bool,
) -> Result<(), Error> {
    run(
        tpm,
        Command::new(TAG_RQU_COMMAND, ORD_PHYSICAL_SET_DEACTIVATED).u8(deactivated.into()),
    )
}

/// `TPM_ForceClear` (physical presence).
pub fn force_clear<T: Transport + ?Sized>(tpm: &mut T) -> Result<(), Error> {
    run(tpm, &mut Command::new(TAG_RQU_COMMAND, ORD_FORCE_CLEAR))
}

/// `TPM_SetOwnerInstall` (physical presence).
pub fn set_owner_install<T: Transport + ?Sized>(tpm: &mut T, allow: bool) -> Result<(), Error> {
    run(
        tpm,
        Command::new(TAG_RQU_COMMAND, ORD_SET_OWNER_INSTALL).u8(allow.into()),
    )
}

fn flags<T: Transport + ?Sized, R>(
    tpm: &mut T,
    sub: u32,
    tag: u16,
    parse: impl FnOnce(&mut Reader<'_>) -> Result<R, Error>,
) -> Result<R, Error> {
    let mut command = Command::new(TAG_RQU_COMMAND, ORD_GET_CAPABILITY);
    command.u32(CAP_FLAG).u32(4).u32(sub);
    let mut response = [0; BUFFER_SIZE];
    let mut body = Reader::new(execute(tpm, command.finish()?, &mut response)?);
    let size = body.u32()? as usize;
    let mut data = Reader::new(body.bytes(size)?);
    if data.u16()? != tag {
        return Err(Error::Response);
    }
    parse(&mut data)
}

/// `TPM_GetCapability(TPM_CAP_FLAG_PERMANENT)`.
pub fn permanent_flags<T: Transport + ?Sized>(tpm: &mut T) -> Result<PermanentFlags, Error> {
    flags(tpm, CAP_FLAG_PERMANENT, 0x001F, |data| {
        let mut b = [false; 9];
        for flag in &mut b {
            *flag = data.u8()? != 0;
        }
        Ok(PermanentFlags {
            disable: b[0],
            ownership: b[1],
            deactivated: b[2],
            physical_presence_lifetime_lock: b[6],
            physical_presence_hw_enable: b[7],
            physical_presence_cmd_enable: b[8],
        })
    })
}

/// `TPM_GetCapability(TPM_CAP_FLAG_VOLATILE)`.
pub fn volatile_flags<T: Transport + ?Sized>(tpm: &mut T) -> Result<VolatileFlags, Error> {
    flags(tpm, CAP_FLAG_VOLATILE, 0x0020, |data| {
        let mut b = [false; 4];
        for flag in &mut b {
            *flag = data.u8()? != 0;
        }
        Ok(VolatileFlags {
            deactivated: b[0],
            physical_presence: b[2],
            physical_presence_lock: b[3],
        })
    })
}
