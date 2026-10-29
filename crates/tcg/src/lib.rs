// SPDX-License-Identifier: MIT OR Apache-2.0
//! TCG measured boot for TPM 1.2 and TPM 2.0.
//!
//! The static root of trust (the first fstart stage running from flash)
//! starts the TPM with [`start`], records its own measurements and those of
//! everything it hands control to with [`measure`] and friends, and leaves
//! one TCG event log behind for the payload and the OS:
//!
//! - TPM 2.0: crypto-agile log, all allocated banks (PC Client Platform
//!   Firmware Profile 1.06).
//! - TPM 1.2: SHA-1 log (TCG EFI Platform 1.22 / Conventional BIOS 1.21).
//!
//! PCR usage follows the PFP for both families. fstart itself measures into
//! PCR 0 (its code: S-CRTM, SMM, payload firmware) and, booting Linux
//! directly, PCR 4 (the kernel) and 9 (its command line); it measures no
//! configuration into PCR 1-3 yet, so those hold only the separators
//! unless the payload measures more.
//!
//! Who finishes the pre-OS phase depends on the payload (see [`Handoff`]):
//! a coreboot-style payload (SeaBIOS) continues the log, extends the
//! separators and locks the TPM itself, as it does on coreboot; before a
//! UEFI or Linux payload fstart locks the TPM itself.
//!
//! Without [`start`], or after a TPM failure, every call is a no-op.
#![no_std]

pub mod event;
pub mod log;

use core::sync::atomic::{AtomicBool, Ordering};
use event::{EV_SEPARATOR, EventData};
pub use fstart_driver_tpm::{Error as TpmError, Family, Transport};
use fstart_driver_tpm::{tpm12, tpm20};
use log::{Digests, EventLog, MAX_BANKS};
pub use log::{Format, PlatformClass};

/// Measured-boot state, owned by the boot CPU.
struct State {
    tpm: &'static mut dyn Transport,
    family: Family,
    /// `(algorithm, allocated PCR mask)` of each bank.
    banks: [(u16, u32); MAX_BANKS],
    bank_count: usize,
    log: EventLog,
    /// A measurement failed and PCR 0-7 are capped; nothing more is
    /// measured.
    failed: bool,
    /// The TPM is locked against later firmware (see [`lock`]).
    locked: bool,
    /// A PCR could not be capped after a failure: PCR 0-7 may look valid
    /// without a complete measurement chain (PFP §3.3.2.2 (3)).
    cap_failed: bool,
}

static mut STATE: Option<State> = None;
/// Taken while [`STATE`] is borrowed: a second, concurrent or reentrant
/// access (another CPU, a transport calling back) finds it taken and does
/// nothing instead of aliasing the state.
static BUSY: AtomicBool = AtomicBool::new(false);

fn exclusive<R>(f: impl FnOnce(&mut Option<State>) -> R) -> Option<R> {
    if BUSY.swap(true, Ordering::Acquire) {
        return None;
    }
    // SAFETY: BUSY grants the only reference to STATE until it is cleared;
    // none escapes the closure.
    let result = f(unsafe { &mut *core::ptr::addr_of_mut!(STATE) });
    BUSY.store(false, Ordering::Release);
    Some(result)
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    exclusive(|state| state.as_mut().map(f)).flatten()
}

/// Why measured boot is off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartError {
    /// A TPM command failed while starting (PFP §3.3.2.1); the TPM is left
    /// alone.
    Startup(&'static str, TpmError),
    /// A TPM 1.2 is disabled or deactivated by its owner.
    Inactive,
    /// A TPM 2.0 has a PCR bank firmware cannot hash.
    UnsupportedBank(u16),
    /// A TPM 2.0 has no PCR allocated.
    NoBank,
    /// The log buffer cannot hold the first event.
    LogFull,
    /// Measured boot failed after the TPM started, and the TPM could not be
    /// hidden or locked either: an OS could use it unmeasured.
    Unprotected,
    /// Measured boot already started.
    Started,
}

