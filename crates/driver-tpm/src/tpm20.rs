// SPDX-License-Identifier: MIT OR Apache-2.0
//! The TPM 2.0 commands platform firmware sends (TPM 2.0 Library part 3).

use crate::{BUFFER_SIZE, Command, Error, Reader, Transport, alg, execute};
use zeroize::Zeroize;

const ST_NO_SESSIONS: u16 = 0x8001;
const ST_SESSIONS: u16 = 0x8002;

const CC_HIERARCHY_CONTROL: u32 = 0x121;
const CC_PCR_EVENT: u32 = 0x13C;
const CC_CLEAR: u32 = 0x126;
const CC_HIERARCHY_CHANGE_AUTH: u32 = 0x129;
const CC_NV_DEFINE_SPACE: u32 = 0x12A;
const CC_NV_WRITE: u32 = 0x137;
const CC_NV_WRITE_LOCK: u32 = 0x138;
const CC_SELF_TEST: u32 = 0x143;
const CC_STARTUP: u32 = 0x144;
const CC_SHUTDOWN: u32 = 0x145;
const CC_STIR_RANDOM: u32 = 0x146;
const CC_NV_READ: u32 = 0x14E;
const CC_NV_READ_PUBLIC: u32 = 0x169;
const CC_GET_CAPABILITY: u32 = 0x17A;
const CC_GET_RANDOM: u32 = 0x17B;
const CC_PCR_EXTEND: u32 = 0x182;

/// Default platform-hierarchy NV index for fstart firmware rollback floor.
///
/// TCG Registry of Reserved TPM 2.0 Handles and Localities §2.2 allocates
/// `0x0140_0000..=0x017F_FFFF` for platform-defined NV indices (OEM/platform
/// firmware).
pub const ROLLBACK_NV_INDEX: u32 = 0x0140_0001;

/// `TPMA_NV` attribute bits (TPM 2.0 Library Part 2 §13.2).
pub mod nv_attr {
    pub const PPWRITE: u32 = 1 << 0;
    pub const WRITELOCKED: u32 = 1 << 11;
    pub const WRITESTCLEAR: u32 = 1 << 14;
    pub const PPREAD: u32 = 1 << 16;
    pub const AUTHREAD: u32 = 1 << 18;
    pub const WRITTEN: u32 = 1 << 29;
    pub const PLATFORMCREATE: u32 = 1 << 30;

    /// Platform-created NV space, readable/writable by platform hierarchy and
    /// lockable per-boot via `TPM2_NV_WriteLock` (`TPMA_NV_WRITESTCLEAR`).
    pub const ROLLBACK_FLOOR: u32 = PLATFORMCREATE | PPWRITE | PPREAD | AUTHREAD | WRITESTCLEAR;

    /// Runtime state bits set by the TPM on write/write-lock rather than at
    /// `TPM2_NV_DefineSpace` time (`TPMA_NV_WRITTEN | TPMA_NV_WRITELOCKED`).
    pub const RUNTIME_STATE_MASK: u32 = WRITELOCKED | WRITTEN;
}

/// Password authorization session handle.
const RS_PW: u32 = 0x4000_0009;

const CAP_PCRS: u32 = 5;

/// Permanent hierarchy handles (`TPM_RH_*`).
pub mod hierarchy {
    pub const OWNER: u32 = 0x4000_0001;
    pub const LOCKOUT: u32 = 0x4000_000A;
    pub const ENDORSEMENT: u32 = 0x4000_000B;
    pub const PLATFORM: u32 = 0x4000_000C;
    pub const PLATFORM_NV: u32 = 0x4000_000D;
}

