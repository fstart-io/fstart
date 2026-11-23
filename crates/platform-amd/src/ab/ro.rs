// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD RO/A/B immutable RO selector stage (PSP BIOS directory entry `0x62`).
//!
//! Runs in PSP-copied DRAM at `0x75f8_0000..0x7600_0000` (entered via
//! `amd_psp_reset.S`):
//! 1. Brings up ECAM, early FCH SPI100 speedups (`fch::spi::reset` on SP3;
//!    SPI100 prefetch check on Siena), board LPC/eSPI console, and an early
//!    Write-Protected (WP) MTRR over the lower 16 MiB flash read window.
//! 2. Reads the build-patched [`TrustBlock`] (and on SP3, the build-patched
//!    [`FlashAnchor`] for total SPI flash size).
//! 3. Probes an optional discrete FIFO TPM 2.0 (`Board::DISCRETE_TPM`): reads
//!    or factory-provisions the monotonic rollback floor NV index
//!    (`0x0140_0001`), finalizes `Phase::FloorPending` only after verifying
//!    both RW slots, and unconditionally write-locks the NV index before
//!    leaving RO.
//! 4. Runs [`fstart_stage::slot::select_slot`] against `SLOT_STATE` (accessed
//!    strictly via an 8 KiB bounded SPI100 `Flash<Tsc>`) and the WP-cached
//!    lower 16 MiB ROM window, loads the authenticated RW stage into fixed
//!    DRAM (`0x75c8_0000..0x75f8_0000`) via `load_bootstrap` (falling back to
//!    the peer RW slot if payload verification fails).
//! 5. Logs verification + load latency, tears down the temporary WP MTRR and
//!    flushes caches (`WBINVD`), writes [`AmdAbHandoff`] at `0x75af_0000`, and
//!    jumps directly to RW `_start_ram` (`0x75c8_0000`) in 64-bit long mode.

use super::handoff::{
    AmdAbHandoff, HANDOFF_MAGIC, HANDOFF_VERSION, ROM3_ABSENT, ROM3_LINEAR, ROM3_PAGED, slot_name,
};
use super::{
    AB_HANDOFF_ADDR, AB_RO_FLASH_OFFSET, AB_ROLLBACK_NV_INDEX, AB_RW_IMAGE_BASE, AB_RW_IMAGE_SIZE,
    SIENA_AB_SLOT_A_OFFSET, SIENA_AB_SLOT_B_OFFSET, SIENA_AB_SLOT_SIZE, SIENA_AB_STATE_SECTOR_0,
    SIENA_AB_STATE_SECTOR_1, SP3_AB_SLOT_A_OFFSET, SP3_AB_SLOT_B_OFFSET, SP3_AB_SLOT_SIZE,
    SP3_AB_STATE_SECTOR_0, SP3_AB_STATE_SECTOR_1, lower_half_probe, tpm_err_parts,
};
use crate::board::Board;
use crate::sp3::entry;
use crate::sp3::silicon::{self, Generation, hw};
use fstart_amdfw_format::anchor::FlashAnchor;
use fstart_core::ffs::trust::{TRUST_SIZE, TrustBlock};
use fstart_core::services::boot_media::MemoryMapped;
use fstart_core::services::{BootMedia, FlashRegion, ServiceError, Timer};
use fstart_crypto::digest::Sha256Hasher;
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::fch::rom3::{self, FlashMap, Rom3};
use fstart_driver_amd::fch::spi::flash::{Extent, Flash};
use fstart_driver_amd::fch::{self, FchHw};
use fstart_driver_tpm::tis::{self, Tis};
use fstart_driver_tpm::tpm20::{self, FloorAdvance, StartupType};
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_ffs::root::{AuthenticatedRoot, BootstrapDescriptor, BootstrapRole, RootPolicy};
use fstart_ffs::slot_state::{
    AppendPlan, JournalScan, Phase, ScanStatus, Slot, SlotExtent, SlotLayout, plan_ro_append,
    program_record_verified, scan_journal_flash,
};
use fstart_stage::boot::{Measurement, MemoryPolicy, MemoryWindow, VerifiedExecutable};
use fstart_stage::directory::MAX_DIRECTORY_SIZE;
use fstart_stage::root::{SliceBootMedia, read_boot_root_with_digest};
use fstart_stage::slot::{
    ActiveSlotHandoff, RecoveryReason, SelectorPolicy, SlotDecision, select_slot,
};
use tock_registers::interfaces::{ReadWriteable, Readable};
use zerocopy::IntoBytes;
use zerocopy::byteorder::{U32, U64};

/// Flash anchor patch site filled by `halved_flash::patch_reset_image_with_trust`
/// on SP3 so RO knows the total SPI part size (16 MiB vs 32 MiB). Unused on
/// Siena (fixed 16 MiB window).
#[repr(transparent)]
struct AnchorSite(core::cell::UnsafeCell<FlashAnchor>);

// SAFETY: patched offline by the image builder; RO only reads it via volatile MMIO.
unsafe impl Sync for AnchorSite {}