/// What PFP §3.3.2.1 asks of the platform after a [`StartError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Remedy {
    /// Keep the TPM hidden from the OS: no ACPI device or table.
    Hide,
    /// The TPM was started from the wrong locality or with a wrong type:
    /// reset the platform and start it again. A platform that cannot tell
    /// a repeated failure apart hides the TPM instead of reset looping.
    Reset,
    /// Nothing: the owner disabled the TPM (TPM 1.2), the OS may still
    /// see it.
    None,
    /// The TPM is usable but unprotected: do not boot (PFP §3.3.2.2 (3)).
    Halt,
}

impl StartError {
    pub fn remedy(&self) -> Remedy {
        match *self {
            Self::Startup(_, TpmError::Code(code))
                if matches!(
                    tpm20::rc::base(code),
                    tpm20::rc::VALUE | tpm20::rc::LOCALITY
                ) =>
            {
                Remedy::Reset
            }
            Self::Inactive | Self::Started => Remedy::None,
            Self::Unprotected => Remedy::Halt,
            _ => Remedy::Hide,
        }
    }

    /// Print the reason to the firmware log.
    pub fn report(&self) {
        match *self {
            Self::Startup(command, TpmError::Code(code)) => {
                fstart_log::warn!("tcg: TPM {} failed with code {:#x}", command, code)
            }
            Self::Startup(command, TpmError::Timeout) => {
                fstart_log::warn!("tcg: TPM {} timed out", command)
            }
            Self::Startup(command, TpmError::Locality) => {
                fstart_log::warn!("tcg: TPM {}: locality not granted", command)
            }
            Self::Startup(command, _) => fstart_log::warn!("tcg: TPM {}: interface error", command),
            Self::Inactive => fstart_log::warn!("tcg: TPM 1.2 is disabled or deactivated"),
            Self::UnsupportedBank(alg) => {
                fstart_log::warn!(
                    "tcg: TPM 2.0 has a PCR bank of unsupported algorithm {:#x}",
                    alg
                )
            }
            Self::LogFull => fstart_log::warn!("tcg: event log area too small"),
            Self::NoBank => fstart_log::warn!("tcg: TPM 2.0 has no PCR bank allocated"),
            Self::Unprotected => {
                fstart_log::error!("tcg: the TPM could not be hidden or locked")
            }
            Self::Started => fstart_log::warn!("tcg: measured boot already started"),
        }
    }
}

fn startup(tpm: &mut dyn Transport) -> Result<Family, StartError> {
    match tpm20::startup(tpm, tpm20::StartupType::Clear) {
        // Already started by earlier firmware: PFP §3.3.2.1 (1).
        Ok(()) | Err(TpmError::Code(tpm20::rc::INITIALIZE)) => Ok(Family::Tpm20),
        // A TPM 1.2 does not know the TPM 2.0 tag: PFP §3.3.2.1 (5).
        Err(TpmError::Code(tpm20::rc::BAD_TAG)) => {
            match tpm12::startup(tpm, tpm12::StartupType::Clear) {
                Ok(()) | Err(TpmError::Code(tpm12::rc::INVALID_POSTINIT)) => Ok(Family::Tpm12),
                Err(error) => Err(StartError::Startup("TPM_Startup", error)),
            }
        }
        Err(error) => Err(StartError::Startup("TPM2_Startup", error)),
    }
}

/// TPM 1.2: assert physical presence through the TSC command interface
/// unless it was locked for the lifetime of the TPM, and refuse a disabled
/// or deactivated TPM, which cannot extend PCRs.
fn prepare_tpm12(tpm: &mut dyn Transport) -> Result<(), StartError> {
    // First: a TPM may refuse other commands until its self test ran
    // (TPM_NEEDS_SELFTEST); ContinueSelfTest is always available.
    tpm12::continue_self_test(tpm).map_err(|e| StartError::Startup("TPM_ContinueSelfTest", e))?;
    let flags =
        tpm12::permanent_flags(tpm).map_err(|e| StartError::Startup("TPM_GetCapability", e))?;
    if !flags.physical_presence_lifetime_lock && !flags.physical_presence_cmd_enable {
        let _ = tpm12::physical_presence(tpm, tpm12::pp::CMD_ENABLE);
    }
    if let Err(error) = tpm12::physical_presence(tpm, tpm12::pp::PRESENT) {
        fstart_log::warn!("tcg: TPM 1.2 physical presence not asserted");
        let _ = error;
    }
    let volatile =
        tpm12::volatile_flags(tpm).map_err(|e| StartError::Startup("TPM_GetCapability", e))?;
    if flags.disable || flags.deactivated || volatile.deactivated {
        return Err(StartError::Inactive);
    }
    Ok(())
}