/// Response codes firmware reacts to (PFP §3.3.2.1).
pub mod rc {
    pub const INITIALIZE: u32 = 0x100;
    pub const FAILURE: u32 = 0x101;
    pub const BAD_TAG: u32 = 0x01E;
    pub const REBOOT: u32 = 0x130;
    pub const UPGRADE: u32 = 0x12D;
    /// Format-one error codes carry a parameter or handle number in bits
    /// 11:6; the code itself is bits 5:0 plus the format bit.
    pub const fn base(code: u32) -> u32 {
        if code & 0x80 != 0 { code & 0xBF } else { code }
    }
    pub const TESTING: u32 = 0x90A;
    pub const YIELDED: u32 = 0x908;
    pub const RETRY: u32 = 0x922;
    pub const VALUE: u32 = 0x084;
    pub const HANDLE: u32 = 0x08B;
    pub const LOCALITY: u32 = 0x107;
    pub const COMMAND_CODE: u32 = 0x143;
    pub const NV_LOCKED: u32 = 0x148;
    pub const NV_UNINITIALIZED: u32 = 0x14A;
    pub const NV_DEFINED: u32 = 0x14C;
}

/// `TPM_SU`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum StartupType {
    Clear = 0,
    State = 1,
}

/// The allocated PCR banks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Banks {
    algs: [u16; 8],
    /// The PCRs allocated in each bank, bit `n` for PCR `n`.
    pcrs: [u32; 8],
    count: usize,
}

impl Banks {
    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.algs[..self.count].iter().copied()
    }
    /// `(algorithm, allocated PCR mask)` of each bank.
    pub fn allocations(&self) -> impl Iterator<Item = (u16, u32)> + '_ {
        self.algs[..self.count]
            .iter()
            .copied()
            .zip(self.pcrs[..self.count].iter().copied())
    }
    pub fn contains(&self, alg: u16) -> bool {
        self.iter().any(|a| a == alg)
    }
    /// Whether firmware can hash for every allocated bank.
    pub fn all_supported(&self) -> bool {
        self.iter().all(|a| alg::digest_size(a).is_some())
    }
    fn push(&mut self, alg: u16, pcrs: u32) -> Result<(), Error> {
        *self.algs.get_mut(self.count).ok_or(Error::Response)? = alg;
        self.pcrs[self.count] = pcrs;
        self.count += 1;
        Ok(())
    }
}

/// An empty password authorization (`TPMS_AUTH_COMMAND`).
fn password_session(command: &mut Command) {
    command.u32(9).u32(RS_PW).sized(&[]).u8(0).sized(&[]);
}

fn run<T: Transport + ?Sized>(tpm: &mut T, command: &mut Command) -> Result<(), Error> {
    let mut response = [0; BUFFER_SIZE];
    execute(tpm, command.finish()?, &mut response).map(|_| ())
}

/// `TPM2_Startup`. A TPM 1.2 fails it with [`rc::BAD_TAG`]: whatever
/// error it reports (`TPM_BADTAG`, `TPM_BAD_ORDINAL`), it answers with a
/// TPM 1.2 response tag.
pub fn startup<T: Transport + ?Sized>(tpm: &mut T, kind: StartupType) -> Result<(), Error> {
    let mut command = Command::new(ST_NO_SESSIONS, CC_STARTUP);
    command.u16(kind as u16);
    let mut response = [0; BUFFER_SIZE];
    // Only a complete response tells the family: transport errors stay
    // transport errors.
    let result = execute(tpm, command.finish()?, &mut response).map(|_| ());
    if let Ok(()) | Err(Error::Code(_)) = result {
        let tag = u16::from_be_bytes([response[0], response[1]]);
        if tag != ST_NO_SESSIONS && tag != ST_SESSIONS {
            return Err(Error::Code(rc::BAD_TAG));
        }
    }
    result
}

/// `TPM2_Shutdown`.
pub fn shutdown<T: Transport + ?Sized>(tpm: &mut T, kind: StartupType) -> Result<(), Error> {
    run(
        tpm,
        Command::new(ST_NO_SESSIONS, CC_SHUTDOWN).u16(kind as u16),
    )
}

/// `TPM2_SelfTest`; `full` tests everything, otherwise only what is
/// untested. The TPM may run the tests in the background: it then answers
/// `TPM_RC_TESTING`, which means they started, and later commands that need
/// an algorithm under test are retried.
pub fn self_test<T: Transport + ?Sized>(tpm: &mut T, full: bool) -> Result<(), Error> {
    let mut command = Command::new(ST_NO_SESSIONS, CC_SELF_TEST);
    command.u8(full.into());
    let command = command.finish()?;
    let mut response = [0; BUFFER_SIZE];
    for _ in 0..crate::RETRIES {
        let len = tpm.transmit(command, &mut response)?;
        match crate::response_body(&response[..len]) {
            Ok(_) | Err(Error::Code(rc::TESTING)) => return Ok(()),
            Err(Error::Code(rc::RETRY | rc::YIELDED)) => tpm.wait_ms(10),
            Err(error) => return Err(error),
        }
    }
    Err(Error::Timeout)
}

