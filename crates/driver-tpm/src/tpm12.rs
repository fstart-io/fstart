// SPDX-License-Identifier: MIT OR Apache-2.0
//! The TPM 1.2 commands platform firmware sends (TPM Main part 3 and the
//! PC Client TSC ordinals).

use crate::{BUFFER_SIZE, Command, Error, Reader, Transport, execute};
use zerocopy::big_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

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

#[derive(Clone, Copy, IntoBytes, Immutable)]
#[repr(C)]
struct ExtendRequest {
    pcr: U32,
    digest: [u8; 20],
}

#[derive(Clone, Copy, IntoBytes, Immutable)]
#[repr(C)]
struct GetCapabilityRequest {
    cap_area: U32,
    sub_cap_size: U32,
    sub_cap: U32,
}

/// Wire prefix of `TPM_PERMANENT_FLAGS` (TPM 1.2 Main Part 2 §7.1).
#[derive(Clone, Copy, FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct PermanentFlagsWire {
    tag: U16,
    disable: u8,
    ownership: u8,
    deactivated: u8,
    read_pubek: u8,
    disable_owner_clear: u8,
    allow_maintenance: u8,
    physical_presence_lifetime_lock: u8,
    physical_presence_hw_enable: u8,
    physical_presence_cmd_enable: u8,
}

/// Wire prefix of `TPM_STCLEAR_FLAGS` (TPM 1.2 Main Part 2 §7.2).
#[derive(Clone, Copy, FromBytes, KnownLayout, Immutable, Unaligned)]
#[repr(C)]
struct VolatileFlagsWire {
    tag: U16,
    deactivated: u8,
    disable_force_clear: u8,
    physical_presence: u8,
    physical_presence_lock: u8,
}

/// `TPM_Extend` of `pcr` with a SHA-1 digest.
pub fn extend<T: Transport + ?Sized>(
    tpm: &mut T,
    pcr: u32,
    digest: &[u8; 20],
) -> Result<(), Error> {
    run(
        tpm,
        Command::new(TAG_RQU_COMMAND, ORD_EXTEND).write(&ExtendRequest {
            pcr: U32::new(pcr),
            digest: *digest,
        }),
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
    parse: impl FnOnce(&mut Reader<'_>) -> Result<R, Error>,
) -> Result<R, Error> {
    let mut command = Command::new(TAG_RQU_COMMAND, ORD_GET_CAPABILITY);
    command.write(&GetCapabilityRequest {
        cap_area: U32::new(CAP_FLAG),
        sub_cap_size: U32::new(4),
        sub_cap: U32::new(sub),
    });
    let mut response = [0; BUFFER_SIZE];
    let mut body = Reader::new(execute(tpm, command.finish()?, &mut response)?);
    let size = body.u32()? as usize;
    let mut data = Reader::new(body.bytes(size)?);
    parse(&mut data)
}

/// `TPM_GetCapability(TPM_CAP_FLAG_PERMANENT)`.
pub fn permanent_flags<T: Transport + ?Sized>(tpm: &mut T) -> Result<PermanentFlags, Error> {
    flags(tpm, CAP_FLAG_PERMANENT, |data| {
        let wire = data.read::<PermanentFlagsWire>()?;
        if wire.tag.get() != 0x001F {
            return Err(Error::Response);
        }
        Ok(PermanentFlags {
            disable: wire.disable != 0,
            ownership: wire.ownership != 0,
            deactivated: wire.deactivated != 0,
            physical_presence_lifetime_lock: wire.physical_presence_lifetime_lock != 0,
            physical_presence_hw_enable: wire.physical_presence_hw_enable != 0,
            physical_presence_cmd_enable: wire.physical_presence_cmd_enable != 0,
        })
    })
}

/// `TPM_GetCapability(TPM_CAP_FLAG_VOLATILE)`.
pub fn volatile_flags<T: Transport + ?Sized>(tpm: &mut T) -> Result<VolatileFlags, Error> {
    flags(tpm, CAP_FLAG_VOLATILE, |data| {
        let wire = data.read::<VolatileFlagsWire>()?;
        if wire.tag.get() != 0x0020 {
            return Err(Error::Response);
        }
        Ok(VolatileFlags {
            deactivated: wire.deactivated != 0,
            physical_presence: wire.physical_presence != 0,
            physical_presence_lock: wire.physical_presence_lock != 0,
        })
    })
}