/// A started TPM 2.0 that firmware cannot measure into is hidden (PFP
/// §8.3.2): endorsement and storage hierarchies off, PCR 0-7 capped by the
/// TPM (it hashes for the banks firmware cannot), no log, and the platform
/// hierarchy behind a random authorization.
fn hide_tpm20(tpm: &mut dyn Transport) -> bool {
    use tpm20::hierarchy::{ENDORSEMENT, OWNER, PLATFORM};
    let mut ok = tpm20::hierarchy_control(tpm, PLATFORM, ENDORSEMENT, false).is_ok()
        & tpm20::hierarchy_control(tpm, PLATFORM, OWNER, false).is_ok();
    for pcr in 0..8 {
        ok &= tpm20::pcr_event(tpm, pcr, &0u32.to_le_bytes()).is_ok();
    }
    let mut auth = [0u8; 32];
    ok &= tpm20::get_random(tpm, &mut auth)
        .and_then(|()| tpm20::hierarchy_change_auth(tpm, PLATFORM, &auth))
        .or_else(|_| tpm20::hierarchy_control(tpm, PLATFORM, PLATFORM, false))
        .is_ok();
    fstart_driver_tpm::zeroize(&mut auth);
    tpm.release();
    if !ok {
        fstart_log::error!("tcg: hiding the TPM failed");
    }
    ok
}

/// Banks, bank count and log of a started TPM.
type Prepared = ([(u16, u32); MAX_BANKS], usize, EventLog);

/// Self test, banks and the first event log entry of a started TPM.
fn prepare(
    tpm: &mut dyn Transport,
    family: Family,
    log_area: &'static mut [u8],
    class: PlatformClass,
    uefi: bool,
) -> Result<Prepared, StartError> {
    let mut banks = [(0, 0); MAX_BANKS];
    let mut bank_count = 0;
    let log = match family {
        Family::Tpm20 => {
            tpm20::self_test(tpm, false).map_err(|e| StartError::Startup("TPM2_SelfTest", e))?;
            let allocated =
                tpm20::pcr_banks(tpm).map_err(|e| StartError::Startup("TPM2_GetCapability", e))?;
            for (alg, pcrs) in allocated.allocations() {
                if fstart_crypto::pcr::digest_size(alg).is_none() || bank_count == MAX_BANKS {
                    return Err(StartError::UnsupportedBank(alg));
                }
                banks[bank_count] = (alg, pcrs);
                bank_count += 1;
            }
            // PFP table 33: the Spec ID event lists at least one algorithm.
            if bank_count == 0 {
                return Err(StartError::NoBank);
            }
            let mut algs = [0; MAX_BANKS];
            for (alg, (bank, _)) in algs.iter_mut().zip(&banks) {
                *alg = *bank;
            }
            EventLog::crypto_agile(log_area, &algs[..bank_count], class)
                .map_err(|_| StartError::LogFull)?
        }
        Family::Tpm12 => {
            prepare_tpm12(tpm)?;
            banks[0] = (fstart_crypto::pcr::ALG_SHA1, u32::MAX);
            bank_count = 1;
            EventLog::sha1(log_area, class, uefi).map_err(|_| StartError::LogFull)?
        }
    };
    Ok((banks, bank_count, log))
}

