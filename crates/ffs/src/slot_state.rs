// SPDX-License-Identifier: MIT OR Apache-2.0
//! Power-fail-safe, append-only NOR flash slot-state journal.
//!
//! Two 4 KiB erase-aligned sectors (`SLOT_STATE_0` and `SLOT_STATE_1`) hold
//! fixed 128-byte CRC32C-protected records (32 records per sector).
//!
//! - Bytes `0x00..0x70` hold the header, full 32-byte `Root` SHA-256, and
//!   CRC32C. Bytes `0x70..0x80` form a dedicated 16-byte commit line whose
//!   last byte (`0x7F`) is programmed to `0x5A` only after `0x00..0x70` is
//!   written and read back, keeping the commit write on its own 16-byte ECC
//!   line for ECC-protected NOR flashes.
//! - CrabEFI rolls over to the spare sector before starting a trial whenever
//!   the active sector has fewer than `tries + 2` free slots remaining, so RO
//!   pre-RAM only ever appends and never needs erase logic.
//! - Torn cells are never reused: scanning finds the highest valid `seq` and
//!   tracks the first erased slot strictly after the last non-`0xFF` cell.

use fstart_core::services::{FlashRegion, ServiceError};
use zerocopy::byteorder::{LE, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const RECORD_MAGIC: [u8; 4] = *b"FSSL";
pub const RECORD_VERSION: u8 = 2;
pub const RECORD_SIZE: usize = 128;
pub const BODY_SIZE: usize = 0x70;
pub const COMMIT_LINE_OFFSET: usize = 0x70;
pub const COMMIT_LINE_SIZE: usize = 16;
pub const COMMIT_OFFSET: usize = 0x7f;
pub const SECTOR_SIZE: usize = 4096;
pub const RECORDS_PER_SECTOR: usize = SECTOR_SIZE / RECORD_SIZE;
pub const COMMIT_MARKER: u8 = 0x5a;
pub const ERASED_BYTE: u8 = 0xff;
pub const DEFAULT_TRIAL_ATTEMPTS: u8 = 3;

/// Standard UEFI ESRT `LastAttemptStatus` values.
pub mod esrt_status {
    pub const SUCCESS: u32 = 0x0000_0000;
    pub const UNSUCCESSFUL: u32 = 0x0000_0001;
    pub const INSUFFICIENT_RESOURCES: u32 = 0x0000_0002;
    pub const INCORRECT_VERSION: u32 = 0x0000_0003;
    pub const INVALID_FORMAT: u32 = 0x0000_0004;
    pub const AUTH_ERROR: u32 = 0x0000_0005;
    pub const POWER_EVENT_AC: u32 = 0x0000_0006;
    pub const POWER_EVENT_BATTERY: u32 = 0x0000_0007;
}

/// Record flags.
pub mod flags {
    pub const CONFIRMED: u8 = 1 << 0;
    pub const SYNC_DONE: u8 = 1 << 1;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Phase {
    Stable = 1,
    Installing = 2,
    Trial = 3,
    FloorPending = 4,
    Recovery = 5,
    /// Confirmed running slot is being copied to the inactive slot before
    /// advancing the TPM rollback floor; a power cut resumes syncing from the
    /// confirmed `active_slot` rather than falling back to the half-erased slot.
    Syncing = 6,
}

impl Phase {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            1 => Some(Self::Stable),
            2 => Some(Self::Installing),
            3 => Some(Self::Trial),
            4 => Some(Self::FloorPending),
            5 => Some(Self::Recovery),
            6 => Some(Self::Syncing),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Slot {
    A = 0,
    B = 1,
    Recovery = 0xff,
}

impl Slot {
    pub const fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::A),
            1 => Some(Self::B),
            0xff => Some(Self::Recovery),
            _ => None,
        }
    }

    pub const fn other_rw(self) -> Option<Self> {
        match self {
            Self::A => Some(Self::B),
            Self::B => Some(Self::A),
            Self::Recovery => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    Unaligned,
    EmptySlot,
    OutOfBounds,
    OverlappingRegions,
    MismatchedSlotSizes,
}

/// RO-owned flash geometry for slot selection and updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotExtent {
    pub offset: u32,
    pub size: u32,
}

impl SlotExtent {
    pub const fn end(self) -> Option<u32> {
        self.offset.checked_add(self.size)
    }

    pub fn validate(self, capacity: u32) -> Result<(), LayoutError> {
        let sector = SECTOR_SIZE as u32;
        if self.size == 0 {
            return Err(LayoutError::EmptySlot);
        }
        if !self.offset.is_multiple_of(sector) || !self.size.is_multiple_of(sector) {
            return Err(LayoutError::Unaligned);
        }
        let end = self.end().ok_or(LayoutError::OutOfBounds)?;
        if end > capacity {
            return Err(LayoutError::OutOfBounds);
        }
        Ok(())
    }

    pub const fn overlaps(self, other: Self) -> bool {
        let (Some(a_end), Some(b_end)) = (self.end(), other.end()) else {
            return true;
        };
        self.offset < b_end && other.offset < a_end
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotLayout {
    /// Single RW slot (`RW_A`) with RO recovery fallback.
    RoA {
        state_sector_0: u32,
        state_sector_1: u32,
        slot_a: SlotExtent,
    },
    /// Dual RW slots (`RW_A` and `RW_B`) with automatic fallback and RO recovery.
    RoAB {
        state_sector_0: u32,
        state_sector_1: u32,
        slot_a: SlotExtent,
        slot_b: SlotExtent,
    },
}

impl SlotLayout {
    pub const fn state_sectors(&self) -> (u32, u32) {
        match *self {
            Self::RoA {
                state_sector_0,
                state_sector_1,
                ..
            }
            | Self::RoAB {
                state_sector_0,
                state_sector_1,
                ..
            } => (state_sector_0, state_sector_1),
        }
    }

    pub const fn extent(&self, slot: Slot) -> Option<SlotExtent> {
        match (*self, slot) {
            (Self::RoA { slot_a, .. } | Self::RoAB { slot_a, .. }, Slot::A) => Some(slot_a),
            (Self::RoAB { slot_b, .. }, Slot::B) => Some(slot_b),
            _ => None,
        }
    }

    pub const fn is_ab(&self) -> bool {
        matches!(self, Self::RoAB { .. })
    }

    /// Validate alignment, non-zero sizes, flash bounds, A/B size equality,
    /// and pairwise non-overlap across both state sectors and all RW slots.
    pub fn validate(&self, flash_capacity: u32) -> Result<(), LayoutError> {
        let (s0, s1) = self.state_sectors();
        let e0 = SlotExtent {
            offset: s0,
            size: SECTOR_SIZE as u32,
        };
        let e1 = SlotExtent {
            offset: s1,
            size: SECTOR_SIZE as u32,
        };
        e0.validate(flash_capacity)?;
        e1.validate(flash_capacity)?;

        match *self {
            Self::RoA { slot_a, .. } => {
                slot_a.validate(flash_capacity)?;
                let regions = [e0, e1, slot_a];
                for i in 0..regions.len() {
                    for j in (i + 1)..regions.len() {
                        if regions[i].overlaps(regions[j]) {
                            return Err(LayoutError::OverlappingRegions);
                        }
                    }
                }
            }
            Self::RoAB { slot_a, slot_b, .. } => {
                slot_a.validate(flash_capacity)?;
                slot_b.validate(flash_capacity)?;
                if slot_a.size != slot_b.size {
                    return Err(LayoutError::MismatchedSlotSizes);
                }
                let regions = [e0, e1, slot_a, slot_b];
                for i in 0..regions.len() {
                    for j in (i + 1)..regions.len() {
                        if regions[i].overlaps(regions[j]) {
                            return Err(LayoutError::OverlappingRegions);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct RecordWire {
    magic: [u8; 4],
    version: u8,
    phase: u8,
    active_slot: u8,
    fallback_slot: u8,
    tries_left: u8,
    flags: u8,
    reserved: [u8; 2],
    fw_version: U32<LE>,
    seq: U64<LE>,
    sec_version: U64<LE>,
    last_attempt_ver: U32<LE>,
    last_attempt_status: U32<LE>,
    root_digest: [u8; 32],
    reserved_body: [u8; 36],
    crc32c: U32<LE>,
    // Dedicated 16-byte commit line (0x70..0x80) so programming the commit
    // marker never touches the ECC line containing body/CRC bytes.
    commit_pad: [u8; 15],
    commit: u8,
}

const _: () = assert!(core::mem::size_of::<RecordWire>() == RECORD_SIZE);
const CRC_END: usize = BODY_SIZE - 4; // 0x6c

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordError {
    InvalidLength,
    Uncommitted,
    InvalidFormat,
    UnsupportedVersion,
    CrcMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotStateRecord {
    pub phase: Phase,
    pub active_slot: Slot,
    pub fallback_slot: Slot,
    pub tries_left: u8,
    pub flags: u8,
    pub fw_version: u32,
    pub seq: u64,
    pub sec_version: u64,
    pub last_attempt_ver: u32,
    pub last_attempt_status: u32,
    pub root_digest: [u8; 32],
}

impl SlotStateRecord {
    pub fn initial(
        slot: Slot,
        fallback: Slot,
        fw_version: u32,
        sec_version: u64,
        root_digest: &[u8; 32],
    ) -> Self {
        Self {
            phase: Phase::Stable,
            active_slot: slot,
            fallback_slot: fallback,
            tries_left: 0,
            flags: flags::CONFIRMED,
            fw_version,
            seq: 1,
            sec_version,
            last_attempt_ver: fw_version,
            last_attempt_status: esrt_status::SUCCESS,
            root_digest: *root_digest,
        }
    }

    /// Encode the full 128-byte record including its dedicated 16-byte commit
    /// line (`0x70..0x80`).
    pub fn encode(&self) -> [u8; RECORD_SIZE] {
        let mut wire = RecordWire {
            magic: RECORD_MAGIC,
            version: RECORD_VERSION,
            phase: self.phase as u8,
            active_slot: self.active_slot as u8,
            fallback_slot: self.fallback_slot as u8,
            tries_left: self.tries_left,
            flags: self.flags,
            reserved: [0; 2],
            fw_version: U32::new(self.fw_version),
            seq: U64::new(self.seq),
            sec_version: U64::new(self.sec_version),
            last_attempt_ver: U32::new(self.last_attempt_ver),
            last_attempt_status: U32::new(self.last_attempt_status),
            root_digest: self.root_digest,
            reserved_body: [0; 36],
            crc32c: U32::new(0),
            commit_pad: [ERASED_BYTE; 15],
            commit: COMMIT_MARKER,
        };
        let crc = crc32c(&wire.as_bytes()[..CRC_END]);
        wire.crc32c = U32::new(crc);
        let mut out = [0u8; RECORD_SIZE];
        out.copy_from_slice(wire.as_bytes());
        out
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, RecordError> {
        if bytes.len() != RECORD_SIZE {
            return Err(RecordError::InvalidLength);
        }
        let wire = RecordWire::read_from_bytes(bytes).map_err(|_| RecordError::InvalidFormat)?;
        if wire.commit != COMMIT_MARKER {
            return Err(RecordError::Uncommitted);
        }
        if wire.magic != RECORD_MAGIC {
            return Err(RecordError::InvalidFormat);
        }
        if wire.version != RECORD_VERSION {
            return Err(RecordError::UnsupportedVersion);
        }
        if wire.reserved != [0; 2]
            || wire.reserved_body != [0; 36]
            || wire.commit_pad != [ERASED_BYTE; 15]
        {
            return Err(RecordError::InvalidFormat);
        }
        let expected_crc = crc32c(&bytes[..CRC_END]);
        if wire.crc32c.get() != expected_crc {
            return Err(RecordError::CrcMismatch);
        }
        let phase = Phase::from_u8(wire.phase).ok_or(RecordError::InvalidFormat)?;
        let active_slot = Slot::from_u8(wire.active_slot).ok_or(RecordError::InvalidFormat)?;
        let fallback_slot = Slot::from_u8(wire.fallback_slot).ok_or(RecordError::InvalidFormat)?;
        let seq = wire.seq.get();
        if seq == 0 {
            return Err(RecordError::InvalidFormat);
        }
        Ok(Self {
            phase,
            active_slot,
            fallback_slot,
            tries_left: wire.tries_left,
            flags: wire.flags,
            fw_version: wire.fw_version.get(),
            seq,
            sec_version: wire.sec_version.get(),
            last_attempt_ver: wire.last_attempt_ver.get(),
            last_attempt_status: wire.last_attempt_status.get(),
            root_digest: wire.root_digest,
        })
    }

    pub fn is_confirmed(&self) -> bool {
        self.flags & flags::CONFIRMED != 0
    }

    pub fn is_sync_done(&self) -> bool {
        self.flags & flags::SYNC_DONE != 0
    }

    pub fn matches_root_digest(&self, digest: &[u8; 32]) -> bool {
        self.root_digest == *digest
    }

    /// RO pre-RAM transition: debit one trial attempt before jumping to RW.
    pub fn next_try_debit(&self) -> Option<Self> {
        if self.phase != Phase::Trial || self.tries_left == 0 {
            return None;
        }
        let seq = self.seq.checked_add(1)?;
        Some(Self {
            tries_left: self.tries_left - 1,
            seq,
            ..*self
        })
    }

    /// RO pre-RAM transition: trial exhausted or failed verification, falling
    /// back to an authenticated `fallback_slot` whose versions and Root digest
    /// are recorded so subsequent cold boots validate it as `Stable`.
    pub fn next_trial_exhausted(
        &self,
        fallback_fw_version: u32,
        fallback_sec_version: u64,
        fallback_root_digest: &[u8; 32],
    ) -> Option<Self> {
        if self.phase != Phase::Trial || self.fallback_slot == Slot::Recovery {
            return None;
        }
        let seq = self.seq.checked_add(1)?;
        Some(Self {
            phase: Phase::Stable,
            active_slot: self.fallback_slot,
            fallback_slot: Slot::Recovery,
            tries_left: 0,
            flags: flags::CONFIRMED,
            fw_version: fallback_fw_version,
            seq,
            sec_version: fallback_sec_version,
            last_attempt_ver: self.fw_version,
            last_attempt_status: esrt_status::UNSUCCESSFUL,
            root_digest: *fallback_root_digest,
        })
    }

    /// RO pre-RAM transition: trial exhausted with no valid RW fallback slot;
    /// enter `Phase::Recovery` while recording the failed attempt for ESRT.
    pub fn next_trial_exhausted_to_recovery(&self) -> Option<Self> {
        if self.phase != Phase::Trial {
            return None;
        }
        let seq = self.seq.checked_add(1)?;
        Some(Self {
            phase: Phase::Recovery,
            active_slot: Slot::Recovery,
            fallback_slot: Slot::Recovery,
            tries_left: 0,
            flags: 0,
            seq,
            last_attempt_ver: self.fw_version,
            last_attempt_status: esrt_status::UNSUCCESSFUL,
            ..*self
        })
    }

    /// RO pre-RAM transition: primary slot failed payload load after Root
    /// authentication; fall back to `fallback_slot` (must be an RW slot) and
    /// persist its authenticated versions and Root digest as `Stable`.
    pub fn next_load_fallback(
        &self,
        fallback_fw_version: u32,
        fallback_sec_version: u64,
        fallback_root_digest: &[u8; 32],
    ) -> Option<Self> {
        if !matches!(self.fallback_slot, Slot::A | Slot::B)
            || self.fallback_slot == self.active_slot
        {
            return None;
        }
        let seq = self.seq.checked_add(1)?;
        Some(Self {
            phase: Phase::Stable,
            active_slot: self.fallback_slot,
            fallback_slot: Slot::Recovery,
            tries_left: 0,
            flags: flags::CONFIRMED,
            fw_version: fallback_fw_version,
            seq,
            sec_version: fallback_sec_version,
            last_attempt_ver: if self.phase == Phase::Trial {
                self.fw_version
            } else {
                self.last_attempt_ver
            },
            last_attempt_status: if self.phase == Phase::Trial {
                esrt_status::UNSUCCESSFUL
            } else {
                self.last_attempt_status
            },
            root_digest: *fallback_root_digest,
        })
    }

    /// CrabEFI transition: mark install in progress before erasing target slot.
    ///
    /// Returns `None` while an unconfirmed trial (`Trial`), peer-slot sync
    /// (`Syncing`), or pending TPM anti-rollback floor advance (`FloorPending`)
    /// is still in progress: overwriting those phases before RO durably
    /// finalizes them would drop rollback protection or clobber a half-synced
    /// peer slot.
    pub fn next_installing(
        &self,
        target: Slot,
        fallback: Slot,
        fw_version: u32,
        sec_version: u64,
        root_digest: &[u8; 32],
    ) -> Option<Self> {
        if matches!(
            self.phase,
            Phase::Trial | Phase::Syncing | Phase::FloorPending
        ) {
            return None;
        }
        let seq = self.seq.checked_add(1)?;
        Some(Self {
            phase: Phase::Installing,
            active_slot: target,
            fallback_slot: fallback,
            tries_left: 0,
            flags: 0,
            fw_version,
            seq,
            sec_version,
            last_attempt_ver: fw_version,
            last_attempt_status: esrt_status::UNSUCCESSFUL,
            root_digest: *root_digest,
        })
    }

    /// CrabEFI transition: target slot written and verified, start trial boots.
    pub fn next_trial(
        &self,
        target: Slot,
        fallback: Slot,
        tries: u8,
        fw_version: u32,
        sec_version: u64,
        root_digest: &[u8; 32],
    ) -> Option<Self> {
        if tries == 0 {
            return None;
        }
        let seq = self.seq.checked_add(1)?;
        Some(Self {
            phase: Phase::Trial,
            active_slot: target,
            fallback_slot: fallback,
            tries_left: tries,
            flags: 0,
            fw_version,
            seq,
            sec_version,
            last_attempt_ver: fw_version,
            last_attempt_status: esrt_status::UNSUCCESSFUL,
            root_digest: *root_digest,
        })
    }

    /// CrabEFI transition: persist confirmation *before* erasing the fallback
    /// slot during sync-before-bump, setting `fallback_slot = Recovery` so a
    /// power cut mid-copy keeps booting the confirmed running slot and never
    /// falls back to the half-erased slot.
    pub fn next_syncing(&self) -> Option<Self> {
        let seq = self.seq.checked_add(1)?;
        Some(Self {
            phase: Phase::Syncing,
            fallback_slot: Slot::Recovery,
            tries_left: 0,
            flags: flags::CONFIRMED,
            seq,
            last_attempt_ver: self.fw_version,
            last_attempt_status: esrt_status::SUCCESS,
            ..*self
        })
    }

    /// CrabEFI transition: confirm the currently booted trial slot.
    ///
    /// If `needs_floor_bump` is true, enters `Phase::FloorPending` so RO
    /// advances the TPM NV floor on the next cold boot before locking it.
    pub fn next_confirm(&self, sync_done: bool, needs_floor_bump: bool) -> Option<Self> {
        let seq = self.seq.checked_add(1)?;
        let mut new_flags = flags::CONFIRMED;
        let fallback_slot = if sync_done {
            new_flags |= flags::SYNC_DONE;
            // The inactive RW slot was just synced and verified to match the
            // confirmed active slot, so restore it as the automatic A/B fallback.
            self.active_slot.other_rw().unwrap_or(self.fallback_slot)
        } else {
            self.fallback_slot
        };
        Some(Self {
            phase: if needs_floor_bump {
                Phase::FloorPending
            } else {
                Phase::Stable
            },
            fallback_slot,
            tries_left: 0,
            flags: new_flags,
            seq,
            last_attempt_ver: self.fw_version,
            last_attempt_status: esrt_status::SUCCESS,
            ..*self
        })
    }

    /// RO pre-RAM transition: finalize `FloorPending` -> `Stable` after TPM NV
    /// has been advanced and verified.
    pub fn next_floor_finalized(&self) -> Option<Self> {
        if self.phase != Phase::FloorPending {
            return None;
        }
        let seq = self.seq.checked_add(1)?;
        Some(Self {
            phase: Phase::Stable,
            seq,
            ..*self
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SectorSummary {
    /// Highest valid record found in this sector, if any.
    pub latest: Option<SlotStateRecord>,
    /// True if two distinct valid records shared the highest `seq` in this sector.
    pub conflict: bool,
    /// True if every byte in the sector is `0xFF`.
    pub all_erased: bool,
    /// Index (`0..RECORDS_PER_SECTOR`) of the first erased slot strictly after
    /// the last non-`0xFF` cell. `RECORDS_PER_SECTOR` means the sector is full.
    pub next_erased_index: usize,
}

impl SectorSummary {
    pub const fn free_slots(&self) -> usize {
        RECORDS_PER_SECTOR.saturating_sub(self.next_erased_index)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStatus {
    /// Both sectors are completely erased (`0xFF`).
    Blank,
    /// At least one sector has non-`0xFF` bytes, but zero valid records exist.
    Corrupt,
    /// Valid latest record found in `active_sector` (`0` or `1`).
    Valid {
        active_sector: u8,
        record: SlotStateRecord,
    },
    /// Equal highest `seq` appeared with differing contents (within or across sectors).
    Conflict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JournalScan {
    pub sectors: [SectorSummary; 2],
    pub status: ScanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendPlan {
    /// Append directly into an already-erased 128-byte slot (no erase needed).
    WriteInSector { sector: u8, offset: usize },
    /// Erase `sector`, verify erased, then write at offset 0 of `sector`.
    RolloverToSector { sector: u8, offset: usize },
}

/// Streaming single-sector scanner using only 128 bytes of stack (safe for CAR).
#[derive(Debug, Clone, Copy)]
pub struct SectorScanner {
    latest: Option<SlotStateRecord>,
    conflict: bool,
    last_non_erased: Option<usize>,
    idx: usize,
}

impl Default for SectorScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl SectorScanner {
    pub const fn new() -> Self {
        Self {
            latest: None,
            conflict: false,
            last_non_erased: None,
            idx: 0,
        }
    }

    pub fn feed(&mut self, cell: &[u8; RECORD_SIZE]) {
        if self.idx >= RECORDS_PER_SECTOR {
            return;
        }
        let idx = self.idx;
        self.idx += 1;
        if cell.iter().all(|&b| b == ERASED_BYTE) {
            return;
        }
        self.last_non_erased = Some(idx);
        if let Ok(rec) = SlotStateRecord::parse(cell) {
            match self.latest {
                None => {
                    self.latest = Some(rec);
                    self.conflict = false;
                }
                Some(cur) if rec.seq > cur.seq => {
                    self.latest = Some(rec);
                    self.conflict = false;
                }
                Some(cur) if rec.seq == cur.seq && rec != cur => {
                    self.conflict = true;
                }
                _ => {}
            }
        }
    }

    pub const fn finish(self) -> SectorSummary {
        let (all_erased, next_erased_index) = match self.last_non_erased {
            Some(last) => (
                false,
                if last + 1 < RECORDS_PER_SECTOR {
                    last + 1
                } else {
                    RECORDS_PER_SECTOR
                },
            ),
            None => (true, 0),
        };
        SectorSummary {
            latest: self.latest,
            conflict: self.conflict,
            all_erased,
            next_erased_index,
        }
    }
}

pub fn scan_sector(sector: &[u8]) -> SectorSummary {
    let mut scanner = SectorScanner::new();
    let count = (sector.len() / RECORD_SIZE).min(RECORDS_PER_SECTOR);
    for idx in 0..count {
        let cell: &[u8; RECORD_SIZE] = sector[idx * RECORD_SIZE..(idx + 1) * RECORD_SIZE]
            .try_into()
            .unwrap();
        scanner.feed(cell);
    }
    scanner.finish()
}

/// Stream-scan a single 4 KiB sector directly from `flash` using 128 bytes of stack.
pub fn scan_sector_flash(
    flash: &mut (impl FlashRegion + ?Sized),
    sector_offset: u32,
) -> Result<SectorSummary, ServiceError> {
    let mut scanner = SectorScanner::new();
    let mut cell = [0u8; RECORD_SIZE];
    for idx in 0..RECORDS_PER_SECTOR {
        let off = sector_offset
            .checked_add((idx * RECORD_SIZE) as u32)
            .ok_or(ServiceError::InvalidParam)?;
        flash.read(off, &mut cell)?;
        scanner.feed(&cell);
    }
    Ok(scanner.finish())
}

pub fn combine_sector_scans(s0: SectorSummary, s1: SectorSummary) -> JournalScan {
    let status = match (s0.latest, s1.latest) {
        (None, None) => {
            if s0.all_erased && s1.all_erased {
                ScanStatus::Blank
            } else {
                ScanStatus::Corrupt
            }
        }
        (Some(r0), None) => {
            if s0.conflict {
                ScanStatus::Conflict
            } else {
                ScanStatus::Valid {
                    active_sector: 0,
                    record: r0,
                }
            }
        }
        (None, Some(r1)) => {
            if s1.conflict {
                ScanStatus::Conflict
            } else {
                ScanStatus::Valid {
                    active_sector: 1,
                    record: r1,
                }
            }
        }
        (Some(r0), Some(r1)) => {
            if r0.seq > r1.seq {
                if s0.conflict {
                    ScanStatus::Conflict
                } else {
                    ScanStatus::Valid {
                        active_sector: 0,
                        record: r0,
                    }
                }
            } else if r1.seq > r0.seq {
                if s1.conflict {
                    ScanStatus::Conflict
                } else {
                    ScanStatus::Valid {
                        active_sector: 1,
                        record: r1,
                    }
                }
            } else if r0 == r1 && !s0.conflict && !s1.conflict {
                ScanStatus::Valid {
                    active_sector: 0,
                    record: r0,
                }
            } else {
                ScanStatus::Conflict
            }
        }
    };
    JournalScan {
        sectors: [s0, s1],
        status,
    }
}

/// Scan both 4 KiB slot-state sectors in memory.
pub fn scan_journal(sector0: &[u8], sector1: &[u8]) -> JournalScan {
    combine_sector_scans(scan_sector(sector0), scan_sector(sector1))
}

/// Stream-scan both 4 KiB slot-state sectors from `flash` using 128 bytes of stack.
pub fn scan_journal_flash(
    flash: &mut (impl FlashRegion + ?Sized),
    s0_offset: u32,
    s1_offset: u32,
) -> Result<JournalScan, ServiceError> {
    let s0 = scan_sector_flash(flash, s0_offset)?;
    let s1 = scan_sector_flash(flash, s1_offset)?;
    Ok(combine_sector_scans(s0, s1))
}

/// Plan an append from RO pre-RAM (never erases a sector).
///
/// Returns `None` if the active sector has no free erased slots remaining.
pub fn plan_ro_append(scan: &JournalScan) -> Option<AppendPlan> {
    let active = match scan.status {
        ScanStatus::Valid { active_sector, .. } => active_sector,
        ScanStatus::Blank => 0,
        ScanStatus::Corrupt | ScanStatus::Conflict => return None,
    };
    let summary = scan.sectors[usize::from(active)];
    if summary.free_slots() == 0 {
        return None;
    }
    Some(AppendPlan::WriteInSector {
        sector: active,
        offset: summary.next_erased_index * RECORD_SIZE,
    })
}

/// Plan an append from CrabEFI/ramstage, rolling over to the spare sector when
/// fewer than `min_free_slots` remain in the active sector.
///
/// Before starting a trial with `N` attempts, pass `min_free_slots = N + 2` so
/// RO pre-RAM is guaranteed erased slots for every trial debit without erasing.
pub fn plan_ramstage_append(scan: &JournalScan, min_free_slots: usize) -> AppendPlan {
    let need = min_free_slots.max(1);
    let active = match scan.status {
        ScanStatus::Valid { active_sector, .. } => active_sector,
        ScanStatus::Blank => {
            return AppendPlan::WriteInSector {
                sector: 0,
                offset: 0,
            };
        }
        ScanStatus::Corrupt | ScanStatus::Conflict => {
            return AppendPlan::RolloverToSector {
                sector: 0,
                offset: 0,
            };
        }
    };
    let summary = scan.sectors[usize::from(active)];
    if summary.free_slots() >= need {
        AppendPlan::WriteInSector {
            sector: active,
            offset: summary.next_erased_index * RECORD_SIZE,
        }
    } else {
        let spare = 1 - active;
        AppendPlan::RolloverToSector {
            sector: spare,
            offset: 0,
        }
    }
}

/// Split an encoded 128-byte record into `(body_0_to_0x70, commit_line_0x70_to_0x80)`
/// for two-step NOR programming where the commit marker lives on its own
/// 16-byte ECC line.
pub const fn split_for_commit(encoded: &[u8; RECORD_SIZE]) -> (&[u8], &[u8]) {
    encoded.split_at(COMMIT_LINE_OFFSET)
}

/// Shared two-step verified NOR record writer (never erases).
///
/// Programs `0x00..0x70`, verifies readback, then programs the dedicated
/// 16-byte commit line `0x70..0x80` and verifies readback.
pub fn program_record_verified(
    flash: &mut (impl FlashRegion + ?Sized),
    abs_offset: u32,
    rec: &SlotStateRecord,
) -> Result<(), ServiceError> {
    let encoded = rec.encode();
    let (body, commit_line) = split_for_commit(&encoded);
    flash.program(abs_offset, body)?;
    let mut verify = [0u8; BODY_SIZE];
    flash.read(abs_offset, &mut verify)?;
    if verify != *body {
        return Err(ServiceError::HardwareError);
    }
    let commit_off = abs_offset
        .checked_add(COMMIT_LINE_OFFSET as u32)
        .ok_or(ServiceError::InvalidParam)?;
    flash.program(commit_off, commit_line)?;
    let mut commit_rb = [0u8; COMMIT_LINE_SIZE];
    flash.read(commit_off, &mut commit_rb)?;
    if commit_rb != *commit_line {
        return Err(ServiceError::HardwareError);
    }
    Ok(())
}

/// Erase a 4 KiB journal sector and verify every byte reads back as `0xFF`.
pub fn erase_sector_verified(
    flash: &mut (impl FlashRegion + ?Sized),
    sector_offset: u32,
) -> Result<(), ServiceError> {
    flash.erase(sector_offset, SECTOR_SIZE as u32)?;
    let mut cell = [0u8; RECORD_SIZE];
    for idx in 0..RECORDS_PER_SECTOR {
        let off = sector_offset
            .checked_add((idx * RECORD_SIZE) as u32)
            .ok_or(ServiceError::InvalidParam)?;
        flash.read(off, &mut cell)?;
        if !cell.iter().all(|&b| b == ERASED_BYTE) {
            return Err(ServiceError::HardwareError);
        }
    }
    Ok(())
}

/// Inputs for [`confirm_trial_and_sync`].
#[derive(Debug, Clone, Copy)]
pub struct TrialConfirm {
    pub layout: SlotLayout,
    pub running_slot: Slot,
    pub running_fw_version: u32,
    pub running_sec_version: u64,
    pub running_root_digest: [u8; 32],
    pub effective_floor: u64,
    pub has_tpm_floor: bool,
    pub trial_active: bool,
    pub sync_pending: bool,
}

/// Append a ramstage/CrabEFI journal transition, rolling over to the spare
/// sector when fewer than `min_free_slots` erased entries remain.
///
/// Only `Slot::Recovery` is permitted to heal a corrupt/conflicting journal.
pub fn append_ramstage_journal(
    flash: &mut (impl FlashRegion + ?Sized),
    layout: SlotLayout,
    running_slot: Slot,
    min_free_slots: usize,
    make_next: impl FnOnce(Option<SlotStateRecord>) -> Option<SlotStateRecord>,
) -> Result<SlotStateRecord, ServiceError> {
    let (s0_off, s1_off) = layout.state_sectors();
    let scan = scan_journal_flash(flash, s0_off, s1_off)?;
    let current = match scan.status {
        ScanStatus::Valid { record, .. } => Some(record),
        ScanStatus::Blank => None,
        ScanStatus::Corrupt | ScanStatus::Conflict => {
            if running_slot != Slot::Recovery {
                return Err(ServiceError::HardwareError);
            }
            erase_sector_verified(flash, s0_off)?;
            erase_sector_verified(flash, s1_off)?;
            None
        }
    };
    let next = make_next(current).ok_or(ServiceError::InvalidParam)?;
    let abs_offset = match plan_ramstage_append(&scan, min_free_slots) {
        AppendPlan::WriteInSector { sector, offset } => {
            let base = if sector == 0 { s0_off } else { s1_off };
            base.checked_add(offset as u32)
                .ok_or(ServiceError::InvalidParam)?
        }
        AppendPlan::RolloverToSector { sector, offset } => {
            let base = if sector == 0 { s0_off } else { s1_off };
            erase_sector_verified(flash, base)?;
            base.checked_add(offset as u32)
                .ok_or(ServiceError::InvalidParam)?
        }
    };
    program_record_verified(flash, abs_offset, &next)?;
    Ok(next)
}

/// Shared firmware-level trial confirmation and power-fail-safe sync-before-bump.
///
/// Used by both CrabEFI's `FstartCapsuleBackend` (UEFI) and non-UEFI RW stages:
/// - Reserves `min_free_slots = 2` on `Syncing` and `FloorPending` so RO always
///   has a free erased entry to finalize `Stable` without erasing.
/// - Erases destination sector 0 (`Root`) FIRST, copies/verifies body sectors
///   `SECTOR_SIZE..copy_len` (skipping sectors that already match), and writes
///   and verifies destination sector 0 (`Root`) LAST so a power cut mid-copy
///   never leaves a valid `Root` over a half-copied body.
pub fn confirm_trial_and_sync(
    flash: &mut (impl FlashRegion + ?Sized),
    cfg: &mut TrialConfirm,
) -> Result<bool, ServiceError> {
    if !cfg.trial_active && !cfg.sync_pending {
        return Ok(false);
    }
    // `Phase::Syncing` is only entered when an in-progress TPM rollback-floor
    // advance began erasing/copying the peer slot. Never silently cancel it to
    // `Stable` if the TPM disappears across a power cut — fail closed so
    // restoring the TPM resumes the copy and floor bump.
    if cfg.sync_pending && !cfg.has_tpm_floor {
        return Err(ServiceError::HardwareError);
    }
    let needs_floor_bump =
        cfg.sync_pending || (cfg.has_tpm_floor && cfg.running_sec_version > cfg.effective_floor);
    let mut sync_done = false;

    let running_slot = cfg.running_slot;
    let fw_ver = cfg.running_fw_version;
    let sec_ver = cfg.running_sec_version;
    let root_digest = cfg.running_root_digest;

    if needs_floor_bump
        && let Some(inactive_slot) = cfg.running_slot.other_rw()
        && let (Some(src), Some(dst)) = (
            cfg.layout.extent(cfg.running_slot),
            cfg.layout.extent(inactive_slot),
        )
    {
        if src.size != dst.size || src.size < SECTOR_SIZE as u32 {
            return Err(ServiceError::InvalidParam);
        }
        // Persist Phase::Syncing BEFORE erasing the fallback slot so a power cut
        // mid-copy resumes syncing instead of falling back to a half-erased slot.
        if !cfg.sync_pending {
            append_ramstage_journal(flash, cfg.layout, running_slot, 2, |current| {
                let base = current.unwrap_or_else(|| {
                    SlotStateRecord::initial(
                        running_slot,
                        Slot::Recovery,
                        fw_ver,
                        sec_ver,
                        &root_digest,
                    )
                });
                base.next_syncing()
            })?;
            cfg.sync_pending = true;
        }

        // Erase destination Root sector (offset 0) FIRST.
        flash.erase(dst.offset, SECTOR_SIZE as u32)?;

        // Copy and verify body sectors (SECTOR_SIZE..copy_len), skipping unchanged sectors.
        let copy_len = src.size;
        let mut src_buf = [0u8; SECTOR_SIZE];
        let mut dst_buf = [0u8; SECTOR_SIZE];
        let mut off = SECTOR_SIZE as u32;
        while off < copy_len {
            let src_off = src
                .offset
                .checked_add(off)
                .ok_or(ServiceError::InvalidParam)?;
            let dst_off = dst
                .offset
                .checked_add(off)
                .ok_or(ServiceError::InvalidParam)?;
            flash.read(src_off, &mut src_buf)?;
            flash.read(dst_off, &mut dst_buf)?;
            if dst_buf != src_buf {
                flash.erase(dst_off, SECTOR_SIZE as u32)?;
                if src_buf.iter().any(|&b| b != ERASED_BYTE) {
                    flash.program(dst_off, &src_buf)?;
                }
                flash.read(dst_off, &mut dst_buf)?;
                if dst_buf != src_buf {
                    return Err(ServiceError::HardwareError);
                }
            }
            off += SECTOR_SIZE as u32;
        }

        // Program and verify destination Root sector (offset 0) LAST.
        flash.read(src.offset, &mut src_buf)?;
        if src_buf.iter().any(|&b| b != ERASED_BYTE) {
            flash.program(dst.offset, &src_buf)?;
        }
        flash.read(dst.offset, &mut dst_buf)?;
        if dst_buf != src_buf {
            return Err(ServiceError::HardwareError);
        }
        sync_done = true;
    }

    append_ramstage_journal(flash, cfg.layout, running_slot, 2, |current| {
        let base = current.unwrap_or_else(|| {
            SlotStateRecord::initial(running_slot, Slot::Recovery, fw_ver, sec_ver, &root_digest)
        });
        base.next_confirm(sync_done, needs_floor_bump)
    })?;
    cfg.trial_active = false;
    cfg.sync_pending = false;
    Ok(true)
}

/// CRC32C (Castagnoli, reflected polynomial `0x82F6_3B78`).
pub fn crc32c(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in bytes {
        crc ^= u32::from(b);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0x82f6_3b78 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blank_sector() -> [u8; SECTOR_SIZE] {
        [ERASED_BYTE; SECTOR_SIZE]
    }

    fn write_record(sector: &mut [u8; SECTOR_SIZE], offset: usize, rec: &SlotStateRecord) {
        let enc = rec.encode();
        sector[offset..offset + RECORD_SIZE].copy_from_slice(&enc);
    }

    #[test]
    fn crc32c_standard_check_value() {
        assert_eq!(crc32c(b"123456789"), 0xe306_9283);
    }

    #[test]
    fn layout_validation_rejects_overlap_unaligned_and_mismatched_sizes() {
        let good = SlotLayout::RoAB {
            state_sector_0: 0x0000,
            state_sector_1: 0x1000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x4000,
            },
            slot_b: SlotExtent {
                offset: 0x6000,
                size: 0x4000,
            },
        };
        assert_eq!(good.validate(0xa000), Ok(()));
        assert_eq!(good.validate(0x9000), Err(LayoutError::OutOfBounds));

        let same_state = SlotLayout::RoA {
            state_sector_0: 0x0000,
            state_sector_1: 0x0000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x4000,
            },
        };
        assert_eq!(
            same_state.validate(0xa000),
            Err(LayoutError::OverlappingRegions)
        );

        let mismatch = SlotLayout::RoAB {
            state_sector_0: 0x0000,
            state_sector_1: 0x1000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x4000,
            },
            slot_b: SlotExtent {
                offset: 0x6000,
                size: 0x5000,
            },
        };
        assert_eq!(
            mismatch.validate(0x10000),
            Err(LayoutError::MismatchedSlotSizes)
        );

        let unaligned = SlotLayout::RoA {
            state_sector_0: 0x0000,
            state_sector_1: 0x1000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x1800,
            },
        };
        assert_eq!(unaligned.validate(0x10000), Err(LayoutError::Unaligned));
    }

    #[test]
    fn record_roundtrip_and_corruption_rejection() {
        let digest = [0xab; 32];
        let rec = SlotStateRecord::initial(Slot::A, Slot::B, 0x0001_0002, 7, &digest);
        let enc = rec.encode();
        assert_eq!(SlotStateRecord::parse(&enc).unwrap(), rec);

        // Uncommitted (power loss before commit line).
        let mut uncommitted = enc;
        uncommitted[COMMIT_OFFSET] = ERASED_BYTE;
        assert_eq!(
            SlotStateRecord::parse(&uncommitted),
            Err(RecordError::Uncommitted)
        );

        // Single bit flip in body fails CRC.
        let mut bad_crc = enc;
        bad_crc[12] ^= 0x01;
        assert_eq!(
            SlotStateRecord::parse(&bad_crc),
            Err(RecordError::CrcMismatch)
        );
    }

    #[test]
    fn distinguishes_blank_from_corrupt_and_detects_same_sector_conflict() {
        let mut s0 = blank_sector();
        let s1 = blank_sector();
        assert_eq!(scan_journal(&s0, &s1).status, ScanStatus::Blank);

        s0[0] = 0x00;
        assert_eq!(scan_journal(&s0, &s1).status, ScanStatus::Corrupt);

        // Same-sector equal-seq divergent records must be Conflict.
        let mut s0 = blank_sector();
        let r0 = SlotStateRecord::initial(Slot::A, Slot::B, 1, 1, &[0x11; 32]);
        let r1 = SlotStateRecord::initial(Slot::B, Slot::A, 2, 1, &[0x22; 32]);
        write_record(&mut s0, 0, &r0);
        write_record(&mut s0, RECORD_SIZE, &r1);
        assert_eq!(scan_journal(&s0, &s1).status, ScanStatus::Conflict);
    }

    #[test]
    fn torn_write_is_skipped_and_never_reused() {
        let mut s0 = blank_sector();
        let s1 = blank_sector();
        let d1 = [0x11; 32];
        let r1 = SlotStateRecord::initial(Slot::A, Slot::B, 1, 1, &d1);
        write_record(&mut s0, 0, &r1);

        let r2 = r1
            .next_trial(Slot::B, Slot::A, 3, 2, 1, &[0x22; 32])
            .unwrap();
        let mut torn = r2.encode();
        torn[COMMIT_OFFSET] = ERASED_BYTE;
        s0[RECORD_SIZE..2 * RECORD_SIZE].copy_from_slice(&torn);

        let scan = scan_journal(&s0, &s1);
        assert_eq!(
            scan.status,
            ScanStatus::Valid {
                active_sector: 0,
                record: r1,
            }
        );
        assert_eq!(
            plan_ro_append(&scan),
            Some(AppendPlan::WriteInSector {
                sector: 0,
                offset: 2 * RECORD_SIZE,
            })
        );
    }

    #[test]
    fn rollover_preserves_old_sector_and_guarantees_ro_headroom() {
        let mut s0 = blank_sector();
        let mut s1 = blank_sector();
        let d1 = [0x33; 32];
        let mut rec = SlotStateRecord::initial(Slot::A, Slot::B, 1, 1, &d1);

        // Fill 28 of 32 slots in sector 0 (4 free slots left).
        for idx in 0..28 {
            rec.seq = (idx + 1) as u64;
            write_record(&mut s0, idx * RECORD_SIZE, &rec);
        }

        let scan = scan_journal(&s0, &s1);
        let plan = plan_ramstage_append(&scan, usize::from(DEFAULT_TRIAL_ATTEMPTS) + 2);
        assert_eq!(
            plan,
            AppendPlan::RolloverToSector {
                sector: 1,
                offset: 0,
            }
        );

        let trial = rec
            .next_trial(Slot::B, Slot::A, DEFAULT_TRIAL_ATTEMPTS, 2, 2, &[0x44; 32])
            .unwrap();
        write_record(&mut s1, 0, &trial);

        let scan2 = scan_journal(&s0, &s1);
        assert_eq!(
            scan2.status,
            ScanStatus::Valid {
                active_sector: 1,
                record: trial,
            }
        );
        assert_eq!(
            plan_ro_append(&scan2),
            Some(AppendPlan::WriteInSector {
                sector: 1,
                offset: RECORD_SIZE,
            })
        );
    }

    #[test]
    fn trial_syncing_and_floor_pending_transitions() {
        let base = SlotStateRecord::initial(Slot::A, Slot::B, 1, 1, &[0x11; 32]);
        let t3 = base
            .next_trial(Slot::B, Slot::A, 3, 2, 2, &[0x22; 32])
            .unwrap();
        let t2 = t3.next_try_debit().unwrap();
        let syncing = t2.next_syncing().unwrap();
        assert_eq!(syncing.phase, Phase::Syncing);
        assert_eq!(syncing.active_slot, Slot::B);
        assert_eq!(syncing.fallback_slot, Slot::Recovery);
        assert!(syncing.is_confirmed());

        let fp = syncing.next_confirm(true, true).unwrap();
        assert_eq!(fp.phase, Phase::FloorPending);
        assert_eq!(fp.fallback_slot, Slot::A);
        assert!(fp.is_sync_done());

        let stable = fp.next_floor_finalized().unwrap();
        assert_eq!(stable.phase, Phase::Stable);
        assert_eq!(stable.fallback_slot, Slot::A);
    }

    struct MockFlash {
        data: alloc::vec::Vec<u8>,
        fail_after_writes: Option<usize>,
        writes: usize,
    }

    impl MockFlash {
        fn new(size: usize) -> Self {
            Self {
                data: alloc::vec![ERASED_BYTE; size],
                fail_after_writes: None,
                writes: 0,
            }
        }
    }

    impl FlashRegion for MockFlash {
        fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ServiceError> {
            let start = offset as usize;
            let end = start + buf.len();
            buf.copy_from_slice(&self.data[start..end]);
            Ok(())
        }

        fn program(&mut self, offset: u32, data: &[u8]) -> Result<(), ServiceError> {
            if let Some(limit) = self.fail_after_writes
                && self.writes >= limit
            {
                return Err(ServiceError::HardwareError);
            }
            self.writes += 1;
            let start = offset as usize;
            for (d, &s) in self.data[start..start + data.len()].iter_mut().zip(data) {
                *d &= s;
            }
            Ok(())
        }

        fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
            let start = offset as usize;
            let end = start + length as usize;
            self.data[start..end].fill(ERASED_BYTE);
            Ok(())
        }
    }

    #[test]
    fn confirm_trial_reserves_ro_slot_on_nearly_full_journal() {
        let layout = SlotLayout::RoAB {
            state_sector_0: 0,
            state_sector_1: SECTOR_SIZE as u32,
            slot_a: SlotExtent {
                offset: 2 * SECTOR_SIZE as u32,
                size: 2 * SECTOR_SIZE as u32,
            },
            slot_b: SlotExtent {
                offset: 4 * SECTOR_SIZE as u32,
                size: 2 * SECTOR_SIZE as u32,
            },
        };
        let mut flash = MockFlash::new(6 * SECTOR_SIZE);
        // Fill slot B with non-0xFF image bytes so sync copies both sectors.
        flash.data[4 * SECTOR_SIZE..5 * SECTOR_SIZE].fill(0x5A);
        flash.data[5 * SECTOR_SIZE..6 * SECTOR_SIZE].fill(0xA5);

        // Pre-fill sector 0 with RECORDS_PER_SECTOR - 1 records so only 1 slot remains!
        let mut rec = SlotStateRecord::initial(Slot::A, Slot::B, 1, 1, &[0x11; 32]);
        for i in 0..(RECORDS_PER_SECTOR - 2) {
            program_record_verified(&mut flash, (i * RECORD_SIZE) as u32, &rec).unwrap();
            rec.seq += 1;
        }
        let trial = rec
            .next_trial(Slot::B, Slot::A, 3, 2, 2, &[0x22; 32])
            .unwrap()
            .next_try_debit()
            .unwrap();
        program_record_verified(
            &mut flash,
            ((RECORDS_PER_SECTOR - 2) * RECORD_SIZE) as u32,
            &trial,
        )
        .unwrap();

        let mut cfg = TrialConfirm {
            layout,
            running_slot: Slot::B,
            running_fw_version: 2,
            running_sec_version: 2,
            running_root_digest: [0x22; 32],
            effective_floor: 1,
            has_tpm_floor: true,
            trial_active: true,
            sync_pending: false,
        };
        assert!(confirm_trial_and_sync(&mut flash, &mut cfg).unwrap());
        assert!(!cfg.trial_active && !cfg.sync_pending);

        // Sector 1 must now hold FloorPending AND still have free slots for RO to append Stable!
        let scan = scan_journal_flash(&mut flash, 0, SECTOR_SIZE as u32).unwrap();
        match scan.status {
            ScanStatus::Valid {
                active_sector,
                record,
            } => {
                assert_eq!(active_sector, 1);
                assert_eq!(record.phase, Phase::FloorPending);
                assert!(record.is_sync_done());
                assert!(plan_ro_append(&scan).is_some());
            }
            other => panic!("expected Valid FloorPending, got {other:?}"),
        }
        assert_eq!(
            flash.data[2 * SECTOR_SIZE..4 * SECTOR_SIZE],
            flash.data[4 * SECTOR_SIZE..6 * SECTOR_SIZE]
        );
    }

    #[test]
    fn confirm_trial_erases_root_first_and_resumes_after_power_cut() {
        let layout = SlotLayout::RoAB {
            state_sector_0: 0,
            state_sector_1: SECTOR_SIZE as u32,
            slot_a: SlotExtent {
                offset: 2 * SECTOR_SIZE as u32,
                size: 3 * SECTOR_SIZE as u32,
            },
            slot_b: SlotExtent {
                offset: 5 * SECTOR_SIZE as u32,
                size: 3 * SECTOR_SIZE as u32,
            },
        };
        let mut flash = MockFlash::new(8 * SECTOR_SIZE);
        flash.data[2 * SECTOR_SIZE..5 * SECTOR_SIZE].fill(0x11); // Old Slot A
        flash.data[5 * SECTOR_SIZE..6 * SECTOR_SIZE].fill(0xB0); // New Slot B Root
        flash.data[6 * SECTOR_SIZE..7 * SECTOR_SIZE].fill(0xB1); // New Slot B body 1
        flash.data[7 * SECTOR_SIZE..8 * SECTOR_SIZE].fill(0xB2); // New Slot B body 2

        let base = SlotStateRecord::initial(Slot::A, Slot::B, 1, 1, &[0x11; 32]);
        let trial = base
            .next_trial(Slot::B, Slot::A, 3, 2, 2, &[0x22; 32])
            .unwrap()
            .next_try_debit()
            .unwrap();
        program_record_verified(&mut flash, 0, &trial).unwrap();

        // Inject power cut after Syncing record (2 program calls: body + commit) + 1 body sector write.
        flash.writes = 0;
        flash.fail_after_writes = Some(3);
        let mut cfg = TrialConfirm {
            layout,
            running_slot: Slot::B,
            running_fw_version: 2,
            running_sec_version: 2,
            running_root_digest: [0x22; 32],
            effective_floor: 1,
            has_tpm_floor: true,
            trial_active: true,
            sync_pending: false,
        };
        assert!(confirm_trial_and_sync(&mut flash, &mut cfg).is_err());

        // Destination Root sector (Slot A offset 0) MUST be erased (0xFF), never stale or half-copied!
        assert!(
            flash.data[2 * SECTOR_SIZE..3 * SECTOR_SIZE]
                .iter()
                .all(|&b| b == ERASED_BYTE)
        );
        // Journal MUST be in Phase::Syncing so next boot resumes sync cleanly.
        let scan = scan_journal_flash(&mut flash, 0, SECTOR_SIZE as u32).unwrap();
        assert!(matches!(
            scan.status,
            ScanStatus::Valid { record, .. } if record.phase == Phase::Syncing
        ));

        // Resuming Phase::Syncing with the TPM absent MUST fail closed and
        // leave the journal in Phase::Syncing (never silently cancel to Stable).
        flash.fail_after_writes = None;
        let mut no_tpm_cfg = TrialConfirm {
            has_tpm_floor: false,
            trial_active: false,
            sync_pending: true,
            ..cfg
        };
        assert!(confirm_trial_and_sync(&mut flash, &mut no_tpm_cfg).is_err());
        let scan_still_syncing = scan_journal_flash(&mut flash, 0, SECTOR_SIZE as u32).unwrap();
        assert!(matches!(
            scan_still_syncing.status,
            ScanStatus::Valid { record, .. } if record.phase == Phase::Syncing
        ));

        // Resume after power + TPM restoration.
        let mut resume_cfg = TrialConfirm {
            trial_active: false,
            sync_pending: true,
            ..cfg
        };
        assert!(confirm_trial_and_sync(&mut flash, &mut resume_cfg).unwrap());
        assert_eq!(
            flash.data[2 * SECTOR_SIZE..5 * SECTOR_SIZE],
            flash.data[5 * SECTOR_SIZE..8 * SECTOR_SIZE]
        );
        let scan2 = scan_journal_flash(&mut flash, 0, SECTOR_SIZE as u32).unwrap();
        assert!(matches!(
            scan2.status,
            ScanStatus::Valid { record, .. } if record.phase == Phase::FloorPending && record.is_sync_done()
        ));
    }

    #[test]
    fn next_installing_rejects_unfinalized_phases() {
        let stable = SlotStateRecord::initial(Slot::A, Slot::B, 1, 1, &[0x11; 32]);
        let inst = stable
            .next_installing(Slot::B, Slot::A, 2, 2, &[0x22; 32])
            .expect("Stable -> Installing allowed");
        let trial = inst
            .next_trial(Slot::B, Slot::A, 3, 2, 2, &[0x22; 32])
            .unwrap();
        let syncing = trial.next_syncing().unwrap();
        let floor_pending = syncing.next_confirm(true, true).unwrap();
        let finalized = floor_pending.next_floor_finalized().unwrap();

        assert!(
            trial
                .next_installing(Slot::A, Slot::B, 3, 3, &[0x33; 32])
                .is_none()
        );
        assert!(
            syncing
                .next_installing(Slot::A, Slot::B, 3, 3, &[0x33; 32])
                .is_none()
        );
        assert!(
            floor_pending
                .next_installing(Slot::A, Slot::B, 3, 3, &[0x33; 32])
                .is_none()
        );
        assert!(
            finalized
                .next_installing(Slot::A, Slot::B, 3, 3, &[0x33; 32])
                .is_some()
        );
    }
}