/// The PCR banks with at least one PCR allocated (`TPM2_GetCapability`,
/// `TPM_CAP_PCRS`).
pub fn pcr_banks<T: Transport + ?Sized>(tpm: &mut T) -> Result<Banks, Error> {
    let mut command = Command::new(ST_NO_SESSIONS, CC_GET_CAPABILITY);
    command.u32(CAP_PCRS).u32(0).u32(1);
    let mut response = [0; BUFFER_SIZE];
    let mut body = Reader::new(execute(tpm, command.finish()?, &mut response)?);
    let _more = body.u8()?;
    if body.u32()? != CAP_PCRS {
        return Err(Error::Response);
    }
    let mut banks = Banks::default();
    for _ in 0..body.u32()? {
        let hash = body.u16()?;
        let size = body.u8()?;
        let select = body.bytes(size.into())?;
        // pcrSelect: bit n of byte m is PCR 8m + n; firmware's PCRs are
        // all below 32.
        let pcrs = select
            .iter()
            .take(4)
            .enumerate()
            .fold(0u32, |mask, (i, &bits)| mask | u32::from(bits) << (8 * i));
        if pcrs != 0 {
            banks.push(hash, pcrs)?;
        }
    }
    Ok(banks)
}

/// `TPM2_PCR_Extend` with one digest per bank, as `(algorithm, digest)`.
pub fn pcr_extend<T: Transport + ?Sized>(
    tpm: &mut T,
    pcr: u32,
    digests: &[(u16, &[u8])],
) -> Result<(), Error> {
    let mut command = Command::new(ST_SESSIONS, CC_PCR_EXTEND);
    command.u32(pcr);
    password_session(&mut command);
    command.u32(digests.len() as u32);
    for (alg, digest) in digests {
        command.u16(*alg).bytes(digest);
    }
    run(tpm, &mut command)
}

/// `TPM2_PCR_Event`: the TPM hashes `data` for every allocated bank, also
/// those firmware cannot hash itself, and extends `pcr`.
pub fn pcr_event<T: Transport + ?Sized>(tpm: &mut T, pcr: u32, data: &[u8]) -> Result<(), Error> {
    let mut command = Command::new(ST_SESSIONS, CC_PCR_EVENT);
    command.u32(pcr);
    password_session(&mut command);
    command.sized(data);
    run(tpm, &mut command)
}

/// `TPM2_HierarchyChangeAuth` of a hierarchy whose authorization is empty.
pub fn hierarchy_change_auth<T: Transport + ?Sized>(
    tpm: &mut T,
    hierarchy: u32,
    new_auth: &[u8],
) -> Result<(), Error> {
    let mut command = Command::new(ST_SESSIONS, CC_HIERARCHY_CHANGE_AUTH);
    command.u32(hierarchy);
    password_session(&mut command);
    command.sized(new_auth);
    run(tpm, &mut command)
}

/// `TPM2_HierarchyControl`: enable or disable `enable` (a hierarchy handle
/// or [`hierarchy::PLATFORM_NV`]) with `auth`'s empty authorization.
pub fn hierarchy_control<T: Transport + ?Sized>(
    tpm: &mut T,
    auth: u32,
    enable: u32,
    state: bool,
) -> Result<(), Error> {
    let mut command = Command::new(ST_SESSIONS, CC_HIERARCHY_CONTROL);
    command.u32(auth);
    password_session(&mut command);
    command.u32(enable).u8(state.into());
    run(tpm, &mut command)
}

/// `TPM2_Clear` with the empty authorization of `auth` (platform or
/// lockout).
pub fn clear<T: Transport + ?Sized>(tpm: &mut T, auth: u32) -> Result<(), Error> {
    let mut command = Command::new(ST_SESSIONS, CC_CLEAR);
    command.u32(auth);
    password_session(&mut command);
    run(tpm, &mut command)
}