/// Start the TPM and the event log in `log_area`; returns the family.
/// `uefi` says the payload is UEFI firmware: a TPM 1.2 log then also
/// identifies the TCG EFI Platform specification.
///
/// When the TPM started but cannot be measured into, it is hidden (TPM
/// 2.0, PFP §7.1, §8.3.2) or its physical presence locked (TPM 1.2). On
/// error measurements are off and the platform applies
/// [`StartError::remedy`].
pub fn start(
    tpm: &'static mut dyn Transport,
    log_area: &'static mut [u8],
    class: PlatformClass,
    uefi: bool,
) -> Result<Family, StartError> {
    if exclusive(|state| state.is_some()) != Some(false) {
        return Err(StartError::Started);
    }
    let family = startup(tpm)?;
    let (banks, bank_count, log) = match prepare(tpm, family, log_area, class, uefi) {
        Ok(prepared) => prepared,
        Err(error) => {
            let protected = match family {
                Family::Tpm20 => hide_tpm20(tpm),
                Family::Tpm12 => {
                    tpm12::physical_presence(tpm, tpm12::pp::NOTPRESENT | tpm12::pp::LOCK).is_ok()
                }
            };
            return Err(if protected {
                error
            } else {
                StartError::Unprotected
            });
        }
    };
    fstart_log::info!(
        "tcg: TPM {} started, {} PCR bank(s), log at {:#x} ({} bytes)",
        match family {
            Family::Tpm12 => "1.2",
            Family::Tpm20 => "2.0",
        },
        bank_count as u32,
        log.base(),
        log.capacity() as u32
    );
    let state = State {
        tpm,
        family,
        banks,
        bank_count,
        log,
        failed: false,
        locked: false,
        cap_failed: false,
    };
    exclusive(|slot| *slot = Some(state)).ok_or(StartError::Started)?;
    Ok(family)
}

/// The TPM family when measured boot is on.
pub fn family() -> Option<Family> {
    with(|state| (!state.failed).then_some(state.family)).flatten()
}

impl State {
    /// One digest of `data` for each bank `pcr` is allocated in.
    fn digests(&self, data: &[u8], pcr: u32) -> Digests {
        let mut digests = Digests::new();
        let allocated = |pcrs: u32| pcr < 32 && pcrs & 1 << pcr != 0;
        for &(alg, _) in self.banks[..self.bank_count]
            .iter()
            .filter(|(_, pcrs)| allocated(*pcrs))
        {
            let mut out = [0; fstart_crypto::pcr::MAX_DIGEST_SIZE];
            if let Some(size) = fstart_crypto::pcr::hash(alg, data, &mut out) {
                digests.push(alg, &out[..size]);
            }
        }
        digests
    }

    fn extend(&mut self, pcr: u32, digests: &Digests) -> Result<(), TpmError> {
        match self.family {
            Family::Tpm20 => {
                let mut list: [(u16, &[u8]); MAX_BANKS] = [(0, &[]); MAX_BANKS];
                let mut count = 0;
                for entry in digests.iter() {
                    list[count] = entry;
                    count += 1;
                }
                tpm20::pcr_extend(self.tpm, pcr, &list[..count])
            }
            Family::Tpm12 => {
                let mut digest = [0; 20];
                digest.copy_from_slice(
                    digests
                        .get(fstart_crypto::pcr::ALG_SHA1)
                        .unwrap_or(&[0; 20]),
                );
                tpm12::extend(self.tpm, pcr, &digest)
            }
        }
    }

    fn record(&mut self, pcr: u32, event_type: u32, digests: &Digests, event: &[u8]) {
        if self.failed {
            return;
        }
        if let Err(error) = self.extend(pcr, digests) {
            fstart_log::error!("tcg: extending PCR {} failed", pcr);
            self.fail(error);
            return;
        }
        if self.log.append(pcr, event_type, digests, event).is_err() {
            fstart_log::error!("tcg: event log full");
            self.fail(TpmError::Response);
        }
    }

