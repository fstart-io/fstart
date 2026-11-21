// SPDX-License-Identifier: MIT OR Apache-2.0
//! RO pre-RAM slot selector and authenticated slot handoff.
//!
//! Stream-scans the two 4 KiB `SLOT_STATE` sectors (128 bytes of stack, safe
//! for CAR), validates [`SlotLayout`], enforces the effective rollback floor,
//! debits trial attempts via append-only NOR writes (never erasing in RO),
//! authenticates the chosen RW slot's `Root` at slot offset 0, and produces an
//! [`ActiveSlotHandoff`] for downstream stages and CrabEFI.

use fstart_core::services::{BootMedia, FlashRegion};
use fstart_ffs::root::{AuthenticatedRoot, RootPolicy};
use fstart_ffs::slot_state::{
    AppendPlan, Phase, ScanStatus, Slot, SlotExtent, SlotLayout, SlotStateRecord, plan_ro_append,
    program_record_verified, scan_journal_flash,
};
use zerocopy::byteorder::{LE, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use crate::root::{SliceBootMedia, read_boot_root_with_digest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryReason {
    InvalidLayout,
    JournalConflict,
    JournalCorrupt,
    JournalReadError,
    NoValidRwSlot,
    RequestedRecovery,
    TpmPolicyFailure,
}

#[derive(Debug, Clone, Copy)]
#[allow(clippy::large_enum_variant)] // no_std: returned once per boot, no heap to Box
pub enum SlotDecision {
    BootRw {
        slot: Slot,
        extent: SlotExtent,
        root: AuthenticatedRoot,
        root_digest: [u8; 32],
        record: Option<SlotStateRecord>,
        trial_active: bool,
        sync_pending: bool,
    },
    BootRecovery(RecoveryReason),
}

/// Board policy for RO slot selection.
#[derive(Debug, Clone, Copy)]
pub struct SelectorPolicy {
    /// On a provisioned board, a completely blank journal goes to recovery
    /// rather than silently booting `Slot::A`.
    pub provisioned: bool,
}

/// Wire-safe handoff record carried from RO -> RW stages -> CrabEFI.
pub const HANDOFF_MAGIC: [u8; 4] = *b"FSSH";
pub const HANDOFF_VERSION: u8 = 1;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct ActiveSlotHandoff {
    pub magic: [u8; 4],
    pub version: u8,
    pub active_slot: u8,
    pub slot_count: u8,
    pub trial_active: u8,
    pub sync_pending: u8,
    pub reserved: [u8; 3],
    pub state_sector_0: U32<LE>,
    pub state_sector_1: U32<LE>,
    pub slot_a_offset: U32<LE>,
    pub slot_a_size: U32<LE>,
    pub slot_b_offset: U32<LE>,
    pub slot_b_size: U32<LE>,
    pub fw_version: U32<LE>,
    pub last_attempt_ver: U32<LE>,
    pub last_attempt_status: U32<LE>,
    pub effective_floor: U64<LE>,
    pub sec_version: U64<LE>,
    pub root_digest: [u8; 32],
}

impl ActiveSlotHandoff {
    #[allow(clippy::too_many_arguments)]
    pub fn from_decision(
        layout: SlotLayout,
        effective_floor: u64,
        slot: Slot,
        root: &AuthenticatedRoot,
        root_digest: [u8; 32],
        record: Option<SlotStateRecord>,
        trial_active: bool,
        sync_pending: bool,
    ) -> Self {
        let (s0, s1) = layout.state_sectors();
        let (slot_a, slot_b, slot_count) = match layout {
            SlotLayout::RoA { slot_a, .. } => (slot_a, SlotExtent { offset: 0, size: 0 }, 1),
            SlotLayout::RoAB { slot_a, slot_b, .. } => (slot_a, slot_b, 2),
        };
        let (last_ver, last_status) = match record {
            Some(r) => (r.last_attempt_ver, r.last_attempt_status),
            None => (root.root().fw_version, 0),
        };
        Self {
            magic: HANDOFF_MAGIC,
            version: HANDOFF_VERSION,
            active_slot: slot as u8,
            slot_count,
            trial_active: u8::from(trial_active),
            sync_pending: u8::from(sync_pending),
            reserved: [0; 3],
            state_sector_0: U32::new(s0),
            state_sector_1: U32::new(s1),
            slot_a_offset: U32::new(slot_a.offset),
            slot_a_size: U32::new(slot_a.size),
            slot_b_offset: U32::new(slot_b.offset),
            slot_b_size: U32::new(slot_b.size),
            fw_version: U32::new(root.root().fw_version),
            last_attempt_ver: U32::new(last_ver),
            last_attempt_status: U32::new(last_status),
            effective_floor: U64::new(effective_floor),
            sec_version: U64::new(root.root().security_version),
            root_digest,
        }
    }

    pub fn layout(&self) -> Option<SlotLayout> {
        if self.magic != HANDOFF_MAGIC || self.version != HANDOFF_VERSION {
            return None;
        }
        let slot_a = SlotExtent {
            offset: self.slot_a_offset.get(),
            size: self.slot_a_size.get(),
        };
        match self.slot_count {
            1 => Some(SlotLayout::RoA {
                state_sector_0: self.state_sector_0.get(),
                state_sector_1: self.state_sector_1.get(),
                slot_a,
            }),
            2 => Some(SlotLayout::RoAB {
                state_sector_0: self.state_sector_0.get(),
                state_sector_1: self.state_sector_1.get(),
                slot_a,
                slot_b: SlotExtent {
                    offset: self.slot_b_offset.get(),
                    size: self.slot_b_size.get(),
                },
            }),
            _ => None,
        }
    }

    pub fn slot(&self) -> Option<Slot> {
        Slot::from_u8(self.active_slot)
    }
}

fn verify_slot(
    media: &(impl BootMedia + ?Sized),
    extent: SlotExtent,
    base_policy: &RootPolicy<'_>,
) -> Option<(AuthenticatedRoot, [u8; 32])> {
    let slice = SliceBootMedia::new(media, extent.offset as usize, extent.size as usize)?;
    let policy = RootPolicy {
        image_family: base_policy.image_family,
        minimum_security_version: base_policy.minimum_security_version,
        image_size: extent.size as u64,
        max_directory_size: base_policy.max_directory_size,
        keys: base_policy.keys,
    };
    read_boot_root_with_digest(&slice, 0, &policy).ok()
}

fn try_fallback(
    layout: SlotLayout,
    preferred: Slot,
    media: &(impl BootMedia + ?Sized),
    policy: &RootPolicy<'_>,
    record: Option<SlotStateRecord>,
) -> SlotDecision {
    if let Some(extent) = layout.extent(preferred)
        && let Some((root, root_digest)) = verify_slot(media, extent, policy)
    {
        return SlotDecision::BootRw {
            slot: preferred,
            extent,
            root,
            root_digest,
            record,
            trial_active: false,
            sync_pending: false,
        };
    }
    SlotDecision::BootRecovery(RecoveryReason::NoValidRwSlot)
}

fn try_append_ro(
    flash: &mut (impl FlashRegion + ?Sized),
    s0_off: u32,
    s1_off: u32,
    plan: AppendPlan,
    rec: &SlotStateRecord,
) -> bool {
    let AppendPlan::WriteInSector { sector, offset } = plan else {
        return false;
    };
    let base = if sector == 0 { s0_off } else { s1_off };
    let Some(abs_offset) = base.checked_add(offset as u32) else {
        return false;
    };
    program_record_verified(flash, abs_offset, rec).is_ok()
}

/// Select and authenticate the active RW slot from RO.
///
/// `policy.minimum_security_version` must already incorporate
/// `max(trust.minimum_security_version(), tpm_nv_floor)`.
pub fn select_slot(
    layout: SlotLayout,
    selector_policy: SelectorPolicy,
    flash: &mut (impl FlashRegion + ?Sized),
    media: &(impl BootMedia + ?Sized),
    policy: &RootPolicy<'_>,
) -> SlotDecision {
    let Ok(capacity) = u32::try_from(media.size()) else {
        return SlotDecision::BootRecovery(RecoveryReason::InvalidLayout);
    };
    if layout.validate(capacity).is_err() {
        return SlotDecision::BootRecovery(RecoveryReason::InvalidLayout);
    }

    let (s0_off, s1_off) = layout.state_sectors();
    let Ok(scan) = scan_journal_flash(flash, s0_off, s1_off) else {
        return SlotDecision::BootRecovery(RecoveryReason::JournalReadError);
    };

    match scan.status {
        ScanStatus::Conflict => SlotDecision::BootRecovery(RecoveryReason::JournalConflict),
        ScanStatus::Corrupt => SlotDecision::BootRecovery(RecoveryReason::JournalCorrupt),
        ScanStatus::Blank => {
            if selector_policy.provisioned {
                return SlotDecision::BootRecovery(RecoveryReason::JournalCorrupt);
            }
            // Factory bring-up with blank journal: try Slot A, then Slot B, then recovery.
            if let Some(ext_a) = layout.extent(Slot::A)
                && let Some((root, root_digest)) = verify_slot(media, ext_a, policy)
            {
                return SlotDecision::BootRw {
                    slot: Slot::A,
                    extent: ext_a,
                    root,
                    root_digest,
                    record: None,
                    trial_active: false,
                    sync_pending: false,
                };
            }
            if let Some(ext_b) = layout.extent(Slot::B)
                && let Some((root, root_digest)) = verify_slot(media, ext_b, policy)
            {
                return SlotDecision::BootRw {
                    slot: Slot::B,
                    extent: ext_b,
                    root,
                    root_digest,
                    record: None,
                    trial_active: false,
                    sync_pending: false,
                };
            }
            SlotDecision::BootRecovery(RecoveryReason::NoValidRwSlot)
        }
        ScanStatus::Valid { record, .. } => match record.phase {
            Phase::Recovery => SlotDecision::BootRecovery(RecoveryReason::RequestedRecovery),
            Phase::Installing => {
                // Interrupted install: fall back to the untouched slot (or recovery in RO/A).
                try_fallback(layout, record.fallback_slot, media, policy, Some(record))
            }
            Phase::Stable | Phase::FloorPending | Phase::Syncing => {
                if let Some(extent) = layout.extent(record.active_slot)
                    && let Some((root, root_digest)) = verify_slot(media, extent, policy)
                    && record.matches_root_digest(&root_digest)
                {
                    return SlotDecision::BootRw {
                        slot: record.active_slot,
                        extent,
                        root,
                        root_digest,
                        record: Some(record),
                        trial_active: false,
                        sync_pending: record.phase == Phase::Syncing,
                    };
                }
                try_fallback(layout, record.fallback_slot, media, policy, Some(record))
            }
            Phase::Trial => {
                let verified = if record.tries_left > 0 {
                    layout
                        .extent(record.active_slot)
                        .and_then(|extent| {
                            verify_slot(media, extent, policy).map(|(r, d)| (extent, r, d))
                        })
                        .filter(|(_, _, digest)| record.matches_root_digest(digest))
                } else {
                    None
                };

                let Some((extent, root, root_digest)) = verified else {
                    // Trial exhausted OR candidate failed verification: verify
                    // the fallback slot first so the persisted Stable record
                    // binds the fallback slot's actual Root digest and versions
                    // (otherwise the *next* cold boot would reject it).
                    if let Some(fb_extent) = layout.extent(record.fallback_slot)
                        && let Some((fb_root, fb_digest)) = verify_slot(media, fb_extent, policy)
                    {
                        let persisted = record
                            .next_trial_exhausted(
                                fb_root.root().fw_version,
                                fb_root.root().security_version,
                                &fb_digest,
                            )
                            .and_then(|next| {
                                let plan = plan_ro_append(&scan)?;
                                try_append_ro(flash, s0_off, s1_off, plan, &next).then_some(next)
                            })
                            .unwrap_or(record);
                        return SlotDecision::BootRw {
                            slot: record.fallback_slot,
                            extent: fb_extent,
                            root: fb_root,
                            root_digest: fb_digest,
                            record: Some(persisted),
                            trial_active: false,
                            sync_pending: false,
                        };
                    }
                    if let Some(next) = record.next_trial_exhausted_to_recovery()
                        && let Some(plan) = plan_ro_append(&scan)
                    {
                        let _ = try_append_ro(flash, s0_off, s1_off, plan, &next);
                    }
                    return SlotDecision::BootRecovery(RecoveryReason::NoValidRwSlot);
                };

                // Commit the trial debit BEFORE jumping to RW. If the debit
                // cannot be persisted, do NOT jump to the untrusted trial slot.
                let Some(debited) = record.next_try_debit() else {
                    return try_fallback(layout, record.fallback_slot, media, policy, Some(record));
                };
                let Some(plan) = plan_ro_append(&scan) else {
                    return try_fallback(layout, record.fallback_slot, media, policy, Some(record));
                };
                if !try_append_ro(flash, s0_off, s1_off, plan, &debited) {
                    return try_fallback(layout, record.fallback_slot, media, policy, Some(record));
                }

                SlotDecision::BootRw {
                    slot: record.active_slot,
                    extent,
                    root,
                    root_digest,
                    record: Some(debited),
                    trial_active: true,
                    sync_pending: false,
                }
            }
        },
    }
}

#[cfg(all(test, feature = "ffs-signature"))]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use fstart_core::ffs::VerificationKey;
    use fstart_core::services::ServiceError;
    use fstart_ffs::root::{DirectoryRef, ROOT_SIZE, Root, SIGNED_SIZE};
    use fstart_ffs::slot_state::{ERASED_BYTE, SECTOR_SIZE, esrt_status};
    use std::vec;
    use std::vec::Vec;

    struct MemFlash(Vec<u8>);

    impl BootMedia for MemFlash {
        fn read_at(&self, offset: usize, buf: &mut [u8]) -> Result<usize, ServiceError> {
            let end = offset
                .checked_add(buf.len())
                .ok_or(ServiceError::InvalidParam)?;
            if end > self.0.len() {
                return Err(ServiceError::InvalidParam);
            }
            buf.copy_from_slice(&self.0[offset..end]);
            Ok(buf.len())
        }
        fn size(&self) -> usize {
            self.0.len()
        }
    }

    impl FlashRegion for MemFlash {
        fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ServiceError> {
            let off = offset as usize;
            let end = off
                .checked_add(buf.len())
                .ok_or(ServiceError::InvalidParam)?;
            if end > self.0.len() {
                return Err(ServiceError::InvalidParam);
            }
            buf.copy_from_slice(&self.0[off..end]);
            Ok(())
        }
        fn program(&mut self, offset: u32, data: &[u8]) -> Result<(), ServiceError> {
            let off = offset as usize;
            let end = off
                .checked_add(data.len())
                .ok_or(ServiceError::InvalidParam)?;
            if end > self.0.len() {
                return Err(ServiceError::InvalidParam);
            }
            for (dst, &src) in self.0[off..end].iter_mut().zip(data.iter()) {
                *dst &= src;
            }
            Ok(())
        }
        fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
            let off = offset as usize;
            let len = length as usize;
            if !off.is_multiple_of(SECTOR_SIZE)
                || !len.is_multiple_of(SECTOR_SIZE)
                || off + len > self.0.len()
            {
                return Err(ServiceError::InvalidParam);
            }
            self.0[off..off + len].fill(ERASED_BYTE);
            Ok(())
        }
    }

    fn signed_root(
        key: &SigningKey,
        fw_version: u32,
        sec_version: u64,
    ) -> ([u8; ROOT_SIZE], [u8; 32]) {
        let mut root = Root {
            security_version: sec_version,
            fw_version,
            image_family: *b"TEST-BOARD-00001",
            key_id: 0,
            descriptors: [None, None],
            directory: DirectoryRef {
                offset: ROOT_SIZE as u64,
                size: 64,
                digest: [0xaa; 32],
            },
            signature: [0; 64],
        };
        let unsigned = root.encode();
        root.signature = key.sign(&unsigned[..SIGNED_SIZE]).to_bytes();
        let bytes = root.encode();
        let digest = fstart_crypto::digest::hash_sha256(&bytes);
        (bytes, digest)
    }

    #[test]
    fn trial_debits_then_falls_back_to_slot_a_on_exhaustion() {
        let key = SigningKey::from_bytes(&[0x42; 32]);
        let vk = [VerificationKey::ed25519(0, key.verifying_key().to_bytes())];
        let policy = RootPolicy {
            image_family: *b"TEST-BOARD-00001",
            minimum_security_version: 1,
            image_size: 0x4000,
            max_directory_size: 0x2000,
            keys: &vk,
        };
        let layout = SlotLayout::RoAB {
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
        let sel = SelectorPolicy { provisioned: true };

        let mut flash = MemFlash(vec![ERASED_BYTE; 0xa000]);
        let (root_a, digest_a) = signed_root(&key, 1, 1);
        let (root_b, digest_b) = signed_root(&key, 2, 1);
        flash.0[0x2000..0x2000 + ROOT_SIZE].copy_from_slice(&root_a);
        flash.0[0x6000..0x6000 + ROOT_SIZE].copy_from_slice(&root_b);

        let init = SlotStateRecord::initial(Slot::A, Slot::B, 1, 1, &digest_a);
        let trial = init
            .next_trial(Slot::B, Slot::A, 2, 2, 1, &digest_b)
            .unwrap();
        program_record_verified(&mut flash, 0, &trial).unwrap();

        // Boot 1: tries 2 -> 1, boots Slot B in trial mode.
        let mut media_copy = MemFlash(flash.0.clone());
        match select_slot(layout, sel, &mut flash, &media_copy, &policy) {
            SlotDecision::BootRw {
                slot,
                trial_active,
                record,
                ..
            } => {
                assert_eq!(slot, Slot::B);
                assert!(trial_active);
                assert_eq!(record.unwrap().tries_left, 1);
            }
            other => panic!("unexpected {other:?}"),
        }

        // Boot 2: tries 1 -> 0, boots Slot B in trial mode.
        media_copy = MemFlash(flash.0.clone());
        match select_slot(layout, sel, &mut flash, &media_copy, &policy) {
            SlotDecision::BootRw {
                slot,
                trial_active,
                record,
                ..
            } => {
                assert_eq!(slot, Slot::B);
                assert!(trial_active);
                assert_eq!(record.unwrap().tries_left, 0);
            }
            other => panic!("unexpected {other:?}"),
        }

        // Boot 3: tries == 0 -> exhausted! Falls back to Slot A with UNSUCCESSFUL
        // and records Slot A's own Root digest and versions.
        media_copy = MemFlash(flash.0.clone());
        match select_slot(layout, sel, &mut flash, &media_copy, &policy) {
            SlotDecision::BootRw {
                slot,
                trial_active,
                record,
                ..
            } => {
                assert_eq!(slot, Slot::A);
                assert!(!trial_active);
                let rec = record.unwrap();
                assert_eq!(rec.phase, Phase::Stable);
                assert_eq!(rec.fw_version, 1);
                assert_eq!(rec.root_digest, digest_a);
                assert_eq!(rec.last_attempt_ver, 2);
                assert_eq!(rec.last_attempt_status, esrt_status::UNSUCCESSFUL);
            }
            other => panic!("unexpected {other:?}"),
        }

        // Boot 4: subsequent cold boot after fallback must still boot Slot A
        // cleanly as Stable (not bounce to Recovery due to stale trial digest).
        media_copy = MemFlash(flash.0.clone());
        match select_slot(layout, sel, &mut flash, &media_copy, &policy) {
            SlotDecision::BootRw {
                slot,
                trial_active,
                record,
                ..
            } => {
                assert_eq!(slot, Slot::A);
                assert!(!trial_active);
                assert_eq!(record.unwrap().phase, Phase::Stable);
            }
            other => panic!("expected Stable Slot A on second boot after fallback, got {other:?}"),
        }
    }

    #[test]
    fn provisioned_board_recovers_on_blank_journal() {
        let key = SigningKey::from_bytes(&[0x42; 32]);
        let vk = [VerificationKey::ed25519(0, key.verifying_key().to_bytes())];
        let policy = RootPolicy {
            image_family: *b"TEST-BOARD-00001",
            minimum_security_version: 1,
            image_size: 0x4000,
            max_directory_size: 0x2000,
            keys: &vk,
        };
        let layout = SlotLayout::RoA {
            state_sector_0: 0x0000,
            state_sector_1: 0x1000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x4000,
            },
        };
        let mut flash = MemFlash(vec![ERASED_BYTE; 0x6000]);
        let (root_a, _) = signed_root(&key, 1, 1);
        flash.0[0x2000..0x2000 + ROOT_SIZE].copy_from_slice(&root_a);
        let media_copy = MemFlash(flash.0.clone());

        match select_slot(
            layout,
            SelectorPolicy { provisioned: true },
            &mut flash,
            &media_copy,
            &policy,
        ) {
            SlotDecision::BootRecovery(RecoveryReason::JournalCorrupt) => {}
            other => panic!("expected recovery on provisioned blank journal, got {other:?}"),
        }
    }

    #[test]
    fn ro_a_falls_back_to_recovery_when_trial_exhausted() {
        let key = SigningKey::from_bytes(&[0x42; 32]);
        let vk = [VerificationKey::ed25519(0, key.verifying_key().to_bytes())];
        let policy = RootPolicy {
            image_family: *b"TEST-BOARD-00001",
            minimum_security_version: 1,
            image_size: 0x4000,
            max_directory_size: 0x2000,
            keys: &vk,
        };
        let layout = SlotLayout::RoA {
            state_sector_0: 0x0000,
            state_sector_1: 0x1000,
            slot_a: SlotExtent {
                offset: 0x2000,
                size: 0x4000,
            },
        };
        let mut flash = MemFlash(vec![ERASED_BYTE; 0x6000]);
        let (root_a, digest_a) = signed_root(&key, 2, 1);
        flash.0[0x2000..0x2000 + ROOT_SIZE].copy_from_slice(&root_a);

        let mut trial = SlotStateRecord::initial(Slot::A, Slot::Recovery, 1, 1, &digest_a)
            .next_trial(Slot::A, Slot::Recovery, 1, 2, 1, &digest_a)
            .unwrap();
        trial.tries_left = 0;
        program_record_verified(&mut flash, 0, &trial).unwrap();

        let media_copy = MemFlash(flash.0.clone());
        match select_slot(
            layout,
            SelectorPolicy { provisioned: true },
            &mut flash,
            &media_copy,
            &policy,
        ) {
            SlotDecision::BootRecovery(RecoveryReason::NoValidRwSlot) => {}
            other => panic!("expected recovery, got {other:?}"),
        }
    }
}
