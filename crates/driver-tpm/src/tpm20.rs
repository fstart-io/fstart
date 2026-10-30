// SPDX-License-Identifier: MIT OR Apache-2.0
//! The TPM 2.0 commands platform firmware sends (TPM 2.0 Library part 3).

use crate::{BUFFER_SIZE, Command, Error, Reader, Transport, alg, execute, zeroize};

const ST_NO_SESSIONS: u16 = 0x8001;
const ST_SESSIONS: u16 = 0x8002;

const CC_HIERARCHY_CONTROL: u32 = 0x121;
const CC_PCR_EVENT: u32 = 0x13C;
const CC_CLEAR: u32 = 0x126;
const CC_HIERARCHY_CHANGE_AUTH: u32 = 0x129;
const CC_SELF_TEST: u32 = 0x143;
const CC_STARTUP: u32 = 0x144;
const CC_SHUTDOWN: u32 = 0x145;
const CC_STIR_RANDOM: u32 = 0x146;
const CC_GET_CAPABILITY: u32 = 0x17A;
const CC_GET_RANDOM: u32 = 0x17B;
const CC_PCR_EXTEND: u32 = 0x182;

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
    pub const LOCALITY: u32 = 0x107;
    pub const COMMAND_CODE: u32 = 0x143;
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
        zeroize(&mut response);
        filled += result?;
    }
    Ok(())
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
}