    /// PFP §3.3.2.2: cap PCR 0-7 with the digest of `00000001h` and log the
    /// separators, then stop measuring.
    fn fail(&mut self, _error: TpmError) {
        self.failed = true;
        for pcr in 0..8 {
            let digests = self.digests(&1u32.to_le_bytes(), pcr);
            if self.extend(pcr, &digests).is_err() {
                self.cap_failed = true;
            }
            let _ = self
                .log
                .append(pcr, EV_SEPARATOR, &digests, &1u32.to_le_bytes());
        }
        if self.cap_failed {
            fstart_log::error!("tcg: PCR 0-7 could not be capped");
        }
    }

    /// Protect the platform hierarchy (TPM 2.0, PFP §11) or physical
    /// presence (TPM 1.2) from code that is not platform firmware.
    fn lock(&mut self) -> Result<(), TpmError> {
        if self.locked {
            return Ok(());
        }
        let result = match self.family {
            Family::Tpm20 => {
                let mut auth = [0u8; 32];
                let result = tpm20::get_random(self.tpm, &mut auth).and_then(|()| {
                    tpm20::hierarchy_change_auth(self.tpm, tpm20::hierarchy::PLATFORM, &auth)
                });
                fstart_driver_tpm::zeroize(&mut auth);
                // Without a random authorization the hierarchy goes off.
                result.or_else(|_| {
                    tpm20::hierarchy_control(
                        self.tpm,
                        tpm20::hierarchy::PLATFORM,
                        tpm20::hierarchy::PLATFORM,
                        false,
                    )
                })
            }
            Family::Tpm12 => {
                tpm12::physical_presence(self.tpm, tpm12::pp::NOTPRESENT | tpm12::pp::LOCK).or_else(
                    |error| {
                        // Already safe: not asserted and locked, or software
                        // cannot assert it at all.
                        let volatile = tpm12::volatile_flags(self.tpm)?;
                        let permanent = tpm12::permanent_flags(self.tpm)?;
                        let safe = !volatile.physical_presence
                            && (volatile.physical_presence_lock
                                || (permanent.physical_presence_lifetime_lock
                                    && !permanent.physical_presence_cmd_enable));
                        if safe { Ok(()) } else { Err(error) }
                    },
                )
            }
        };
        self.locked = result.is_ok();
        result
    }
}

/// A measurement firmware must make could not be made (PFP §3.3.2.2): cap
/// PCR 0-7 and stop measuring.
pub fn measurement_error() {
    with(|state| {
        if !state.failed {
            fstart_log::error!("tcg: measurement failed, PCR 0-7 capped");
            state.fail(TpmError::Response);
        }
    });
}

/// Measure `data` into `pcr` and log it with `event` as event data.
pub fn measure(pcr: u32, event_type: u32, data: &[u8], event: &[u8]) {
    with(|state| {
        let digests = state.digests(data, pcr);
        state.record(pcr, event_type, &digests, event);
    });
}

/// Measure and log `event`, which is its own measured data.
pub fn measure_event(pcr: u32, event_type: u32, event: &[u8]) {
    measure(pcr, event_type, event, event);
}

/// Measure a firmware blob at `base` into `pcr`.
///
/// `event_type` is the TPM 2.0 type (`EV_POST_CODE2`, `EV_S_CRTM_CONTENTS`
/// or `EV_EFI_PLATFORM_FIRMWARE_BLOB2`), recorded with a
/// `UEFI_PLATFORM_FIRMWARE_BLOB2` carrying `description`. A TPM 1.2 log gets
/// the TPM 1.2 type (`EV_POST_CODE`, `EV_EFI_PLATFORM_FIRMWARE_BLOB`) and a
/// `UEFI_PLATFORM_FIRMWARE_BLOB`.
pub fn measure_blob(pcr: u32, event_type: u32, description: &str, base: u64, data: &[u8]) {
    let Some(family) = family() else {
        return;
    };
    let length = data.len() as u64;
    let (event_type, event) = match family {
        Family::Tpm20 => (event_type, EventData::blob2(description, base, length)),
        Family::Tpm12 => (
            match event_type {
                event::EV_POST_CODE2 => event::EV_POST_CODE,
                event::EV_EFI_PLATFORM_FIRMWARE_BLOB2 => event::EV_EFI_PLATFORM_FIRMWARE_BLOB,
                other => other,
            },
            EventData::blob(base, length),
        ),
    };
    measure(pcr, event_type, data, event.as_bytes());
}