#[used]
static RO_FLASH_ANCHOR: AnchorSite =
    AnchorSite(core::cell::UnsafeCell::new(FlashAnchor::placeholder()));

/// Physical memory read byte-wise (identity-mapped by RO's page tables).
fn read_phys(address: u64, buf: &mut [u8]) {
    for (b, a) in buf.iter_mut().zip(address..) {
        // SAFETY: identity-mapped flash window or RO's own build-patched anchor.
        *b = unsafe { fstart_core::mmio::read8(a as *const u8) };
    }
}

/// Adapter translating absolute flash offsets inside `[base, base + size)` to
/// `0..size` offsets on a strictly bounded SPI100 [`Flash`] covering only the
/// `SLOT_STATE` sectors.
///
/// This guarantees RO's SPI100 driver can never erase or program any flash
/// sector outside the two 4 KiB `SLOT_STATE` sectors while allowing
/// `fstart_ffs::slot_state` helpers to address the journal by absolute offset.
struct BoundedStateFlash {
    inner: Flash<Tsc>,
    base: u32,
}

impl BoundedStateFlash {
    fn rel(&self, offset: u32) -> Result<u32, ServiceError> {
        offset
            .checked_sub(self.base)
            .ok_or(ServiceError::InvalidParam)
    }
}

impl FlashRegion for BoundedStateFlash {
    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ServiceError> {
        let rel = self.rel(offset)?;
        self.inner.read(rel, buf)
    }

    fn program(&mut self, offset: u32, data: &[u8]) -> Result<(), ServiceError> {
        let rel = self.rel(offset)?;
        self.inner.program(rel, data)
    }

    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
        let rel = self.rel(offset)?;
        self.inner.erase(rel, length)
    }
}

fn recovery_reason_str(reason: RecoveryReason) -> &'static str {
    match reason {
        RecoveryReason::InvalidLayout => "invalid_layout",
        RecoveryReason::JournalConflict => "journal_conflict",
        RecoveryReason::JournalCorrupt => "journal_corrupt",
        RecoveryReason::JournalReadError => "journal_read_error",
        RecoveryReason::NoValidRwSlot => "no_valid_rw_slot",
        RecoveryReason::RequestedRecovery => "requested_recovery",
        RecoveryReason::TpmPolicyFailure => "tpm_policy_failure",
    }
}

/// Authenticate a slot's signed `Root`, verify its `Mainstage` bootstrap
/// descriptor bounds, and stream-hash the stored RW stage against
/// `descriptor.stored_digest`.
fn verify_slot_bootable<M: BootMedia>(
    media: &M,
    extent: SlotExtent,
    base_policy: &RootPolicy<'_>,
    min_sec_version: u64,
    expected_digest: Option<&[u8; 32]>,
) -> Option<(AuthenticatedRoot, [u8; 32], BootstrapDescriptor)> {
    let slice = SliceBootMedia::new(media, extent.offset as usize, extent.size as usize)?;
    let policy = RootPolicy {
        image_family: base_policy.image_family,
        minimum_security_version: base_policy.minimum_security_version.max(min_sec_version),
        image_size: extent.size as u64,
        max_directory_size: base_policy.max_directory_size,
        keys: base_policy.keys,
    };
    let (root, digest) = read_boot_root_with_digest(&slice, 0, &policy).ok()?;
    if root.root().security_version < min_sec_version {
        return None;
    }
    if let Some(expected) = expected_digest
        && digest != *expected
    {
        return None;
    }
    let descriptor = root
        .root()
        .descriptors
        .iter()
        .flatten()
        .find(|d| d.role == BootstrapRole::Mainstage)
        .copied()?;
    if descriptor.load_addr != AB_RW_IMAGE_BASE || descriptor.loaded_size > AB_RW_IMAGE_SIZE {
        return None;
    }
    let end = descriptor.offset.checked_add(descriptor.stored_size)?;
    if end > extent.size as u64 {
        return None;
    }
    let mut hasher = Sha256Hasher::new();
    let mut buf = [0u8; 4096];
    let mut pos = descriptor.offset as usize;
    let mut remaining = descriptor.stored_size as usize;
    while remaining > 0 {
        let chunk = remaining.min(buf.len());
        slice.read_at(pos, &mut buf[..chunk]).ok()?;
        hasher.update(&buf[..chunk]);
        pos += chunk;
        remaining -= chunk;
    }
    if hasher.finalize() != descriptor.stored_digest {
        return None;
    }
    Some((root, digest, descriptor))
}