/// `TPM2_StirRandom`.
pub fn stir_random<T: Transport + ?Sized>(tpm: &mut T, entropy: &[u8]) -> Result<(), Error> {
    run(
        tpm,
        Command::new(ST_NO_SESSIONS, CC_STIR_RANDOM).sized(entropy),
    )
}

/// Fill `out` from `TPM2_GetRandom`.
pub fn get_random<T: Transport + ?Sized>(tpm: &mut T, out: &mut [u8]) -> Result<(), Error> {
    let mut filled = 0;
    while filled < out.len() {
        let want = (out.len() - filled).min(32);
        let mut command = Command::new(ST_NO_SESSIONS, CC_GET_RANDOM);
        command.u16(want as u16);
        let mut response = [0; BUFFER_SIZE];
        let result = execute(tpm, command.finish()?, &mut response).and_then(|body| {
            let mut body = Reader::new(body);
            let size = usize::from(body.u16()?).min(want);
            if size == 0 {
                return Err(Error::Response);
            }
            out[filled..filled + size].copy_from_slice(body.bytes(size)?);
            Ok(size)
        });
        response.zeroize();
        filled += result?;
    }
    Ok(())
}

/// `TPM2_NV_DefineSpace` under `auth_handle` (typically [`hierarchy::PLATFORM`])
/// with empty authorization and empty `authPolicy`.
pub fn nv_define_space<T: Transport + ?Sized>(
    tpm: &mut T,
    auth_handle: u32,
    nv_index: u32,
    attributes: u32,
    data_size: u16,
) -> Result<(), Error> {
    let mut command = Command::new(ST_SESSIONS, CC_NV_DEFINE_SPACE);
    command.u32(auth_handle);
    password_session(&mut command);
    // auth: empty TPM2B_AUTH
    command.sized(&[]);
    // publicInfo: TPM2B_NV_PUBLIC (14 bytes: nvIndex(4) + nameAlg(2) +
    // attributes(4) + authPolicy(2) + dataSize(2)).
    command
        .u16(14)
        .u32(nv_index)
        .u16(alg::SHA256)
        .u32(attributes)
        .sized(&[])
        .u16(data_size);
    run(tpm, &mut command)
}

/// Public metadata returned by `TPM2_NV_ReadPublic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NvPublic {
    pub nv_index: u32,
    pub name_alg: u16,
    pub attributes: u32,
    pub data_size: u16,
}

/// Result of [`advance_rollback_floor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorAdvance {
    /// NV floor was already `>= requested`.
    AlreadyAtOrAbove(u64),
    /// NV floor was advanced and read back as this value.
    Advanced(u64),
}

/// `TPM2_NV_ReadPublic`: read and parse `TPMS_NV_PUBLIC` for `nv_index`.
pub fn nv_read_public<T: Transport + ?Sized>(
    tpm: &mut T,
    nv_index: u32,
) -> Result<NvPublic, Error> {
    let mut command = Command::new(ST_NO_SESSIONS, CC_NV_READ_PUBLIC);
    command.u32(nv_index);
    let mut response = [0; BUFFER_SIZE];
    let body = execute(tpm, command.finish()?, &mut response)?;
    let mut reader = Reader::new(body);
    let pub_size = usize::from(reader.u16()?);
    let mut pub_reader = Reader::new(reader.bytes(pub_size)?);
    let index = pub_reader.u32()?;
    let name_alg = pub_reader.u16()?;
    let attributes = pub_reader.u32()?;
    let policy_len = usize::from(pub_reader.u16()?);
    let _ = pub_reader.bytes(policy_len)?;
    let data_size = pub_reader.u16()?;
    Ok(NvPublic {
        nv_index: index,
        name_alg,
        attributes,
        data_size,
    })
}

