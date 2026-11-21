// SPDX-License-Identifier: MIT OR Apache-2.0
//! fstart `CapsuleBackend` for CrabEFI Capsule-on-Disk updates.
//!
//! Bridges CrabEFI's FMP/PKCS#7 capsule applier directly to
//! [`fstart_core::services::FlashRegion`], fstart's RO-owned [`SlotLayout`],
//! two-sector `SLOT_STATE` journal, and Ed25519 `Root` preflight verification.

use crabefi::{CapsuleBackend, FirmwareInfo, FirmwareSlots, FmapRegion, StorageError};
use fstart_core::ffs::VerificationKey;
use fstart_core::services::{FlashRegion, ServiceError};
use fstart_ffs::root::{ROOT_SIZE, RootPolicy, authenticate_root};
use fstart_ffs::slot_state::{
    AppendPlan, DEFAULT_TRIAL_ATTEMPTS, SECTOR_SIZE, ScanStatus, Slot, SlotExtent, SlotLayout,
    SlotStateRecord, erase_sector_verified, plan_ramstage_append, program_record_verified,
    scan_journal_flash,
};

pub const REGION_RW_A: &str = "RW_A";
pub const REGION_RW_B: &str = "RW_B";
/// Logical single-image RMAP region resolved by [`FstartCapsuleBackend`] to
/// the inactive slot (in RO/A/B) or `RW_A` (in RO/A recovery), so one capsule
/// carries a single RW FFS image rather than duplicating it for A and B.
pub const REGION_RW_SLOT: &str = "RW_SLOT";

fn map_service_error(err: ServiceError) -> StorageError {
    match err {
        ServiceError::InvalidParam => StorageError::InvalidArgument,
        ServiceError::NotSupported => StorageError::NotSupported,
        ServiceError::Timeout => StorageError::Timeout,
        _ => StorageError::IoError,
    }
}

/// Verified metadata for a staged inactive slot awaiting `activate_slot`.
#[derive(Debug, Clone, Copy)]
struct StagedSlot {
    target: Slot,
    fw_version: u32,
    sec_version: u64,
    root_digest: [u8; 32],
}

/// Configuration for [`FstartCapsuleBackend`], derived from RO's `TrustBlock`
/// and authenticated slot handoff.
pub struct CapsuleConfig<'a> {
    pub flash_capacity: u32,
    pub layout: SlotLayout,
    pub running_slot: Slot,
    pub running_fw_version: u32,
    pub running_sec_version: u64,
    pub running_root_digest: [u8; 32],
    pub effective_floor: u64,
    pub trial_active: bool,
    pub sync_pending: bool,
    pub image_family: [u8; 16],
    pub root_keys: &'a [VerificationKey],
    pub firmware_info: FirmwareInfo,
    pub trust_certs: &'a [&'a [u8]],
}

pub struct FstartCapsuleBackend<'a, F, L> {
    flash: F,
    lock_fn: L,
    layout: SlotLayout,
    running_slot: Slot,
    running_fw_version: u32,
    running_sec_version: u64,
    running_root_digest: [u8; 32],
    effective_floor: u64,
    trial_active: bool,
    sync_pending: bool,
    image_family: [u8; 16],
    root_keys: &'a [VerificationKey],
    firmware_info: FirmwareInfo,
    trust_certs: &'a [&'a [u8]],
    regions: [FmapRegion; 3],
    region_count: usize,
    staged: Option<StagedSlot>,
    /// Latched once a trial has been committed during this boot so a second
    /// capsule in `\EFI\UpdateCapsule\` cannot clobber the committed trial.
    activated_this_boot: bool,
}

fn make_fmap_region(name: &str, extent: SlotExtent) -> FmapRegion {
    FmapRegion {
        name: name.parse().unwrap_or_default(),
        offset: extent.offset,
        size: extent.size,
    }
}