/// Measure the version of the static root of trust (`EV_S_CRTM_VERSION`).
pub fn measure_crtm_version(version: &str) {
    measure_event(
        0,
        event::EV_S_CRTM_VERSION,
        EventData::ucs2(version).as_bytes(),
    );
}

/// Extend the `EV_SEPARATOR` events of PCR 0-7 that end the pre-OS phase
/// (PFP §8.2.4).
pub fn separators() {
    with(|state| {
        let value = 0u32.to_le_bytes();
        for pcr in 0..8 {
            let digests = state.digests(&value, pcr);
            state.record(pcr, EV_SEPARATOR, &digests, &value);
        }
    });
}

/// Lock the TPM against code that is not platform firmware: TPM 2.0 gets a
/// random platform authorization or, failing that, a disabled platform
/// hierarchy (PFP §11); TPM 1.2 has physical presence deasserted and
/// locked. Succeeds once locked; nothing to lock without measured boot.
pub fn lock() -> Result<(), TpmError> {
    with(|state| state.lock()).unwrap_or(Ok(()))
}

/// The event log as handed to the payload and the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogInfo {
    /// The TPM that measured boot started, also after a failure.
    pub family: Family,
    pub format: Format,
    /// Physical address and size of the whole log area.
    pub base: u64,
    pub capacity: usize,
    /// Bytes used and offset of the last entry.
    pub used: usize,
    pub last_entry: Option<usize>,
}

/// The current event log, also after a failure (it then ends with the
/// error separators).
pub fn log() -> Option<LogInfo> {
    with(|state| LogInfo {
        family: state.family,
        format: state.log.format(),
        base: state.log.base(),
        capacity: state.log.capacity(),
        used: state.log.used(),
        last_entry: state.log.last_entry(),
    })
}

/// What happens to the TPM at the payload handoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Handoff {
    /// The coreboot payload contract (SeaBIOS, ...): the payload continues
    /// the log, extends the separators and locks the TPM before running
    /// code that is not platform firmware, as on coreboot.
    Open,
    /// A UEFI payload: fstart locks the TPM; the payload measures the boot
    /// and extends the separators at ReadyToBoot.
    Locked,
    /// Direct OS boot: fstart extends the separators and locks the TPM.
    Final,
}

/// The TPM cannot be handed over safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HandoffError {
    /// The platform hierarchy or physical presence could not be locked.
    Lock,
    /// A failure left PCR 0-7 uncapped.
    Cap,
}

/// Whether the payload may run: no failure left PCR 0-7 uncapped. For
/// measurements after [`handoff`] (the direct Linux launcher).
pub fn check() -> Result<(), HandoffError> {
    // The payload's own TPM accesses claim the locality again.
    let cap_failed = with(|state| {
        state.tpm.release();
        state.cap_failed
    });
    if cap_failed.unwrap_or(false) {
        return Err(HandoffError::Cap);
    }
    Ok(())
}

/// Prepare the TPM for the payload. On error the platform must not run
/// the payload (PFP §3.3.2.2 (3), §11): the launchers halt.
pub fn handoff(kind: Handoff) -> Result<(), HandoffError> {
    if kind == Handoff::Final {
        separators();
    }
    if kind != Handoff::Open && lock().is_err() {
        fstart_log::error!("tcg: the TPM could not be locked");
        return Err(HandoffError::Lock);
    }
    check()?;
    if let Some(info) = log() {
        fstart_log::info!(
            "tcg: handing over the event log, {} of {} bytes used",
            info.used as u32,
            info.capacity as u32
        );
    }
    Ok(())
}

/// Measured boot as a payload takes it over.
pub struct PayloadTpm {
    pub family: Family,
    /// The TPM, for the payload's own measurements and TPM commands.
    pub transport: &'static mut dyn Transport,
    banks: [u16; MAX_BANKS],
    bank_count: usize,
    /// The event log area; the log is in its first `log_used` bytes.
    pub log_area: &'static mut [u8],
    pub log_used: usize,
}