/// `TPM2_NV_Read`: read `out.len()` bytes starting at `offset` from `nv_index`.
pub fn nv_read<T: Transport + ?Sized>(
    tpm: &mut T,
    auth_handle: u32,
    nv_index: u32,
    offset: u16,
    out: &mut [u8],
) -> Result<(), Error> {
    let size = u16::try_from(out.len()).map_err(|_| Error::Response)?;
    let mut command = Command::new(ST_SESSIONS, CC_NV_READ);
    command.u32(auth_handle).u32(nv_index);
    password_session(&mut command);
    command.u16(size).u16(offset);
    let mut response = [0; BUFFER_SIZE];
    let body = execute(tpm, command.finish()?, &mut response)?;
    let mut reader = Reader::new(body);
    let param_size = reader.u32()? as usize;
    let params = reader.bytes(param_size)?;
    let mut params = Reader::new(params);
    let data_len = usize::from(params.u16()?);
    if data_len != out.len() {
        return Err(Error::Response);
    }
    let bytes = params.bytes(data_len)?;
    out.copy_from_slice(bytes);
    if u16::from_be_bytes([response[0], response[1]]) != ST_SESSIONS {
        return Err(Error::Response);
    }
    Ok(())
}

/// `TPM2_NV_Write`: write `data` starting at `offset` into `nv_index`.
pub fn nv_write<T: Transport + ?Sized>(
    tpm: &mut T,
    auth_handle: u32,
    nv_index: u32,
    offset: u16,
    data: &[u8],
) -> Result<(), Error> {
    let mut command = Command::new(ST_SESSIONS, CC_NV_WRITE);
    command.u32(auth_handle).u32(nv_index);
    password_session(&mut command);
    command.sized(data).u16(offset);
    run(tpm, &mut command)
}

/// `TPM2_NV_WriteLock`: lock `nv_index` against further writes until the next
/// TPM reset/restart (requires `TPMA_NV_WRITESTCLEAR` on the index).
pub fn nv_write_lock<T: Transport + ?Sized>(
    tpm: &mut T,
    auth_handle: u32,
    nv_index: u32,
) -> Result<(), Error> {
    let mut command = Command::new(ST_SESSIONS, CC_NV_WRITE_LOCK);
    command.u32(auth_handle).u32(nv_index);
    password_session(&mut command);
    run(tpm, &mut command)
}

/// Explicit one-time factory/provisioning step: define the 8-byte platform
/// rollback-floor NV index and write `initial_floor`.
///
/// Never call this as a silent fallback on normal boots of a provisioned
/// device—a missing NV index on a provisioned board must fail closed into
/// recovery.
pub fn provision_rollback_floor<T: Transport + ?Sized>(
    tpm: &mut T,
    nv_index: u32,
    initial_floor: u64,
) -> Result<u64, Error> {
    nv_define_space(
        tpm,
        hierarchy::PLATFORM,
        nv_index,
        nv_attr::ROLLBACK_FLOOR,
        8,
    )?;
    nv_write(
        tpm,
        hierarchy::PLATFORM,
        nv_index,
        0,
        &initial_floor.to_le_bytes(),
    )?;
    let readback = read_rollback_floor(tpm, nv_index)?;
    if readback != initial_floor {
        return Err(Error::Response);
    }
    Ok(readback)
}