impl<'a, F: FlashRegion, L: FnMut() -> Result<(), StorageError>> FstartCapsuleBackend<'a, F, L> {
    /// Construct a validated capsule backend with a mandatory hardware SPI
    /// lockdown callback `lock_fn`.
    pub fn new(flash: F, lock_fn: L, config: CapsuleConfig<'a>) -> Result<Self, StorageError> {
        config
            .layout
            .validate(config.flash_capacity)
            .map_err(|_| StorageError::InvalidArgument)?;

        let empty = make_fmap_region("", SlotExtent { offset: 0, size: 0 });
        let (regions, region_count) = match config.layout {
            SlotLayout::RoA { slot_a, .. } => (
                [
                    make_fmap_region(REGION_RW_SLOT, slot_a),
                    make_fmap_region(REGION_RW_A, slot_a),
                    empty,
                ],
                2,
            ),
            SlotLayout::RoAB { slot_a, slot_b, .. } => {
                let target_ext = match config.running_slot {
                    Slot::A => slot_b,
                    Slot::B | Slot::Recovery => slot_a,
                };
                (
                    [
                        make_fmap_region(REGION_RW_SLOT, target_ext),
                        make_fmap_region(REGION_RW_A, slot_a),
                        make_fmap_region(REGION_RW_B, slot_b),
                    ],
                    3,
                )
            }
        };
        Ok(Self {
            flash,
            lock_fn,
            layout: config.layout,
            running_slot: config.running_slot,
            running_fw_version: config.running_fw_version,
            running_sec_version: config.running_sec_version,
            running_root_digest: config.running_root_digest,
            effective_floor: config.effective_floor,
            trial_active: config.trial_active,
            sync_pending: config.sync_pending,
            image_family: config.image_family,
            root_keys: config.root_keys,
            firmware_info: config.firmware_info,
            trust_certs: config.trust_certs,
            regions,
            region_count,
            staged: None,
            activated_this_boot: false,
        })
    }

    #[cfg(test)]
    pub(crate) fn flash_mut(&mut self) -> &mut F {
        &mut self.flash
    }

    /// Engage hardware SPI write-protection locks before launching any
    /// third-party EFI code, option ROM, or bootloader.
    pub fn lock_flash(&mut self) -> Result<(), StorageError> {
        (self.lock_fn)()
    }

    fn slot_for_name(&self, name: &str) -> Option<(Slot, SlotExtent)> {
        let slot = match name {
            REGION_RW_SLOT => match self.running_slot {
                Slot::A if self.layout.is_ab() => Slot::B,
                Slot::B if self.layout.is_ab() => Slot::A,
                Slot::Recovery => Slot::A,
                _ => return None,
            },
            REGION_RW_A => Slot::A,
            REGION_RW_B => Slot::B,
            _ => return None,
        };
        let extent = self.layout.extent(slot)?;
        Some((slot, extent))
    }

    fn append_journal(
        &mut self,
        min_free_slots: usize,
        make_next: impl FnOnce(Option<SlotStateRecord>) -> Option<SlotStateRecord>,
    ) -> Result<SlotStateRecord, StorageError> {
        let (s0_off, s1_off) = self.layout.state_sectors();
        let scan =
            scan_journal_flash(&mut self.flash, s0_off, s1_off).map_err(map_service_error)?;
        let current = match scan.status {
            ScanStatus::Valid { record, .. } => Some(record),
            ScanStatus::Blank => None,
            // Only RO recovery may heal a corrupt/conflicting journal by
            // erasing both sectors and seeding a fresh record.
            ScanStatus::Corrupt | ScanStatus::Conflict => {
                if self.running_slot != Slot::Recovery {
                    return Err(StorageError::IoError);
                }
                erase_sector_verified(&mut self.flash, s0_off).map_err(map_service_error)?;
                erase_sector_verified(&mut self.flash, s1_off).map_err(map_service_error)?;
                None
            }
        };
        let next = make_next(current).ok_or(StorageError::InvalidArgument)?;
        let plan = plan_ramstage_append(&scan, min_free_slots);

        let abs_offset = match plan {
            AppendPlan::WriteInSector { sector, offset } => {
                let base = if sector == 0 { s0_off } else { s1_off };
                base.checked_add(offset as u32)
                    .ok_or(StorageError::InvalidArgument)?
            }
            AppendPlan::RolloverToSector { sector, offset } => {
                let base = if sector == 0 { s0_off } else { s1_off };
                erase_sector_verified(&mut self.flash, base).map_err(map_service_error)?;
                base.checked_add(offset as u32)
                    .ok_or(StorageError::InvalidArgument)?
            }
        };

        program_record_verified(&mut self.flash, abs_offset, &next).map_err(map_service_error)?;
        Ok(next)
    }

