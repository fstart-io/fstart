//! Intel x86 microcode update parser and loader.
//!
//! Intel distributes microcode as concatenated update records.  Each record
//! starts with a 48-byte header and may include an extended signature table.
//! The CPU authenticates the encrypted/signed payload when firmware writes the
//! data pointer to `IA32_BIOS_UPDT_TRIG`; this crate only selects the update
//! that matches the current processor signature/platform flags.

#![no_std]

use fstart_arch_x86::cpuid;
use fstart_arch_x86::msr::{rdmsr, wrmsr};

const IA32_PLATFORM_ID: u32 = 0x17;
const IA32_BIOS_UPDT_TRIG: u32 = 0x79;
const IA32_BIOS_SIGN_ID: u32 = 0x8b;

const HEADER_SIZE: usize = 48;
const DEFAULT_UPDATE_SIZE: usize = 2048;
const EXT_TABLE_SIZE: usize = 20;
const EXT_ENTRY_SIZE: usize = 12;

/// Result of attempting a microcode update on the current CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpdateStatus {
    /// A matching update was found and successfully installed.
    Updated { old_rev: u32, new_rev: u32 },
    /// The CPU already has this revision or a newer one installed.
    AlreadyCurrent { rev: u32 },
    /// The blob contained no update matching this CPU signature/platform.
    NoMatchingPatch,
}

/// Error while parsing or loading an Intel microcode update.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MicrocodeError {
    /// A record's advertised size would run past the supplied blob.
    TruncatedRecord,
    /// A matching update was found but the CPU did not report the new revision.
    LoadFailed { expected: u32, actual: u32 },
}

/// Borrowed Intel microcode patch selected for the current CPU.
#[derive(Debug, Clone, Copy)]
pub struct MicrocodePatch<'a> {
    record: &'a [u8],
    revision: u32,
}

impl<'a> MicrocodePatch<'a> {
    /// Entire update record, including the 48-byte Intel header.
    pub fn record(self) -> &'a [u8] {
        self.record
    }

    /// Intel update revision from the record header.
    pub fn revision(self) -> u32 {
        self.revision
    }

    fn data_ptr(self) -> u64 {
        // Intel SDM: IA32_BIOS_UPDT_TRIG receives a physical pointer to the
        // encrypted update data following the 48-byte header.
        self.record.as_ptr().wrapping_add(HEADER_SIZE) as u64
    }
}

#[derive(Debug, Clone, Copy)]
struct Header {
    revision: u32,
    signature: u32,
    platform_flags: u32,
    data_size: u32,
    total_size: u32,
}

impl Header {
    fn parse(record: &[u8]) -> Option<Self> {
        Some(Self {
            revision: le32(record, 4)?,
            signature: le32(record, 12)?,
            platform_flags: le32(record, 24)?,
            data_size: le32(record, 28)?,
            total_size: le32(record, 32)?,
        })
    }

    fn update_size(self) -> usize {
        if self.total_size == 0 {
            DEFAULT_UPDATE_SIZE
        } else {
            self.total_size as usize
        }
    }
}