/// Verify the rollback-floor NV index's static public attributes match
/// [`nv_attr::ROLLBACK_FLOOR`] (ignoring only the TPM-managed runtime state
/// bits [`nv_attr::RUNTIME_STATE_MASK`], while still rejecting any extra
/// OS-writable bits such as `AUTHWRITE`/`OWNERWRITE`) and read its
/// little-endian `u64` floor.
pub fn read_rollback_floor<T: Transport + ?Sized>(
    tpm: &mut T,
    nv_index: u32,
) -> Result<u64, Error> {
    let public = nv_read_public(tpm, nv_index)?;
    if public.nv_index != nv_index
        || public.data_size != 8
        || (public.attributes & !nv_attr::RUNTIME_STATE_MASK) != nv_attr::ROLLBACK_FLOOR
    {
        return Err(Error::Response);
    }
    let mut buf = [0u8; 8];
    nv_read(tpm, hierarchy::PLATFORM, nv_index, 0, &mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

/// Monotonically advance `nv_index` to `new_floor` (refusing any decrease) and
/// verify the readback value.
pub fn advance_rollback_floor<T: Transport + ?Sized>(
    tpm: &mut T,
    nv_index: u32,
    new_floor: u64,
) -> Result<FloorAdvance, Error> {
    let current = read_rollback_floor(tpm, nv_index)?;
    if new_floor <= current {
        return Ok(FloorAdvance::AlreadyAtOrAbove(current));
    }
    nv_write(
        tpm,
        hierarchy::PLATFORM,
        nv_index,
        0,
        &new_floor.to_le_bytes(),
    )?;
    let readback = read_rollback_floor(tpm, nv_index)?;
    if readback != new_floor {
        return Err(Error::Response);
    }
    Ok(FloorAdvance::Advanced(readback))
}

/// Lock the rollback-floor NV index against writes until the next TPM reset.
pub fn lock_rollback_floor<T: Transport + ?Sized>(tpm: &mut T, nv_index: u32) -> Result<(), Error> {
    nv_write_lock(tpm, hierarchy::PLATFORM, nv_index)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers each command with the next scripted (tag, code, body).
    struct Script(&'static [(u16, u32, &'static [u8])], usize);

    impl Transport for Script {
        fn transmit(&mut self, _: &[u8], response: &mut [u8]) -> Result<usize, Error> {
            let (tag, code, body) = self.0[self.1];
            self.1 += 1;
            let len = 10 + body.len();
            response[..2].copy_from_slice(&tag.to_be_bytes());
            response[2..6].copy_from_slice(&(len as u32).to_be_bytes());
            response[6..10].copy_from_slice(&code.to_be_bytes());
            response[10..len].copy_from_slice(body);
            Ok(len)
        }
        fn wait_ms(&mut self, _: u32) {}
    }

    #[test]
    fn a_tpm12_fails_startup_with_bad_tag() {
        // TPM_BAD_ORDINAL with the TPM 1.2 response tag.
        let mut tpm = Script(&[(0x00C4, 0x0A, &[])], 0);
        assert_eq!(
            startup(&mut tpm, StartupType::Clear),
            Err(Error::Code(rc::BAD_TAG))
        );
    }

    #[test]
    fn testing_ends_a_self_test_and_is_retried_after() {
        let mut tpm = Script(&[(ST_NO_SESSIONS, rc::TESTING, &[])], 0);
        assert_eq!(self_test(&mut tpm, false), Ok(()));
        // SHA-1 bank empty, SHA-256 bank with all PCRs.
        const CAPS: &[u8] = &[
            0, 0, 0, 0, 5, 0, 0, 0, 2, 0, 0x04, 3, 0, 0, 0, 0, 0x0B, 3, 0xFF, 0xFF, 0xFF,
        ];
        let mut tpm = Script(
            &[
                (ST_NO_SESSIONS, rc::TESTING, &[]),
                (ST_NO_SESSIONS, 0, CAPS),
            ],
            0,
        );
        let banks = pcr_banks(&mut tpm).unwrap();
        assert_eq!(tpm.1, 2, "retried once");
        assert!(banks.contains(alg::SHA256) && !banks.contains(alg::SHA1));
        assert_eq!(banks.allocations().next(), Some((alg::SHA256, 0x00FF_FFFF)));
    }

    #[test]
    fn nv_read_write_lock_and_monotonic_rollback_floor() {
        // TPM2B_NV_PUBLIC: size(2)=14, index(4)=0x01400001, nameAlg(2)=SHA256(0x000B),
        // attrs(4)=ROLLBACK_FLOOR|WRITTEN(0x60054001), authPolicy(2)=0, dataSize(2)=8.
        const PUB_WRITTEN: &[u8] = &[
            0, 14, 0x01, 0x40, 0x00, 0x01, 0x00, 0x0B, 0x60, 0x05, 0x40, 0x01, 0, 0, 0, 8,
        ];
        // After TPM2_NV_WriteLock: ROLLBACK_FLOOR|WRITTEN|WRITELOCKED(0x60054801).
        const PUB_LOCKED: &[u8] = &[
            0, 14, 0x01, 0x40, 0x00, 0x01, 0x00, 0x0B, 0x60, 0x05, 0x48, 0x01, 0, 0, 0, 8,
        ];
        // parameterSize(4) = 10, TPM2B length(2) = 8, payload(8) = 5u64 LE.
        const READ_5: &[u8] = &[0, 0, 0, 10, 0, 8, 5, 0, 0, 0, 0, 0, 0, 0];
        const READ_9: &[u8] = &[0, 0, 0, 10, 0, 8, 9, 0, 0, 0, 0, 0, 0, 0];

        let mut tpm = Script(
            &[
                // advance_rollback_floor(9): read_public + read(5), write(9), read_public + readback(9)
                (ST_NO_SESSIONS, 0, PUB_WRITTEN),
                (ST_SESSIONS, 0, READ_5),
                (ST_SESSIONS, 0, &[]),
                (ST_NO_SESSIONS, 0, PUB_WRITTEN),
                (ST_SESSIONS, 0, READ_9),
                // advance_rollback_floor(7) refuses decrease: read_public + read(9) only
                (ST_NO_SESSIONS, 0, PUB_WRITTEN),
                (ST_SESSIONS, 0, READ_9),
                // lock_rollback_floor
                (ST_SESSIONS, 0, &[]),
                // read_rollback_floor still succeeds after write-lock (WRITELOCKED bit set)
                (ST_NO_SESSIONS, 0, PUB_LOCKED),
                (ST_SESSIONS, 0, READ_9),
            ],
            0,
        );

        assert_eq!(
            advance_rollback_floor(&mut tpm, ROLLBACK_NV_INDEX, 9),
            Ok(FloorAdvance::Advanced(9))
        );
        assert_eq!(
            advance_rollback_floor(&mut tpm, ROLLBACK_NV_INDEX, 7),
            Ok(FloorAdvance::AlreadyAtOrAbove(9))
        );
        assert_eq!(lock_rollback_floor(&mut tpm, ROLLBACK_NV_INDEX), Ok(()));
        assert_eq!(read_rollback_floor(&mut tpm, ROLLBACK_NV_INDEX), Ok(9));
        assert_eq!(tpm.1, 10);
    }

    #[test]
    fn read_rollback_floor_rejects_wrong_attributes() {
        // Missing PLATFORMCREATE (attrs = 0x20054001 with WRITTEN set).
        const PUB_MISSING: &[u8] = &[
            0, 14, 0x01, 0x40, 0x00, 0x01, 0x00, 0x0B, 0x20, 0x05, 0x40, 0x01, 0, 0, 0, 8,
        ];
        let mut tpm = Script(&[(ST_NO_SESSIONS, 0, PUB_MISSING)], 0);
        assert_eq!(
            read_rollback_floor(&mut tpm, ROLLBACK_NV_INDEX),
            Err(Error::Response)
        );
        // Superset with AUTHWRITE (bit 2 = 0x04 -> 0x60054005 with WRITTEN set) must be rejected!
        const PUB_SUPERSET: &[u8] = &[
            0, 14, 0x01, 0x40, 0x00, 0x01, 0x00, 0x0B, 0x60, 0x05, 0x40, 0x05, 0, 0, 0, 8,
        ];
        let mut tpm = Script(&[(ST_NO_SESSIONS, 0, PUB_SUPERSET)], 0);
        assert_eq!(
            read_rollback_floor(&mut tpm, ROLLBACK_NV_INDEX),
            Err(Error::Response)
        );
    }

    #[test]
    fn provision_rollback_floor_defines_writes_and_verifies() {
        const PUB_WRITTEN: &[u8] = &[
            0, 14, 0x01, 0x40, 0x00, 0x01, 0x00, 0x0B, 0x60, 0x05, 0x40, 0x01, 0, 0, 0, 8,
        ];
        const READ_3: &[u8] = &[0, 0, 0, 10, 0, 8, 3, 0, 0, 0, 0, 0, 0, 0];
        let mut tpm = Script(
            &[
                (ST_SESSIONS, 0, &[]),
                (ST_SESSIONS, 0, &[]),
                (ST_NO_SESSIONS, 0, PUB_WRITTEN),
                (ST_SESSIONS, 0, READ_3),
            ],
            0,
        );
        assert_eq!(
            provision_rollback_floor(&mut tpm, ROLLBACK_NV_INDEX, 3),
            Ok(3)
        );
        assert_eq!(tpm.1, 4);
    }
}