    /// Firmware-level trial confirmation and power-fail-safe sync-before-bump.
    ///
    /// Call on normal (non-capsule) boots once the platform reaches the health
    /// point before locking SPI flash and launching third-party EFI code.
    ///
    /// - If `running_sec_version == effective_floor`: marks the trial `Stable`
    ///   without touching the inactive slot or TPM NV.
    /// - If `running_sec_version > effective_floor`:
    ///   - In A/B mode: **first** persists `Phase::Syncing` (pointing at the
    ///     confirmed running slot so a power cut mid-copy resumes syncing
    ///     instead of falling back to the half-erased slot), then copies every
    ///     4 KiB sector of the running slot to the inactive slot and verifies
    ///     **every sector byte-for-byte**, then persists `Phase::FloorPending`.
    ///   - In RO/A mode: persists `Phase::FloorPending` directly.
    pub fn confirm_trial_if_active(&mut self) -> Result<bool, StorageError> {
        if !self.trial_active && !self.sync_pending {
            return Ok(false);
        }
        let needs_floor_bump = self.running_sec_version > self.effective_floor;
        let mut sync_done = false;

        let running_slot = self.running_slot;
        let fw_ver = self.running_fw_version;
        let sec_ver = self.running_sec_version;
        let root_digest = self.running_root_digest;

        if needs_floor_bump
            && let Some(inactive_slot) = self.running_slot.other_rw()
            && let (Some(src), Some(dst)) = (
                self.layout.extent(self.running_slot),
                self.layout.extent(inactive_slot),
            )
        {
            // Persist Phase::Syncing BEFORE erasing the fallback slot!
            if !self.sync_pending {
                self.append_journal(2, |current| {
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
                self.sync_pending = true;
            }

            // Erase destination Root sector (offset 0) FIRST so a power cut
            // mid-copy never leaves a valid Root over a half-copied body.
            self.flash
                .erase(dst.offset, SECTOR_SIZE as u32)
                .map_err(map_service_error)?;

            // Copy and verify body sectors (SECTOR_SIZE..src.size) byte-for-byte.
            let mut src_buf = [0u8; SECTOR_SIZE];
            let mut dst_buf = [0u8; SECTOR_SIZE];
            let mut off = SECTOR_SIZE as u32;
            while off < src.size {
                let src_off = src
                    .offset
                    .checked_add(off)
                    .ok_or(StorageError::InvalidArgument)?;
                let dst_off = dst
                    .offset
                    .checked_add(off)
                    .ok_or(StorageError::InvalidArgument)?;
                self.flash
                    .read(src_off, &mut src_buf)
                    .map_err(map_service_error)?;
                self.flash
                    .erase(dst_off, SECTOR_SIZE as u32)
                    .map_err(map_service_error)?;
                self.flash
                    .program(dst_off, &src_buf)
                    .map_err(map_service_error)?;
                self.flash
                    .read(dst_off, &mut dst_buf)
                    .map_err(map_service_error)?;
                if dst_buf != src_buf {
                    return Err(StorageError::IoError);
                }
                off += SECTOR_SIZE as u32;
            }

            // Copy and verify the Root sector (offset 0) LAST.
            self.flash
                .read(src.offset, &mut src_buf)
                .map_err(map_service_error)?;
            self.flash
                .program(dst.offset, &src_buf)
                .map_err(map_service_error)?;
            self.flash
                .read(dst.offset, &mut dst_buf)
                .map_err(map_service_error)?;
            if dst_buf != src_buf {
                return Err(StorageError::IoError);
            }
            sync_done = true;
        }

        self.append_journal(2, |current| {
            let base = current.unwrap_or_else(|| {
                SlotStateRecord::initial(
                    running_slot,
                    Slot::Recovery,
                    fw_ver,
                    sec_ver,
                    &root_digest,
                )
            });
            base.next_confirm(sync_done, needs_floor_bump)
        })?;
        self.trial_active = false;
        self.sync_pending = false;
        Ok(true)
    }
}

impl<F: FlashRegion, L: FnMut() -> Result<(), StorageError>> CapsuleBackend
    for FstartCapsuleBackend<'_, F, L>
{
    fn firmware_info(&self) -> Option<&FirmwareInfo> {
        Some(&self.firmware_info)
    }

    fn capsule_trust_store(&self) -> &[&[u8]] {
        self.trust_certs
    }

    fn fmap_regions(&mut self) -> &[FmapRegion] {
        &self.regions[..self.region_count]
    }

    fn running_firmware_offset(&self) -> Option<u32> {
        self.layout.extent(self.running_slot).map(|e| e.offset)
    }

    fn firmware_slots(&mut self) -> Option<FirmwareSlots> {
        if !self.layout.is_ab() || self.running_slot == Slot::Recovery {
            return None;
        }
        let inactive = self.running_slot.other_rw()?;
        let running_ext = self.layout.extent(self.running_slot)?;
        let inactive_ext = self.layout.extent(inactive)?;
        let running_name = if self.running_slot == Slot::A {
            REGION_RW_A
        } else {
            REGION_RW_B
        };
        let inactive_name = if inactive == Slot::A {
            REGION_RW_A
        } else {
            REGION_RW_B
        };
        Some(FirmwareSlots {
            running: make_fmap_region(running_name, running_ext),
            inactive: make_fmap_region(inactive_name, inactive_ext),
        })
    }

    fn firmware_slots_expected(&mut self) -> bool {
        self.layout.is_ab() && self.running_slot != Slot::Recovery
    }

    fn write_firmware_region(
        &mut self,
        region_name: &str,
        offset: u32,
        data: &[u8],
    ) -> Result<(), StorageError> {
        // Refuse installing while a trial or sync is unconfirmed, or after a
        // capsule has already been activated in this boot (prevents a second
        // capsule file from clobbering the committed trial before reset).
        if self.trial_active || self.sync_pending || self.activated_this_boot {
            return Err(StorageError::AccessDenied);
        }

        let (target, extent) = self
            .slot_for_name(region_name)
            .ok_or(StorageError::InvalidArgument)?;

        // Never overwrite the running slot (in either RO/A/B or RO/A).
        // RO/A updates must be applied from RO recovery (`running_slot == Recovery`).
        if target == self.running_slot {
            return Err(StorageError::WriteProtected);
        }

        // Enforce whole-slot write at offset 0 so preflight auth cannot be bypassed.
        if offset != 0 || data.len() != extent.size as usize || data.len() < ROOT_SIZE {
            return Err(StorageError::InvalidArgument);
        }

        // Preflight Ed25519 Root verification BEFORE erasing anything.
        let policy = RootPolicy {
            image_family: self.image_family,
            minimum_security_version: self.effective_floor,
            image_size: extent.size as u64,
            max_directory_size: 64 * 1024,
            keys: self.root_keys,
        };
        let auth = authenticate_root(&policy, &data[..ROOT_SIZE])
            .map_err(|_| StorageError::InvalidArgument)?;
        let fw_version = auth.root().fw_version;
        let sec_version = auth.root().security_version;
        if fw_version < self.firmware_info.lowest_supported_version {
            return Err(StorageError::InvalidArgument);
        }
        let root_digest = fstart_crypto::digest::hash_sha256(&data[..ROOT_SIZE]);

        let fallback = if self.layout.is_ab() && self.running_slot != Slot::Recovery {
            self.running_slot
        } else {
            Slot::Recovery
        };
        let running_slot = self.running_slot;
        let running_fw = self.running_fw_version;
        let running_sec = self.running_sec_version;
        let running_digest = self.running_root_digest;
        self.append_journal(usize::from(DEFAULT_TRIAL_ATTEMPTS) + 3, |current| {
            let base = current.unwrap_or_else(|| {
                SlotStateRecord::initial(
                    running_slot,
                    fallback,
                    running_fw,
                    running_sec,
                    &running_digest,
                )
            });
            base.next_installing(target, fallback, fw_version, sec_version, &root_digest)
        })?;

        self.flash
            .erase(extent.offset, extent.size)
            .map_err(map_service_error)?;
        self.flash
            .program(extent.offset, data)
            .map_err(map_service_error)?;

        let staged = StagedSlot {
            target,
            fw_version,
            sec_version,
            root_digest,
        };

        // In RO/A or Recovery mode, CrabEFI uses its direct-apply path (where
        // firmware_slots() is None) and does not call verify_region or
        // activate_slot. Perform full readback verification and commit Trial
        // here so the freshly written slot boots on reset.
        if !self.layout.is_ab() || self.running_slot == Slot::Recovery {
            let mut verify_buf = [0u8; SECTOR_SIZE];
            let mut off = 0u32;
            while (off as usize) < data.len() {
                let chunk = (data.len() - off as usize).min(SECTOR_SIZE);
                let abs = extent
                    .offset
                    .checked_add(off)
                    .ok_or(StorageError::InvalidArgument)?;
                self.flash
                    .read(abs, &mut verify_buf[..chunk])
                    .map_err(map_service_error)?;
                if verify_buf[..chunk] != data[off as usize..off as usize + chunk] {
                    return Err(StorageError::IoError);
                }
                off += chunk as u32;
            }
            self.append_journal(usize::from(DEFAULT_TRIAL_ATTEMPTS) + 2, |current| {
                let base = current?;
                base.next_trial(
                    staged.target,
                    fallback,
                    DEFAULT_TRIAL_ATTEMPTS,
                    staged.fw_version,
                    staged.sec_version,
                    &staged.root_digest,
                )
            })?;
            self.activated_this_boot = true;
            return Ok(());
        }

        self.staged = Some(staged);
        Ok(())
    }

    fn read_firmware_region(
        &mut self,
        region_name: &str,
        offset: u32,
        buffer: &mut [u8],
    ) -> Result<(), StorageError> {
        let (_, extent) = self
            .slot_for_name(region_name)
            .ok_or(StorageError::InvalidArgument)?;
        let end = offset
            .checked_add(u32::try_from(buffer.len()).map_err(|_| StorageError::InvalidArgument)?)
            .ok_or(StorageError::InvalidArgument)?;
        if end > extent.size {
            return Err(StorageError::InvalidArgument);
        }
        let abs = extent
            .offset
            .checked_add(offset)
            .ok_or(StorageError::InvalidArgument)?;
        self.flash.read(abs, buffer).map_err(map_service_error)
    }

    fn activate_slot(&mut self, slot: &FmapRegion) -> Result<(), StorageError> {
        let (target, extent) = self
            .slot_for_name(slot.name.as_str())
            .ok_or(StorageError::InvalidArgument)?;
        let staged = self.staged.ok_or(StorageError::InvalidArgument)?;
        if staged.target != target {
            return Err(StorageError::InvalidArgument);
        }

        // Re-read and re-authenticate the on-flash Root before activating.
        let mut root_buf = [0u8; ROOT_SIZE];
        self.flash
            .read(extent.offset, &mut root_buf)
            .map_err(map_service_error)?;
        let on_flash_digest = fstart_crypto::digest::hash_sha256(&root_buf);
        if on_flash_digest != staged.root_digest {
            return Err(StorageError::IoError);
        }

        let fallback = if self.layout.is_ab() && self.running_slot != Slot::Recovery {
            self.running_slot
        } else {
            Slot::Recovery
        };
        self.append_journal(usize::from(DEFAULT_TRIAL_ATTEMPTS) + 2, |current| {
            let base = current?;
            base.next_trial(
                staged.target,
                fallback,
                DEFAULT_TRIAL_ATTEMPTS,
                staged.fw_version,
                staged.sec_version,
                &staged.root_digest,
            )
        })?;
        self.staged = None;
        self.activated_this_boot = true;
        Ok(())
    }

    fn platform_owns_trial(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use fstart_ffs::root::{DirectoryRef, Root, SIGNED_SIZE};
    use fstart_ffs::slot_state::{ERASED_BYTE, Phase, scan_journal};
    use std::vec;
    use std::vec::Vec;

    struct MockFlash {
        data: Vec<u8>,
    }

    impl MockFlash {
        fn new(size: usize) -> Self {
            Self {
                data: vec![ERASED_BYTE; size],
            }
        }
    }

    impl FlashRegion for MockFlash {
        fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ServiceError> {
            let off = offset as usize;
            let end = off
                .checked_add(buf.len())
                .ok_or(ServiceError::InvalidParam)?;
            if end > self.data.len() {
                return Err(ServiceError::InvalidParam);
            }
            buf.copy_from_slice(&self.data[off..end]);
            Ok(())
        }
        fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
            let off = offset as usize;
            let len = length as usize;
            if !off.is_multiple_of(SECTOR_SIZE)
                || !len.is_multiple_of(SECTOR_SIZE)
                || off + len > self.data.len()
            {
                return Err(ServiceError::InvalidParam);
            }
            self.data[off..off + len].fill(ERASED_BYTE);
            Ok(())
        }
        fn program(&mut self, offset: u32, data: &[u8]) -> Result<(), ServiceError> {
            let off = offset as usize;
            let end = off
                .checked_add(data.len())
                .ok_or(ServiceError::InvalidParam)?;
            if end > self.data.len() {
                return Err(ServiceError::InvalidParam);
            }
            for (dst, &src) in self.data[off..end].iter_mut().zip(data.iter()) {
                *dst &= src;
            }
            Ok(())
        }
    }

    fn make_slot_image(key: &SigningKey, fw_version: u32, sec_version: u64) -> (Vec<u8>, [u8; 32]) {
        let mut root = Root {
            security_version: sec_version,
            fw_version,
            image_family: *b"MJ11-TEST-FAMILY",
            key_id: 0,
            descriptors: [None, None],
            directory: DirectoryRef {
                offset: ROOT_SIZE as u64,
                size: 64,
                digest: [0x55; 32],
            },
            signature: [0; 64],
        };
        let unsigned = root.encode();
        root.signature = key.sign(&unsigned[..SIGNED_SIZE]).to_bytes();
        let root_bytes = root.encode();
        let digest = fstart_crypto::digest::hash_sha256(&root_bytes);
        let mut image = vec![0xaa; 0x2000];
        image[..ROOT_SIZE].copy_from_slice(&root_bytes);
        (image, digest)
    }

    #[test]
    fn ab_install_verify_activate_confirm_and_sync_before_bump() {
        let key = SigningKey::from_bytes(&[0x17; 32]);
        let vk = [VerificationKey::ed25519(0, key.verifying_key().to_bytes())];
        let layout = SlotLayout::RoAB {
            state_sector_0: 0x0000,
            state_sector_1: 0x1000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x2000,
            },
            slot_b: SlotExtent {
                offset: 0x4000,
                size: 0x2000,
            },
        };

        let (img_v1, dig_v1) = make_slot_image(&key, 1, 1);
        let (img_v2, dig_v2) = make_slot_image(&key, 2, 2);

        let mut flash = MockFlash::new(0x6000);
        flash.data[0x2000..0x4000].copy_from_slice(&img_v1);

        let mut backend = FstartCapsuleBackend::new(
            flash,
            || Ok(()),
            CapsuleConfig {
                flash_capacity: 0x6000,
                layout,
                running_slot: Slot::A,
                running_fw_version: 1,
                running_sec_version: 1,
                running_root_digest: dig_v1,
                effective_floor: 1,
                trial_active: false,
                sync_pending: false,
                image_family: *b"MJ11-TEST-FAMILY",
                root_keys: &vk,
                firmware_info: FirmwareInfo {
                    guid: [0x11; 16],
                    version: 1,
                    lowest_supported_version: 1,
                    fw_size: 0x2000,
                },
                trust_certs: &[],
            },
        )
        .unwrap();

        // Running slot A must be rejected.
        assert_eq!(
            backend.write_firmware_region(REGION_RW_A, 0, &img_v2),
            Err(StorageError::WriteProtected)
        );
        // Non-zero offset write must be rejected.
        assert_eq!(
            backend.write_firmware_region(REGION_RW_SLOT, 64, &img_v2[64..]),
            Err(StorageError::InvalidArgument)
        );
        // Tampered image must be rejected before erasing B.
        let mut bad_img = img_v2.clone();
        bad_img[10] ^= 0xff;
        assert_eq!(
            backend.write_firmware_region(REGION_RW_SLOT, 0, &bad_img),
            Err(StorageError::InvalidArgument)
        );
        assert!(
            backend.flash_mut().data[0x4000..0x6000]
                .iter()
                .all(|&b| b == ERASED_BYTE)
        );

        // Valid v2 writes via logical REGION_RW_SLOT -> Slot B, verifies readback, activates trial.
        backend
            .write_firmware_region(REGION_RW_SLOT, 0, &img_v2)
            .unwrap();
        let mut rb = vec![0u8; img_v2.len()];
        backend
            .read_firmware_region(REGION_RW_SLOT, 0, &mut rb)
            .unwrap();
        assert_eq!(rb, img_v2);

        let slots = backend.firmware_slots().unwrap();
        backend.activate_slot(&slots.inactive).unwrap();

        // A second capsule write in the same boot after activation must be rejected.
        assert_eq!(
            backend.write_firmware_region(REGION_RW_SLOT, 0, &img_v2),
            Err(StorageError::AccessDenied)
        );

        let scan = scan_journal(
            &backend.flash.data[0x0000..0x1000],
            &backend.flash.data[0x1000..0x2000],
        );
        let ScanStatus::Valid { record, .. } = scan.status else {
            panic!("expected valid trial record");
        };
        assert_eq!(record.phase, Phase::Trial);
        assert_eq!(record.active_slot, Slot::B);
        assert_eq!(record.tries_left, DEFAULT_TRIAL_ATTEMPTS);

        // Next boot: running Slot B in trial mode with sec_ver=2 > floor=1.
        // Installing a new capsule while trial_active must be refused!
        let flash2 = backend.flash;
        let mut backend2 = FstartCapsuleBackend::new(
            flash2,
            || Ok(()),
            CapsuleConfig {
                flash_capacity: 0x6000,
                layout,
                running_slot: Slot::B,
                running_fw_version: 2,
                running_sec_version: 2,
                running_root_digest: dig_v2,
                effective_floor: 1,
                trial_active: true,
                sync_pending: false,
                image_family: *b"MJ11-TEST-FAMILY",
                root_keys: &vk,
                firmware_info: FirmwareInfo {
                    guid: [0x11; 16],
                    version: 2,
                    lowest_supported_version: 1,
                    fw_size: 0x2000,
                },
                trust_certs: &[],
            },
        )
        .unwrap();
        assert_eq!(
            backend2.write_firmware_region(REGION_RW_SLOT, 0, &img_v2),
            Err(StorageError::AccessDenied)
        );

        // confirm_trial_if_active writes Syncing -> copies & verifies B->A -> writes FloorPending.
        assert_eq!(backend2.confirm_trial_if_active(), Ok(true));
        assert_eq!(&backend2.flash.data[0x2000..0x4000], &img_v2[..]);
        let scan2 = scan_journal(
            &backend2.flash.data[0x0000..0x1000],
            &backend2.flash.data[0x1000..0x2000],
        );
        let ScanStatus::Valid { record: r2, .. } = scan2.status else {
            panic!("expected valid FloorPending record");
        };
        assert_eq!(r2.phase, Phase::FloorPending);
        assert_eq!(r2.fallback_slot, Slot::A);
        assert!(r2.is_confirmed());
        assert!(r2.is_sync_done());
    }

    #[test]
    fn crabefi_trial_check_and_rmap_delegate_to_platform_journal() {
        let key = SigningKey::from_bytes(&[0x29; 32]);
        let vk = [VerificationKey::ed25519(0, key.verifying_key().to_bytes())];
        let layout = SlotLayout::RoAB {
            state_sector_0: 0x0000,
            state_sector_1: 0x1000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x2000,
            },
            slot_b: SlotExtent {
                offset: 0x4000,
                size: 0x2000,
            },
        };
        let (img_v1, dig_v1) = make_slot_image(&key, 1, 1);
        let (img_v2, _) = make_slot_image(&key, 2, 1);

        let mut flash = MockFlash::new(0x6000);
        flash.data[0x2000..0x4000].copy_from_slice(&img_v1);

        let mut backend = FstartCapsuleBackend::new(
            flash,
            || Ok(()),
            CapsuleConfig {
                flash_capacity: 0x6000,
                layout,
                running_slot: Slot::A,
                running_fw_version: 1,
                running_sec_version: 1,
                running_root_digest: dig_v1,
                effective_floor: 1,
                trial_active: false,
                sync_pending: false,
                image_family: *b"MJ11-TEST-FAMILY",
                root_keys: &vk,
                firmware_info: FirmwareInfo {
                    guid: [0x11; 16],
                    version: 1,
                    lowest_supported_version: 1,
                    fw_size: 0x2000,
                },
                trust_certs: &[],
            },
        )
        .unwrap();

        // CrabEFI's own EFI-variable trial check must be a no-op because
        // platform_owns_trial() == true.
        assert!(!crabefi::efi::capsule::trial::check(&mut backend));

        // Build RMAP-suffixed single-image payload ("RW_SLOT\n") and verify
        // CrabEFI's RMAP validator maps it to the inactive slot (RW_B).
        let mut rmap_payload = img_v2.clone();
        let manifest = b"RW_SLOT\n";
        rmap_payload.extend_from_slice(manifest);
        rmap_payload.extend_from_slice(&(manifest.len() as u32).to_le_bytes());

        let approved = crabefi::efi::capsule::rmap::parse_and_validate_rmap(
            &rmap_payload,
            backend.fmap_regions(),
        )
        .unwrap();
        assert_eq!(approved.len(), 1);
        assert_eq!(approved[0].name.as_str(), REGION_RW_SLOT);
        let slots = backend.firmware_slots().unwrap();
        assert_eq!(approved[0].offset, slots.inactive.offset);
        assert_eq!(approved[0].size, slots.inactive.size);
    }