fn try_load_slot<M: BootMedia>(
    media: &M,
    extent: SlotExtent,
    descriptor: &BootstrapDescriptor,
) -> Result<VerifiedExecutable, fstart_stage::boot::LoadError> {
    let Some(slice) = SliceBootMedia::new(media, extent.offset as usize, extent.size as usize)
    else {
        return Err(fstart_stage::boot::LoadError::Bounds);
    };
    let writable = [MemoryWindow {
        start: AB_RW_IMAGE_BASE,
        size: AB_RW_IMAGE_SIZE,
    }];
    let load_policy = MemoryPolicy {
        writable: &writable,
        reserved: &[],
        entry_alignment: 1,
    };
    let codec = const { fstart_stage::boot::codec(crate::compression::PAYLOAD) };
    // SAFETY:
    // - `writable` covers exclusively the RW stage DRAM window
    //   (`0x75c8_0000..0x75f8_0000`), disjoint from RO's page tables, scratch
    //   RAM, handoff page (`0x75af_0000`), Siena's APOB (`0x7590_0000`), and
    //   RO's own image (`0x75f8_0000..0x7600_0000`).
    // - `scratch` (`0x7530_0000..0x7580_0000`, 5 MiB) lies inside the 16 MiB
    //   write-back early DRAM window and is compile-time asserted disjoint from
    //   SP3 RO (`..0x752c_0000`), Siena RO (`..0x750d_0000`), Siena APOB
    //   (`0x7590_0000..`), handoff, RW, and RO.
    unsafe {
        let scratch = core::slice::from_raw_parts_mut(
            crate::ab::AB_RW_SCRATCH_BASE as *mut u8,
            crate::ab::AB_RW_SCRATCH_SIZE as usize,
        );
        fstart_stage::boot::load_bootstrap_with_scratch(
            &slice,
            descriptor,
            &load_policy,
            scratch,
            codec,
            &Measurement::NONE,
        )
    }
}

fn append_ro_record<F: FlashRegion>(
    flash: &mut F,
    layout: SlotLayout,
    scan: &JournalScan,
    record: &fstart_ffs::slot_state::SlotStateRecord,
) -> bool {
    let Some(AppendPlan::WriteInSector { sector, offset }) = plan_ro_append(scan) else {
        return false;
    };
    let (s0, s1) = layout.state_sectors();
    let base = if sector == 0 { s0 } else { s1 };
    let Some(abs) = base.checked_add(offset as u32) else {
        return false;
    };
    program_record_verified(flash, abs, record).is_ok()
}

fn halt() -> ! {
    fstart_log::flush();
    fstart_arch::x86::boot::halt()
}