impl PayloadTpm {
    /// The allocated PCR banks, by algorithm ID.
    pub fn banks(&self) -> &[u16] {
        &self.banks[..self.bank_count]
    }
}

/// Hand the TPM and the event log to a payload that continues measured
/// boot in-process (CrabEFI). fstart measures nothing afterwards. `None`
/// when measured boot is off or failed: the payload must then not claim an
/// attestable TPM.
pub fn take_for_payload() -> Option<PayloadTpm> {
    // A failed measured boot stays here: `log` still describes its capped
    // log for the platform's ACPI tables.
    let state = exclusive(|slot| slot.take_if(|state| !state.failed)).flatten()?;
    let (log_area, log_used) = state.log.into_area();
    Some(PayloadTpm {
        family: state.family,
        transport: state.tpm,
        banks: {
            let mut algs = [0; MAX_BANKS];
            for (alg, (bank, _)) in algs.iter_mut().zip(&state.banks) {
                *alg = *bank;
            }
            algs
        },
        bank_count: state.bank_count,
        log_area,
        log_used,
    })
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::boxed::Box;
    use std::vec::Vec;

    /// A TPM 2.0 with a SHA-256 bank that fails the listed command codes
    /// (or every command with a transport timeout).
    struct Mock {
        fail: &'static [u32],
        timeout: bool,
        sent: Vec<u32>,
    }

    impl Transport for Mock {
        fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, TpmError> {
            if self.timeout {
                return Err(TpmError::Timeout);
            }
            let code = u32::from_be_bytes(command[6..10].try_into().unwrap());
            self.sent.push(code);
            let body: &[u8] = match code {
                0x17A => &[0, 0, 0, 0, 5, 0, 0, 0, 1, 0, 0x0B, 3, 0xFF, 0xFF, 0xFF],
                0x17B => &[
                    0, 32, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7, 7,
                    7, 7, 7, 7, 7, 7, 7, 7,
                ],
                _ => &[],
            };
            let rc: u32 = if self.fail.contains(&code) { 0x101 } else { 0 };
            let body = if rc == 0 { body } else { &[] };
            let len = 10 + body.len();
            response[..2].copy_from_slice(&0x8001u16.to_be_bytes());
            response[2..6].copy_from_slice(&(len as u32).to_be_bytes());
            response[6..10].copy_from_slice(&rc.to_be_bytes());
            response[10..len].copy_from_slice(body);
            Ok(len)
        }
        fn wait_ms(&mut self, _: u32) {}
    }

    fn tpm(fail: &'static [u32], timeout: bool) -> &'static mut Mock {
        Box::leak(Box::new(Mock {
            fail,
            timeout,
            sent: Vec::new(),
        }))
    }

    fn log_area() -> &'static mut [u8] {
        Box::leak(std::vec![0xAAu8; 4096].into_boxed_slice())
    }

    /// A TPM 1.2: rejects TPM 2.0 commands with its own tag, enabled and
    /// active, records the ordinals it gets.
    struct Tpm12Mock(Vec<u32>);

    impl Transport for Tpm12Mock {
        fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, TpmError> {
            let tag = u16::from_be_bytes([command[0], command[1]]);
            let ordinal = u32::from_be_bytes(command[6..10].try_into().unwrap());
            self.0.push(ordinal);
            let (code, body): (u32, Vec<u8>) = match (tag, ordinal) {
                (0x8001 | 0x8002, _) => (0x0A, Vec::new()),
                (_, 0x65) => {
                    let sub = u32::from_be_bytes(command[18..22].try_into().unwrap());
                    let flags: &[u8] = if sub == 0x108 {
                        &[
                            0x00, 0x1F, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                        ]
                    } else {
                        &[0x00, 0x20, 0, 0, 1, 0, 0]
                    };
                    let mut body = (flags.len() as u32).to_be_bytes().to_vec();
                    body.extend_from_slice(flags);
                    (0, body)
                }
                (_, 0x14) => (0, std::vec![0; 20]),
                _ => (0, Vec::new()),
            };
            let len = 10 + body.len();
            response[..2].copy_from_slice(&0x00C4u16.to_be_bytes());
            response[2..6].copy_from_slice(&(len as u32).to_be_bytes());
            response[6..10].copy_from_slice(&code.to_be_bytes());
            response[10..len].copy_from_slice(&body);
            Ok(len)
        }
        fn wait_ms(&mut self, _: u32) {}
    }

    // One test: measured boot state is a global.
    #[test]
    fn startup_lock_and_failure_paths() {
        // TPM 1.2: TPM2_Startup answered with a TPM 1.2 tag, then
        // TPM_Startup, self test first, physical presence, and a log with
        // Spec ID Event00 and Event02 before the first measurement.
        let mock = Box::leak(Box::new(Tpm12Mock(Vec::new())));
        let sent: *const Vec<u32> = &mock.0;
        assert_eq!(
            start(mock, log_area(), PlatformClass::Client, true),
            Ok(Family::Tpm12)
        );
        measure_crtm_version("test");
        assert_eq!(handoff(Handoff::Locked), Ok(()));
        // SAFETY: the mock outlives the test; nothing else touches it now.
        let sent = unsafe { &*sent };
        assert_eq!(&sent[..4], &[0x144, 0x99, 0x53, 0x65]);
        assert!(sent.contains(&0x14), "extended");
        assert_eq!(sent.last(), Some(&0x4000_000A), "physical presence locked");
        let payload = take_for_payload().unwrap();
        let log = &payload.log_area;
        assert_eq!(&log[32..48], b"Spec ID Event00\0");
        assert_eq!(&log[57 + 32..57 + 48], b"Spec ID Event02\0");
        assert_eq!(
            u32::from_le_bytes(log[118..122].try_into().unwrap()),
            8,
            "EV_S_CRTM_VERSION"
        );

        // A transport failure is not taken for a TPM 1.2.
        let mock = tpm(&[], true);
        assert_eq!(
            start(mock, log_area(), PlatformClass::Client, false),
            Err(StartError::Startup("TPM2_Startup", TpmError::Timeout))
        );

        // Lock falls back to disabling the platform hierarchy.
        let mock = tpm(&[0x129], false);
        let sent: *const Vec<u32> = &mock.sent;
        assert_eq!(
            start(mock, log_area(), PlatformClass::Client, false),
            Ok(Family::Tpm20)
        );
        measure_crtm_version("test");
        assert_eq!(handoff(Handoff::Locked), Ok(()));
        // SAFETY: the mock outlives the test; nothing else touches it now.
        let sent = unsafe { &*sent };
        assert_eq!(&sent[sent.len() - 3..], &[0x17B, 0x129, 0x121]);
        assert!(take_for_payload().is_some());

        // Self test fails: the started TPM is hidden...
        let mock = tpm(&[0x143], false);
        assert_eq!(
            start(mock, log_area(), PlatformClass::Client, false),
            Err(StartError::Startup("TPM2_SelfTest", TpmError::Code(0x101)))
        );
        // ...unless hiding fails too: then nothing may boot.
        let mock = tpm(&[0x143, 0x121], false);
        let error = start(mock, log_area(), PlatformClass::Client, false).unwrap_err();
        assert_eq!(
            (error, error.remedy()),
            (StartError::Unprotected, Remedy::Halt)
        );

        // Neither works: the payload must not run.
        let mock = tpm(&[0x129, 0x121], false);
        start(mock, log_area(), PlatformClass::Client, false).unwrap();
        assert_eq!(handoff(Handoff::Locked), Err(HandoffError::Lock));
        take_for_payload();

        // A failed extend caps; failed caps forbid the handoff.
        let mock = tpm(&[0x182], false);
        start(mock, log_area(), PlatformClass::Client, false).unwrap();
        measure_crtm_version("test");
        assert_eq!(family(), None, "measuring stopped");
        assert_eq!(handoff(Handoff::Open), Err(HandoffError::Cap));
        assert!(take_for_payload().is_none());
    }
}