fn le32(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn current_signature_and_platform() -> (u32, u32) {
    let (sig, _, _, _) = cpuid(1);
    let family = ((sig >> 8) & 0xf) + ((sig >> 20) & 0xff);
    let model = ((sig >> 4) & 0xf) + (((sig >> 16) & 0xf) << 4);

    let mut platform = 0;
    if model >= 5 || family > 6 {
        // SAFETY: IA32_PLATFORM_ID is present on Intel CPUs in the families
        // that use these microcode updates.  If this is not Intel, the caller
        // should not provide an Intel microcode blob.
        let msr = unsafe { rdmsr(IA32_PLATFORM_ID) };
        platform = 1u32 << (((msr >> 50) & 7) as u32);
    }

    (sig, platform)
}

fn record_matches(record: &[u8], header: Header, sig: u32, platform: u32) -> bool {
    if header.signature == sig && (header.platform_flags & platform) != 0 {
        return true;
    }

    let base_size = HEADER_SIZE.saturating_add(header.data_size as usize);
    let total_size = header.update_size();
    let ext = match record.get(base_size..total_size) {
        Some(ext) if ext.len() >= EXT_TABLE_SIZE => ext,
        _ => return false,
    };

    let count = match le32(ext, 0) {
        Some(count) => count as usize,
        None => return false,
    };
    let entries_len = match count.checked_mul(EXT_ENTRY_SIZE) {
        Some(len) => len,
        None => return false,
    };
    if ext.len() < EXT_TABLE_SIZE + entries_len {
        return false;
    }

    let mut off = EXT_TABLE_SIZE;
    for _ in 0..count {
        let entry_sig = match le32(ext, off) {
            Some(v) => v,
            None => return false,
        };
        let entry_platform = match le32(ext, off + 4) {
            Some(v) => v,
            None => return false,
        };
        if entry_sig == sig && (entry_platform & platform) != 0 {
            return true;
        }
        off += EXT_ENTRY_SIZE;
    }

    false
}

/// Read the currently installed microcode revision.
///
/// This uses the CPUID sequence coreboot uses because some Intel CPUs are
/// sensitive to exactly how `IA32_BIOS_SIGN_ID` is sampled.
pub fn current_revision() -> u32 {
    // SAFETY: IA32_BIOS_SIGN_ID is the architectural Intel microcode revision
    // reporting MSR.  Writing zero, executing CPUID(1), then reading it is the
    // documented discovery sequence.
    unsafe {
        wrmsr(IA32_BIOS_SIGN_ID, 0);
        let _ = cpuid(1);
        (rdmsr(IA32_BIOS_SIGN_ID) >> 32) as u32
    }
}

/// Find the best matching patch for the current CPU in a concatenated blob.
pub fn find_for_current_cpu(blob: &[u8]) -> Result<Option<MicrocodePatch<'_>>, MicrocodeError> {
    let (sig, platform) = current_signature_and_platform();
    let current = current_revision();
    let mut best: Option<MicrocodePatch<'_>> = None;
    let mut offset = 0usize;

    while blob.len().saturating_sub(offset) >= HEADER_SIZE {
        let remaining = &blob[offset..];
        let header = match Header::parse(remaining) {
            Some(header) => header,
            None => break,
        };
        let size = header.update_size();
        if size < HEADER_SIZE || size > remaining.len() {
            return Err(MicrocodeError::TruncatedRecord);
        }
        let record = &remaining[..size];
        if record_matches(record, header, sig, platform) && header.revision > current {
            let candidate = MicrocodePatch {
                record,
                revision: header.revision,
            };
            if best.is_none_or(|patch| candidate.revision > patch.revision) {
                best = Some(candidate);
            }
        }
        offset += size;
    }

    Ok(best)
}

/// Load a selected patch on the current CPU.
///
/// # Safety
///
/// The caller must ensure `patch.record()` is reachable at its current physical
/// address by this CPU and remains alive for the duration of the WRMSR.
pub unsafe fn load(patch: MicrocodePatch<'_>) -> Result<UpdateStatus, MicrocodeError> {
    let old_rev = current_revision();
    if old_rev >= patch.revision {
        return Ok(UpdateStatus::AlreadyCurrent { rev: old_rev });
    }

    unsafe { wrmsr(IA32_BIOS_UPDT_TRIG, patch.data_ptr()) };

    let new_rev = current_revision();
    if new_rev == patch.revision {
        Ok(UpdateStatus::Updated { old_rev, new_rev })
    } else {
        Err(MicrocodeError::LoadFailed {
            expected: patch.revision,
            actual: new_rev,
        })
    }
}

/// Find and apply the best matching patch for the current CPU.
///
/// # Safety
///
/// The blob must be physically reachable by the CPU for MSR-triggered loading.
pub unsafe fn update_current_cpu(blob: &[u8]) -> Result<UpdateStatus, MicrocodeError> {
    let Some(patch) = find_for_current_cpu(blob)? else {
        return Ok(UpdateStatus::NoMatchingPatch);
    };
    unsafe { load(patch) }
}

/// Apply a microcode blob and log the result.  Intended for MP flight-plan use.
///
/// # Safety
///
/// Same requirements as [`update_current_cpu`].
pub unsafe fn update_current_cpu_logged(blob: &[u8]) {
    match unsafe { update_current_cpu(blob) } {
        Ok(UpdateStatus::Updated { old_rev, new_rev }) => {
            fstart_log::info!("microcode: updated {:#x} -> {:#x}", old_rev, new_rev);
        }
        Ok(UpdateStatus::AlreadyCurrent { rev }) => {
            fstart_log::info!("microcode: already current rev={:#x}", rev);
        }
        Ok(UpdateStatus::NoMatchingPatch) => {
            fstart_log::warn!("microcode: no matching Intel patch found");
        }
        Err(MicrocodeError::LoadFailed { expected, actual }) => {
            fstart_log::error!(
                "microcode: update failed expected={:#x} actual={:#x}",
                expected,
                actual
            );
        }
        Err(MicrocodeError::TruncatedRecord) => {
            fstart_log::error!("microcode: blob has a truncated record");
        }
    }
}