/// Shared RO selector core for SP3 and Siena once early FCH/SPI/console and
/// the lower 16 MiB flash window are established.
#[allow(clippy::too_many_arguments)]
fn select_and_boot_rw<B: Board>(
    label: &'static str,
    timer: Tsc,
    layout: SlotLayout,
    flash_size: u32,
    rom_window_base: u64,
    rom_page: u8,
    rom3_mode: u8,
) -> ! {
    const LOWER_HALF_SIZE: u64 = 0x100_0000;

    // Enable temporary Write-Protected (WP) MTRR over the lower 16 MiB flash
    // window BEFORE hashing and decompressing the RW slot.
    // SAFETY: BSP-only in RO before APs run; cleared with WBINVD before jump.
    let wp_enabled = match unsafe {
        fstart_arch::x86::mtrr::set_rom_window_wp(rom_window_base, LOWER_HALF_SIZE, true)
    } {
        Ok(()) => true,
        Err(_) => {
            fstart_log::warn!(
                "{}: unable to enable early WP MTRR at {:#x}",
                label,
                rom_window_base
            );
            false
        }
    };

    let t_start = timer.timestamp_us();

    // Read build-patched TrustBlock from RO image via volatile byte reads.
    let mut trust_bytes = [0u8; TRUST_SIZE];
    let trust_raw = fstart_stage::fstart_trust_bytes();
    for (dst, src) in trust_bytes.iter_mut().zip(trust_raw.iter()) {
        // SAFETY: build-patched static in RO image; volatile read prevents
        // folding link-time placeholder bytes.
        *dst = unsafe { fstart_core::mmio::read8(src as *const u8) };
    }
    let Some(trust) = TrustBlock::parse(&trust_bytes).filter(|t| t.key_count.get() > 0) else {
        fstart_log::error!("{}: missing or invalid TrustBlock", label);
        halt();
    };

    let Some(ab_cfg) = B::AB_UPDATE else {
        fstart_log::error!("{}: board has no Board::AB_UPDATE configuration", label);
        halt();
    };

    if layout.validate(AB_RO_FLASH_OFFSET).is_err() {
        fstart_log::error!("{}: invalid A/B slot layout", label);
        halt();
    }

    // Bind SPI100 Flash strictly to the two 4 KiB SLOT_STATE sectors and probe
    // command-engine addressing against a lower-half-only region (never RO,
    // which is duplicated verbatim in the upper 16 MiB half on 32 MiB parts).
    let (s0, s1) = layout.state_sectors();
    let (probe_off, probe_bytes) = lower_half_probe(layout, rom_window_base);
    let state_extent = Extent {
        offset: s0,
        size: (s1 - s0) + 0x1000,
    };
    // SAFETY: FCH decodes SPI100 at SPI_BASE; RO runs in DRAM and owns SPI100 exclusively.
    let Ok(raw_state_flash) = (unsafe { Flash::new(timer, state_extent, probe_off, &probe_bytes) })
    else {
        fstart_log::error!("{}: SPI100 Flash init failed", label);
        halt();
    };
    if !raw_state_flash.writable() {
        fstart_log::error!(
            "{}: SPI100 command engine is not writable; cannot update SLOT_STATE",
            label
        );
        halt();
    }
    let mut state_flash = BoundedStateFlash {
        inner: raw_state_flash,
        base: s0,
    };
    // SAFETY: lower 16 MiB ROM window is identity-mapped read-only flash;
    // normal loads benefit from the temporary WP MTRR.
    let rom_media =
        unsafe { MemoryMapped::from_raw_addr(rom_window_base, LOWER_HALF_SIZE as usize) };

    let initial_scan = scan_journal_flash(&mut state_flash, s0, s1);

    // Optional discrete FIFO TPM 2.0 rollback floor check + lock.
    let mut tpm_present = false;
    let mut tpm_floor: u64 = 0;

    if B::DISCRETE_TPM {
        // SAFETY: FCH decodes 0xfed4_0000 to LPC/eSPI TPM when fitted.
        let mut tis = unsafe { Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
        if let Some(id) = tis.probe()
            && !tis.is_crb()
        {
            fstart_log::info!(
                "{}: TPM {:#x}:{:#x} rev {}",
                label,
                id.vendor,
                id.device,
                id.revision,
            );
            match tpm20::startup(&mut tis, StartupType::Clear) {
                Ok(()) | Err(fstart_driver_tpm::Error::Code(tpm20::rc::INITIALIZE)) => {}
                Err(e) => {
                    let (kind, code) = tpm_err_parts(e);
                    fstart_log::error!("{}: TPM2_Startup failed: {} ({:#x})", label, kind, code);
                    halt();
                }
            }
            if let Err(e) = tpm20::self_test(&mut tis, false) {
                let (kind, code) = tpm_err_parts(e);
                fstart_log::error!("{}: TPM2_SelfTest failed: {} ({:#x})", label, kind, code);
                halt();
            }

            match tpm20::read_rollback_floor(&mut tis, AB_ROLLBACK_NV_INDEX) {
                Ok(floor) => {
                    tpm_present = true;
                    tpm_floor = floor;
                    fstart_log::info!("{}: TPM rollback floor = {}", label, tpm_floor);
                }
                Err(fstart_driver_tpm::Error::Code(code))
                    if tpm20::rc::base(code) == tpm20::rc::HANDLE
                        || code == tpm20::rc::NV_UNINITIALIZED =>
                {
                    let allow_factory = match &initial_scan {
                        Ok(scan) => matches!(
                            scan.status,
                            ScanStatus::Valid { record, .. }
                                if record.seq == 1 && record.phase == Phase::Stable
                        ),
                        Err(_) => false,
                    };
                    if !allow_factory {
                        fstart_log::error!(
                            "{}: TPM rollback NV index undefined on provisioned device",
                            label,
                        );
                        halt();
                    }
                    let factory_policy = RootPolicy {
                        image_family: trust.image_family,
                        minimum_security_version: trust.minimum_security_version.get(),
                        image_size: 0,
                        max_directory_size: MAX_DIRECTORY_SIZE,
                        keys: trust.valid_keys(),
                    };
                    let Some((auth_a, _, _)) = layout.extent(Slot::A).and_then(|ext| {
                        verify_slot_bootable(
                            &rom_media,
                            ext,
                            &factory_policy,
                            trust.minimum_security_version.get(),
                            None,
                        )
                    }) else {
                        fstart_log::error!(
                            "{}: cannot factory-provision TPM floor without authentic Slot A",
                            label,
                        );
                        halt();
                    };
                    let init_floor = auth_a
                        .root()
                        .security_version
                        .max(trust.minimum_security_version.get());
                    let provisioned = if code == tpm20::rc::NV_UNINITIALIZED {
                        tpm20::resume_uninitialized_rollback_floor(
                            &mut tis,
                            AB_ROLLBACK_NV_INDEX,
                            init_floor,
                        )
                    } else {
                        tpm20::provision_rollback_floor(&mut tis, AB_ROLLBACK_NV_INDEX, init_floor)
                    };
                    match provisioned {
                        Ok(floor) => {
                            tpm_present = true;
                            tpm_floor = floor;
                            fstart_log::info!(
                                "{}: provisioned TPM rollback floor = {}",
                                label,
                                tpm_floor,
                            );
                        }
                        Err(e) => {
                            let (kind, code) = tpm_err_parts(e);
                            fstart_log::error!(
                                "{}: TPM rollback provisioning failed: {} ({:#x})",
                                label,
                                kind,
                                code,
                            );
                            halt();
                        }
                    }
                }
                Err(e) => {
                    let (kind, code) = tpm_err_parts(e);
                    fstart_log::error!(
                        "{}: TPM rollback floor read failed: {} ({:#x})",
                        label,
                        kind,
                        code,
                    );
                    halt();
                }
            }

            // Advance TPM rollback floor when RW left Phase::FloorPending.
            if let Ok(scan) = &initial_scan
                && let ScanStatus::Valid { record, .. } = scan.status
                && record.phase == Phase::FloorPending
            {
                let base_policy = RootPolicy {
                    image_family: trust.image_family,
                    minimum_security_version: trust.minimum_security_version.get().max(tpm_floor),
                    image_size: 0,
                    max_directory_size: MAX_DIRECTORY_SIZE,
                    keys: trust.valid_keys(),
                };
                let can_finalize = if record.sec_version <= tpm_floor {
                    true
                } else {
                    let slots_ready = record.is_sync_done()
                        && layout.extent(Slot::A).is_some_and(|e| {
                            verify_slot_bootable(
                                &rom_media,
                                e,
                                &base_policy,
                                record.sec_version,
                                Some(&record.root_digest),
                            )
                            .is_some()
                        })
                        && layout.extent(Slot::B).is_some_and(|e| {
                            verify_slot_bootable(
                                &rom_media,
                                e,
                                &base_policy,
                                record.sec_version,
                                Some(&record.root_digest),
                            )
                            .is_some()
                        });
                    if slots_ready {
                        match tpm20::advance_rollback_floor(
                            &mut tis,
                            AB_ROLLBACK_NV_INDEX,
                            record.sec_version,
                        ) {
                            Ok(FloorAdvance::Advanced(f) | FloorAdvance::AlreadyAtOrAbove(f)) => {
                                tpm_floor = f;
                                fstart_log::info!(
                                    "{}: TPM rollback floor advanced to {}",
                                    label,
                                    tpm_floor,
                                );
                                true
                            }
                            Err(e) => {
                                let (kind, code) = tpm_err_parts(e);
                                fstart_log::error!(
                                    "{}: TPM rollback floor advance failed: {} ({:#x})",
                                    label,
                                    kind,
                                    code,
                                );
                                match tpm20::read_rollback_floor(&mut tis, AB_ROLLBACK_NV_INDEX) {
                                    Ok(actual) => {
                                        tpm_floor = actual;
                                        actual >= record.sec_version
                                    }
                                    Err(re_err) => {
                                        let (rk, rc) = tpm_err_parts(re_err);
                                        fstart_log::error!(
                                            "{}: TPM floor re-read failed: {} ({:#x})",
                                            label,
                                            rk,
                                            rc,
                                        );
                                        halt();
                                    }
                                }
                            }
                        }
                    } else {
                        fstart_log::warn!(
                            "{}: FloorPending skipped (slots not synced/verified)",
                            label,
                        );
                        false
                    }
                };
                if can_finalize && let Some(finalized) = record.next_floor_finalized() {
                    if append_ro_record(&mut state_flash, layout, scan, &finalized) {
                        fstart_log::info!("{}: slot state finalized to Stable", label);
                    } else {
                        fstart_log::warn!(
                            "{}: FloorPending finalized TPM floor but SLOT_STATE append failed; will retry on next boot",
                            label,
                        );
                    }
                }
            }

            if let Err(e) = tpm20::lock_rollback_floor(&mut tis, AB_ROLLBACK_NV_INDEX) {
                let (kind, code) = tpm_err_parts(e);
                fstart_log::error!(
                    "{}: failed to lock TPM rollback floor: {} ({:#x})",
                    label,
                    kind,
                    code,
                );
                halt();
            }
            fstart_log::info!("{}: TPM rollback floor locked", label);
        }
    }

    if !tpm_present {
        if ab_cfg.require_tpm_floor {
            fstart_log::error!(
                "{}: mandatory discrete FIFO TPM 2.0 did not respond; halting",
                label,
            );
            halt();
        }
        // Never silently finalize Phase::FloorPending or resume Phase::Syncing
        // without a live TPM: both states are only entered on TPM-provisioned
        // devices, so a transient TPM disconnect must not cancel the pending
        // monotonic anti-rollback floor advance.
        if let Ok(scan) = &initial_scan
            && let ScanStatus::Valid { record, .. } = scan.status
            && matches!(record.phase, Phase::FloorPending | Phase::Syncing)
        {
            let phase_name = match record.phase {
                Phase::FloorPending => "FloorPending",
                Phase::Syncing => "Syncing",
                _ => "unknown",
            };
            fstart_log::error!(
                "{}: SLOT_STATE is {} (sec={}) but no discrete FIFO TPM responded; halting",
                label,
                phase_name,
                record.sec_version,
            );
            halt();
        }
        fstart_log::warn!(
            "{}: no discrete FIFO TPM fitted; hardware anti-rollback unavailable",
            label,
        );
    }

    let effective_floor = trust.minimum_security_version.get().max(tpm_floor);
    let policy = RootPolicy {
        image_family: trust.image_family,
        minimum_security_version: effective_floor,
        image_size: 0,
        max_directory_size: MAX_DIRECTORY_SIZE,
        keys: trust.valid_keys(),
    };
    let decision = select_slot(
        layout,
        SelectorPolicy { provisioned: true },
        &mut state_flash,
        &rom_media,
        &policy,
    );

    let (
        mut slot,
        mut extent,
        mut root,
        mut root_digest,
        mut record,
        mut trial_active,
        mut sync_pending,
    ) = match decision {
        SlotDecision::BootRw {
            slot,
            extent,
            root,
            root_digest,
            record,
            trial_active,
            sync_pending,
        } => (
            slot,
            extent,
            root,
            root_digest,
            record,
            trial_active,
            sync_pending,
        ),
        SlotDecision::BootRecovery(reason) => {
            fstart_log::error!(
                "{}: recovery required ({})",
                label,
                recovery_reason_str(reason),
            );
            halt();
        }
    };

    let primary_loaded = root
        .root()
        .descriptors
        .iter()
        .flatten()
        .find(|d| d.role == BootstrapRole::Mainstage && d.load_addr == AB_RW_IMAGE_BASE)
        .copied()
        .ok_or(fstart_stage::boot::LoadError::Bounds)
        .and_then(|desc| try_load_slot(&rom_media, extent, &desc));

    let verified = match primary_loaded {
        Ok(v) => v,
        Err(primary_err) => {
            fstart_log::error!(
                "{}: primary slot {} load failed ({:?}); checking journal fallback",
                label,
                slot_name(slot),
                primary_err,
            );
            // Only fall back when the journal explicitly designates an alternate
            // RW slot (never bypass Phase::Syncing / Slot::Recovery or downgrade
            // below current_record.sec_version on non-trial boots).
            let Some(current_record) = record else {
                fstart_log::error!("{}: no journal record permitting RW fallback", label);
                halt();
            };
            let fallback = current_record.fallback_slot;
            if !layout.is_ab() || !matches!(fallback, Slot::A | Slot::B) || fallback == slot {
                fstart_log::error!(
                    "{}: no journal-permitted RW fallback slot (fallback={})",
                    label,
                    slot_name(fallback),
                );
                halt();
            }
            let Some(peer_ext) = layout.extent(fallback) else {
                fstart_log::error!(
                    "{}: fallback slot {} has no flash extent",
                    label,
                    slot_name(fallback)
                );
                halt();
            };
            let fallback_floor = if trial_active {
                effective_floor
            } else {
                effective_floor.max(current_record.sec_version)
            };
            let Some((peer_root, peer_digest, peer_desc)) =
                verify_slot_bootable(&rom_media, peer_ext, &policy, fallback_floor, None)
            else {
                fstart_log::error!(
                    "{}: fallback slot {} failed authentication (min_sec={})",
                    label,
                    slot_name(fallback),
                    fallback_floor,
                );
                halt();
            };
            let Ok(v) = try_load_slot(&rom_media, peer_ext, &peer_desc) else {
                fstart_log::error!(
                    "{}: fallback slot {} load failed",
                    label,
                    slot_name(fallback)
                );
                halt();
            };
            let Ok(fb_scan) = scan_journal_flash(&mut state_flash, s0, s1) else {
                fstart_log::error!(
                    "{}: failed to rescan SLOT_STATE before fallback append",
                    label
                );
                halt();
            };
            let Some(fb_record) = current_record.next_load_fallback(
                peer_root.root().fw_version,
                peer_root.root().security_version,
                &peer_digest,
            ) else {
                fstart_log::error!("{}: unable to construct fallback SLOT_STATE record", label);
                halt();
            };
            if !append_ro_record(&mut state_flash, layout, &fb_scan, &fb_record) {
                fstart_log::error!("{}: failed to persist fallback SLOT_STATE record", label);
                halt();
            }
            fstart_log::warn!(
                "{}: fell back to journal slot {} fw={} sec={}",
                label,
                slot_name(fallback),
                peer_root.root().fw_version,
                peer_root.root().security_version,
            );
            slot = fallback;
            extent = peer_ext;
            root = peer_root;
            root_digest = peer_digest;
            record = Some(fb_record);
            trial_active = false;
            sync_pending = false;
            v
        }
    };

    let elapsed_us = timer.timestamp_us().saturating_sub(t_start);
    fstart_log::info!(
        "{}: verified + loaded slot {} (fw={} sec={} floor={} trial={} sync={}) in {} ms ({} us, WP={} @ {:#x})",
        label,
        slot_name(slot),
        root.root().fw_version,
        root.root().security_version,
        effective_floor,
        u8::from(trial_active),
        u8::from(sync_pending),
        elapsed_us / 1000,
        elapsed_us,
        u8::from(wp_enabled),
        rom_window_base,
    );

    // Tear down temporary WP MTRR and flush caches before jumping to RW so no
    // stale flash cache lines survive across future SPI writes or MTRR rebuild.
    if wp_enabled {
        // SAFETY: BSP-only, before RW takes over and before AP startup.
        if unsafe {
            fstart_arch::x86::mtrr::set_rom_window_wp(rom_window_base, LOWER_HALF_SIZE, false)
        }
        .is_err()
        {
            fstart_log::warn!(
                "{}: unable to clear temporary WP MTRR at {:#x}",
                label,
                rom_window_base,
            );
        }
    }
    // SAFETY: ring 0, caches enabled in RAM.
    unsafe { fstart_arch::x86::writeback_invalidate_caches() };

    let slot_handoff = ActiveSlotHandoff::from_decision(
        layout,
        effective_floor,
        slot,
        &root,
        root_digest,
        record,
        trial_active,
        sync_pending,
    );
    let handoff = AmdAbHandoff {
        magic: HANDOFF_MAGIC,
        version: U32::new(HANDOFF_VERSION),
        tpm_present: u8::from(tpm_present),
        rom_page,
        rom3_mode,
        reserved: [0; 5],
        flash_size: U32::new(flash_size),
        bsp_bist: U32::new(entry::bsp_bist()),
        ap_route_addr: U64::new(entry::ap_route_addr()),
        rom_window_base: U64::new(rom_window_base),
        tpm_floor: U64::new(tpm_floor),
        ro_verify_us: U64::new(elapsed_us),
        slot: slot_handoff,
        trust_bytes,
    };
    // SAFETY: AB_HANDOFF_ADDR (`0x75af_0000`) is a dedicated 4 KiB page inside
    // the 16 MiB early DRAM window, disjoint from RO, RW, and Siena's APOB.
    unsafe {
        core::ptr::copy_nonoverlapping(
            handoff.as_bytes().as_ptr(),
            AB_HANDOFF_ADDR as *mut u8,
            core::mem::size_of::<AmdAbHandoff>(),
        );
    }

    fstart_log::info!(
        "{}: jumping to RW stage at {:#x} (slot offset {:#x}, size {:#x})",
        label,
        verified.entry(),
        extent.offset,
        extent.size,
    );
    fstart_log::flush();
    fstart_arch::x86::boot::jump_to(verified.entry())
}

/// Execute the SP3 (Rome/Milan/Naples) RO selector stage and jump to the
/// authenticated RW stage.
pub fn run_sp3_ro<B: Board, E>(
    early_init: fn(&hw::PrimaryFch) -> Result<(), E>,
    console_cfg: Ns16550Config,
) -> ! {
    hw::post(0x01);
    let signature = fstart_arch::x86::cpuid(1).0;
    let ext_ebx = fstart_arch::x86::cpuid(0x8000_0001).1;
    let Ok(identity) = silicon::identify(signature, ext_ebx) else {
        hw::post(0x13);
        fstart_arch::x86::boot::halt();
    };
    let generation = Generation::of(identity.silicon());
    let _pci_root = hw::enable_ecam(generation);
    let timer = hw::tsc();
    let fch = hw::primary_fch(timer);

    // Early FCH setup: ACPI MMIO + LPC decodes + SPI100 speedup (FastRead +
    // HostBurstEn + 22 MHz) BEFORE verifying RW flash. These calls are
    // idempotent: the SP3 RW stage re-runs its standard FCH reset sequence
    // after handoff without disturbing DRAM or SPI flash contents.
    fch::tasks::reset::prelim(&fch, generation.silicon());
    fch::lpc::reset(&fch, generation.silicon());
    fch::spi::reset(&fch, generation.silicon(), &fch::spi::SpiPolicy::default());

    if early_init(&fch).is_err() {
        hw::post(0x02);
        fstart_arch::x86::boot::halt();
    }
    let Ok(mut console) = Ns16550::new(console_cfg) else {
        fstart_arch::x86::boot::halt();
    };
    if console.init().is_err() {
        fstart_arch::x86::boot::halt();
    }
    static CONSOLE: fstart_log::StaticConsole<Ns16550> = fstart_log::StaticConsole::new();
    let _ = CONSOLE.install(console);
    fstart_log::info!(
        "sp3-ab ro: {} ({:#x}, TSC {} MHz, SPI100 FastRead+Burst enabled)",
        identity.revision.name(),
        identity.signature,
        timer.mhz(),
    );

    // Read build-patched FlashAnchor to get the SPI part size (16 or 32 MiB).
    // Fail closed if unpatched or corrupt instead of guessing 16 MiB.
    let mut anchor_bytes = [0u8; core::mem::size_of::<FlashAnchor>()];
    read_phys(RO_FLASH_ANCHOR.0.get() as u64, &mut anchor_bytes);
    let Some(anchor) = FlashAnchor::parse(&anchor_bytes) else {
        fstart_log::error!("sp3-ab ro: missing or invalid FlashAnchor");
        halt();
    };
    let flash_size = anchor.flash_size.get();

    // Resolve the lower 16 MiB flash window (ROM2 if booted from lower half,
    // else probe ROM3 above 4 GiB). Probe happens while ROM windows are UC.
    let page = rom3::rom_page(&fch);
    if page != 0 {
        fstart_log::error!(
            "sp3-ab ro: booted from ROM2 page {} (upper half); halting until upper-half SPI lockdown is supported",
            page,
        );
        halt();
    }
    let rom2 = FlashMap::rom2(page, flash_size);
    let map = if rom2.address(0, 0x100_0000).is_some() {
        rom2
    } else {
        FlashMap::probe(page, flash_size, read_phys)
    };
    let Some(rom_window_base) = map.address(0, 0x100_0000) else {
        fstart_log::error!(
            "sp3-ab ro: lower 16 MiB flash window unreachable (page {})",
            page
        );
        halt();
    };
    let rom3_mode = match map.rom3 {
        Rom3::Absent => ROM3_ABSENT,
        Rom3::Linear => ROM3_LINEAR,
        Rom3::Paged => ROM3_PAGED,
    };
    fstart_log::info!(
        "sp3-ab ro: flash {:#x} bytes, ROM2 page {}, lower-half window {:#x}",
        flash_size,
        page,
        rom_window_base,
    );

    let layout = SlotLayout::RoAB {
        state_sector_0: SP3_AB_STATE_SECTOR_0,
        state_sector_1: SP3_AB_STATE_SECTOR_1,
        slot_a: SlotExtent {
            offset: SP3_AB_SLOT_A_OFFSET,
            size: SP3_AB_SLOT_SIZE,
        },
        slot_b: SlotExtent {
            offset: SP3_AB_SLOT_B_OFFSET,
            size: SP3_AB_SLOT_SIZE,
        },
    };
    select_and_boot_rw::<B>(
        "sp3-ab ro",
        timer,
        layout,
        flash_size,
        rom_window_base,
        page,
        rom3_mode,
    )
}

/// Execute the Siena (SP6 / RS) RO selector stage and jump to the
/// authenticated RW stage.
pub fn run_siena_ro<B: Board>(early_init: fn(&fch::Native<Tsc>), console_cfg: Ns16550Config) -> ! {
    use fstart_arch::x86::cpu::msr::NativeMsr;
    use fstart_driver_amd::ccx;
    use fstart_driver_amd::window::Window;

    fch::post_native(0x01);
    // SAFETY: BSP only, single-threaded cold boot.
    let msr = unsafe { NativeMsr::new() };
    ccx::msr::enable_ecam(&msr, crate::rs::ECAM_BASE, crate::rs::ECAM_BUSES);
    fstart_pci::ecam::init(crate::rs::ECAM_BASE as usize);
    // SAFETY: exclusive FCH programming before the console.
    unsafe { fch::acpimmio::enable_acpimmio() };
    let timer = Tsc::measured(&msr).unwrap_or_else(|| Tsc::native(&msr));
    // SAFETY: BSP only, cold boot; ACPI MMIO and ECAM are decoded.
    let fch = unsafe { fch::Native::new(Tsc::native(&msr)) };
    fch::songshan::take_over(&fch);

    // Ensure SPI100 host burst prefetch is enabled on Siena before hashing RW.
    let spi_decoded = fch
        .lpc()
        .at(fch::lpc::SPI_BASE_ADDRESS)
        .read(fch::lpc::SpiBase::BASE)
        == fch::SPI_BASE >> 8;
    if spi_decoded {
        fch.mmio()
            .at(fch::spi::SPI100_HOST_PREFETCH)
            .modify(fch::spi::Spi100HostPrefetch::HOST_BURST_EN::SET);
    }

    early_init(&fch);
    let Ok(mut console) = Ns16550::new(console_cfg) else {
        fstart_arch::x86::boot::halt();
    };
    if console.init().is_err() {
        fstart_arch::x86::boot::halt();
    }
    static CONSOLE: fstart_log::StaticConsole<Ns16550> = fstart_log::StaticConsole::new();
    let _ = CONSOLE.install(console);
    fstart_log::info!(
        "siena-ab ro: entered (TSC {} MHz, SPI100={})",
        timer.mhz(),
        u8::from(spi_decoded),
    );
    if !spi_decoded {
        fstart_log::error!("siena-ab ro: SPI100 not decoded at {:#x}", fch::SPI_BASE);
        halt();
    }

    let layout = SlotLayout::RoAB {
        state_sector_0: SIENA_AB_STATE_SECTOR_0,
        state_sector_1: SIENA_AB_STATE_SECTOR_1,
        slot_a: SlotExtent {
            offset: SIENA_AB_SLOT_A_OFFSET,
            size: SIENA_AB_SLOT_SIZE,
        },
        slot_b: SlotExtent {
            offset: SIENA_AB_SLOT_B_OFFSET,
            size: SIENA_AB_SLOT_SIZE,
        },
    };
    select_and_boot_rw::<B>(
        "siena-ab ro",
        timer,
        layout,
        0x100_0000,
        fstart_amdfw_format::ROM2_BASE,
        0,
        ROM3_ABSENT,
    )
}