    #[test]
    fn ro_a_recovery_write_verifies_and_activates_trial_directly() {
        let key = SigningKey::from_bytes(&[0x31; 32]);
        let vk = [VerificationKey::ed25519(0, key.verifying_key().to_bytes())];
        let layout = SlotLayout::RoA {
            state_sector_0: 0x0000,
            state_sector_1: 0x1000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x2000,
            },
        };
        let (img_v2, dig_v2) = make_slot_image(&key, 2, 1);
        let flash = MockFlash::new(0x4000);

        let mut backend = FstartCapsuleBackend::new(
            flash,
            || Ok(()),
            CapsuleConfig {
                flash_capacity: 0x4000,
                layout,
                running_slot: Slot::Recovery,
                running_fw_version: 0,
                running_sec_version: 0,
                running_root_digest: [0; 32],
                effective_floor: 1,
                trial_active: false,
                sync_pending: false,
                image_family: *b"MJ11-TEST-FAMILY",
                root_keys: &vk,
                firmware_info: FirmwareInfo {
                    guid: [0x11; 16],
                    version: 0,
                    lowest_supported_version: 1,
                    fw_size: 0x2000,
                },
                trust_certs: &[],
            },
        )
        .unwrap();

        assert!(backend.firmware_slots().is_none());
        backend
            .write_firmware_region(REGION_RW_SLOT, 0, &img_v2)
            .unwrap();
        let scan = scan_journal(
            &backend.flash.data[0x0000..0x1000],
            &backend.flash.data[0x1000..0x2000],
        );
        let ScanStatus::Valid { record, .. } = scan.status else {
            panic!("expected Trial record after RO/A recovery write");
        };
        assert_eq!(record.phase, Phase::Trial);
        assert_eq!(record.active_slot, Slot::A);
        assert_eq!(record.fallback_slot, Slot::Recovery);
        assert_eq!(record.root_digest, dig_v2);
        // Second capsule in same boot must be refused.
        assert_eq!(
            backend.write_firmware_region(REGION_RW_SLOT, 0, &img_v2),
            Err(StorageError::AccessDenied)
        );
    }
}
