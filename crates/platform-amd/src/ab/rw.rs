// SPDX-License-Identifier: MIT OR Apache-2.0
//! AMD RO/A/B RW stage helpers: active-slot FFS mounting, TPM handoff
//! measurement and platform hierarchy lockdown, verified two-stage hardware FCH
//! SPI lockdown (`RomProtect0..2` + `AltSPICS[PROTECT_LOCK]` +
//! `SPIRestrictedCmd`/`2` + `SPIx00[SpiHostAccessRomEn=0]`), trial
//! confirmation, and CrabEFI `FstartCapsuleBackend` + `FlashStorage` handoff
//! sharing one SPI100 controller.

extern crate alloc;

use super::handoff::{AmdAbHandoff, handoff, slot_name};
use super::{
    AB_HANDOFF_ADDR, AB_NVRAM_OFFSET, AB_NVRAM_SIZE, AB_RO_FLASH_OFFSET, lower_half_probe,
    tpm_err_parts,
};

#[cfg(all(feature = "payload-uefi-basic", not(feature = "payload-uefi")))]
compile_error!(
    "AMD A/B mode requires full UEFI (`--payload uefi` or `uefi-ui`) for capsule and flash lockdown support, not `uefi-basic`"
);
use crate::board::Facts;
use crate::layout::{Layout, Region};
use crate::media::MountError;
use core::sync::atomic::{AtomicBool, Ordering};
use fstart_core::ffs::trust::TrustBlock;
use fstart_core::services::boot_media::MemoryMapped;
use fstart_core::services::memory_detect::E820Entry;
#[cfg(feature = "payload-uefi")]
use fstart_core::services::{FlashRegion, ServiceError};
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::fch::spi::ProtectRange;
#[cfg(feature = "payload-uefi")]
use fstart_driver_amd::fch::spi::flash::ERASE_SIZE;
#[cfg(feature = "payload-uefi")]
use fstart_driver_amd::fch::spi::flash::SubRegion;
use fstart_driver_amd::fch::spi::flash::{Extent, Flash, SharedFlash};
use fstart_driver_amd::fch::{self, FchHw};
use fstart_ffs::root::RootPolicy;
use fstart_ffs::slot_state::SlotLayout;
use fstart_stage::directory::MAX_DIRECTORY_SIZE;
use fstart_stage::root::read_boot_root_with_digest;

/// Software write-lock latch paired with `RomProtect0..2` +
/// `AltSPICS[PROTECT_LOCK]` (blocks `SLOT_STATE` / `RW_A` / `RW_B` writes).
static AMD_AB_WRITE_LOCKED: AtomicBool = AtomicBool::new(false);

/// Software write-lock latch paired with `SPIRestrictedCmd` +
/// `SPIx00[SpiHostAccessRomEn=0]` (blocks all SPI writes including `NVRAM`,
/// returning `ServiceError::NotSupported` -> `StorageError::WriteProtected`
/// cleanly after CrabEFI varstore initialization).
#[cfg(feature = "payload-uefi")]
static AMD_GLOBAL_SPI_LOCKED: AtomicBool = AtomicBool::new(false);

/// Non-empty marker published into `ffs_context::set_memory_mapped` so
/// downstream asset/microcode/payload loaders see the active RW slot window
/// (the directory itself is installed directly via `install_directory_context`).
static AB_ANCHOR_MARKER: [u8; 4] = *b"AMAB";

fn halt() -> ! {
    fstart_log::flush();
    fstart_arch::x86::boot::halt()
}

/// Active RW slot region in CPU physical address space, resolved from RO's
/// handoff (`rom_window_base + active_slot_extent.offset`).
pub(crate) fn active_slot_region(h: &AmdAbHandoff) -> Option<(Region, u32)> {
    let layout = h.slot.layout()?;
    let slot = h.slot.slot()?;
    let extent = layout.extent(slot)?;
    let base = h
        .rom_window_base
        .get()
        .checked_add(u64::from(extent.offset))?;
    Some((Region::new(base, u64::from(extent.size)), extent.offset))
}

/// Read-only view of the shared A/B EFI NVRAM region (`0xf4_0000..0xf8_0000`)
/// through the lower 16 MiB ROM window.
pub(crate) fn ab_nvram_slice(h: &AmdAbHandoff) -> Option<&'static [u8]> {
    let addr = h
        .rom_window_base
        .get()
        .checked_add(u64::from(AB_NVRAM_OFFSET))?;
    // SAFETY: lower 16 MiB ROM window is identity-mapped read-only flash.
    Some(unsafe { core::slice::from_raw_parts(addr as *const u8, AB_NVRAM_SIZE as usize) })
}

/// Mount and re-authenticate the active RW slot FFS in DRAM, install the load
/// policy and verified directory context, and publish the active slot window
/// to `ffs_context` so microcode and payload loading work identically to
/// single-image boots.
pub(crate) fn mount_ab_ffs(
    e820: &[E820Entry],
    layout: &Layout,
    ffs: Region,
) -> Result<(), MountError> {
    let h = handoff();
    let slot = h.slot.slot().ok_or(MountError::Mount)?;
    fstart_log::info!(
        "amd-ab rw: slot={} fw={} sec={} floor={} trial={} sync={} (RO verify {} ms)",
        slot_name(slot),
        h.slot.fw_version.get(),
        h.slot.sec_version.get(),
        h.slot.effective_floor.get(),
        h.slot.trial_active,
        h.slot.sync_pending,
        h.ro_verify_us.get() / 1000,
    );

    crate::media::install_load_policy(e820, layout, ffs)?;

    let trust = TrustBlock::parse(&h.trust_bytes)
        .filter(|t| t.key_count.get() > 0)
        .ok_or(MountError::Verify)?;
    let size = usize::try_from(ffs.size).map_err(|_| MountError::Size)?;
    // SAFETY: active RW slot inside the identity-mapped lower 16 MiB ROM window.
    let media = unsafe { MemoryMapped::from_raw_addr(ffs.base, size) };
    let policy = RootPolicy {
        image_family: trust.image_family,
        minimum_security_version: h.slot.effective_floor.get(),
        image_size: ffs.size,
        max_directory_size: MAX_DIRECTORY_SIZE,
        keys: trust.valid_keys(),
    };
    let (root, digest) =
        read_boot_root_with_digest(&media, 0, &policy).map_err(|_| MountError::Verify)?;
    if digest != h.slot.root_digest {
        return Err(MountError::Verify);
    }

    // SAFETY: RW runs single-threaded on the BSP with DRAM allocator live;
    // the directory has just been authenticated against RO's TrustBlock and
    // root digest, and the heap/early DRAM is reserved from DMA and loaders.
    unsafe { fstart_stage::directory::install_directory_context(&media, *root.directory()) }
        .map_err(|_| MountError::Mount)?;
    fstart_core::services::ffs_context::set_memory_mapped(&AB_ANCHOR_MARKER, ffs.base, ffs.size);
    Ok(())
}

/// Extend PCR 0 with RO's TrustBlock and the active RW slot's Root digest
/// after [`crate::tpm::start`] has measured the RO reset image.
pub(crate) fn measure_ab_tpm() {
    let h = handoff();
    fstart_tcg::measure_blob(
        0,
        fstart_tcg::event::EV_S_CRTM_CONTENTS,
        "ro-trust",
        AB_HANDOFF_ADDR,
        &h.trust_bytes,
    );
    let slot_addr = active_slot_region(&h).map_or(0, |(r, _)| r.base);
    fstart_tcg::measure_blob(
        0,
        fstart_tcg::event::EV_S_CRTM_CONTENTS,
        "rw-root",
        slot_addr,
        &h.slot.root_digest,
    );
}

/// Compute the three 64 KiB-aligned FCH `RomProtect` ranges covering the lower
/// 16 MiB ROM2 window (`0xff00_0000..0x1_0000_0000`):
/// - Slot 0 (`[0, state_base)`): vendor PSP / EFS / ABL partition (immutable).
/// - Slot 1 (`[AB_RO_FLASH_OFFSET, 0x100_0000)`): RO selector image (immutable).
/// - Slot 2 (`[state_base, AB_NVRAM_OFFSET)`): `SLOT_STATE`, `RW_A`, `RW_B`.
///
/// `NVRAM` (`0xf4_0000..0xf8_0000`) lies outside slots 0..2 so CrabEFI's
/// trusted Boot Menu / Settings UI can persist variables before
/// [`finalize_fch_flash_lock`] engages global SPI100 opcode restriction.
fn rom_protect_ranges(layout: SlotLayout) -> Result<[ProtectRange; 3], &'static str> {
    let (state_base, _) = layout.state_sectors();
    if state_base == 0
        || !state_base.is_multiple_of(0x1_0000)
        || state_base >= AB_NVRAM_OFFSET
        || !AB_NVRAM_OFFSET.is_multiple_of(0x1_0000)
        || !AB_RO_FLASH_OFFSET.is_multiple_of(0x1_0000)
    {
        return Err("unaligned RomProtect bounds");
    }
    let rom2 = fstart_amdfw_format::ROM2_BASE as u32;
    Ok([
        ProtectRange {
            base: rom2,
            size_64k: (state_base / 0x1_0000) as u16,
            read: false,
            write: true,
        },
        ProtectRange {
            base: rom2 + AB_RO_FLASH_OFFSET,
            size_64k: ((0x100_0000 - AB_RO_FLASH_OFFSET) / 0x1_0000) as u16,
            read: false,
            write: true,
        },
        ProtectRange {
            base: rom2 + state_base,
            size_64k: ((AB_NVRAM_OFFSET - state_base) / 0x1_0000) as u16,
            read: false,
            write: true,
        },
    ])
}

/// Stage 1A (UEFI): always program and verify `RomProtect0` (PSP) and
/// `RomProtect1` (RO) before CrabEFI runs, even when Capsule-on-Disk is
/// pending.
#[cfg(feature = "payload-uefi")]
fn protect_immutable_regions<H: FchHw>(fch: &H, layout: SlotLayout) -> Result<(), &'static str> {
    let [psp, ro, _] = rom_protect_ranges(layout)?;
    fch::spi::protect(fch, 0, &psp);
    if !fch::spi::verify_protect(fch, 0, &psp) {
        return Err("RomProtect0 (PSP) verify failed");
    }
    fch::spi::protect(fch, 1, &ro);
    if !fch::spi::verify_protect(fch, 1, &ro) {
        return Err("RomProtect1 (RO) verify failed");
    }
    Ok(())
}

/// Stage 1B: program and verify `RomProtect0..2` (`PSP`, `RO`, `SLOT_STATE` +
/// `RW_A` + `RW_B`), latch `AltSPICS[PROTECT_EN0 | PROTECT_LOCK]`, verify
/// readback, and engage the A/B software write latch.
///
/// Leaves `NVRAM` (`0xf4_0000..0xf8_0000`) writable until
/// [`finalize_fch_flash_lock`] engages global SPI100 opcode restriction.
fn protect_ab_slots_and_lock_ranges<H: FchHw>(
    fch: &H,
    layout: SlotLayout,
) -> Result<(), &'static str> {
    let ranges = rom_protect_ranges(layout)?;
    for (slot, range) in ranges.iter().enumerate() {
        fch::spi::protect(fch, slot as u8, range);
        if !fch::spi::verify_protect(fch, slot as u8, range) {
            return Err("RomProtect verify failed");
        }
    }
    fch::spi::lock(fch);
    if !fch::spi::verify_lock(fch) {
        return Err("AltSPICS PROTECT_LOCK verify failed");
    }
    AMD_AB_WRITE_LOCKED.store(true, Ordering::Release);
    Ok(())
}

/// Stage 2 (unconditional pre-boot lockdown): ensure `RomProtect0..2` +
/// `PROTECT_LOCK` are programmed and verified, then engage and verify global
/// SPI100 write restriction (`SPIRestrictedCmd`/`SPIRestrictedCmd2` including
/// addressless `WREN`/`WRSR` in `WO_ADDR0..2`, plus write-0-once
/// `SPIx00[SpiHostAccessRomEn=0, SpiAccessMacRomEn=0]`).
///
/// Note per AMD Siena PPR §10.3.8.1.1 / §10.1.9.1.4: `AltSPICS[PROTECT_LOCK]`
/// only locks `AltSPICS[5:3]` and `D14F3x50..5C`; it does not block subsequent
/// programming of `SPIx04`/`SPIx08`/`SPIx00[23:22]`.
///
/// Idempotent: returns `Ok(())` cleanly if already locked and verified.
pub(crate) fn finalize_fch_flash_lock<H: FchHw>(
    fch: &H,
    layout: SlotLayout,
) -> Result<(), &'static str> {
    let ranges = rom_protect_ranges(layout)?;
    let ranges_ok = ranges
        .iter()
        .enumerate()
        .all(|(slot, range)| fch::spi::verify_protect(fch, slot as u8, range));
    let already_locked =
        ranges_ok && fch::spi::verify_lock(fch) && fch::spi::verify_writes_restricted(fch);
    if already_locked {
        AMD_AB_WRITE_LOCKED.store(true, Ordering::Release);
        #[cfg(feature = "payload-uefi")]
        AMD_GLOBAL_SPI_LOCKED.store(true, Ordering::Release);
        return Ok(());
    }

    if !fch::spi::verify_lock(fch) {
        protect_ab_slots_and_lock_ranges(fch, layout)?;
    } else if !ranges_ok {
        return Err("RomProtect locked with unexpected ranges");
    }

    fch::spi::restrict_writes(fch)?;
    AMD_AB_WRITE_LOCKED.store(true, Ordering::Release);
    #[cfg(feature = "payload-uefi")]
    AMD_GLOBAL_SPI_LOCKED.store(true, Ordering::Release);

    let (state_base, _) = layout.state_sectors();
    fstart_log::info!(
        "amd-ab rw: FCH SPI finalized & verified (PSP=0..{:#x} AB={:#x}..{:#x} RO={:#x}..0x1000000 + PROTECT_LOCK + SPIRestrictedCmd + HostRomEn=0)",
        state_base,
        state_base,
        AB_NVRAM_OFFSET,
        AB_RO_FLASH_OFFSET,
    );
    Ok(())
}

/// Ensure the discrete TPM 2.0 platform hierarchy (`TPM_RH_PLATFORM`) is locked
/// before handing off to any payload or OS whenever RO interacted with a TPM.
///
/// Without locking `TPM_RH_PLATFORM`, an untrusted OS could call
/// `TPM2_NV_UndefineSpace` on the `TPMA_NV_PLATFORMCREATE` anti-rollback NV
/// index (`0x0140_0001`). This covers ALL cases:
/// - `fstart_tcg` active: calls idempotent `fstart_tcg::lock()` (so UEFI/Linux
///   later `tcg::handoff` succeeds and Coreboot/Elf/Halt are also locked).
/// - `fstart_tcg` inactive (e.g. `Hide`/`None`/`Reset` remedy or skipped):
///   directly probes FIFO TIS and calls `tpm20::lock_platform_hierarchy`.
fn ensure_tpm_platform_locked(h: &AmdAbHandoff) -> Result<(), &'static str> {
    if h.tpm_present == 0 {
        return Ok(());
    }
    if fstart_tcg::platform_hierarchy_locked() {
        fstart_log::info!("amd-ab rw: TPM platform hierarchy already locked (via fstart-tcg)");
        return Ok(());
    }
    if fstart_tcg::log().is_some() {
        fstart_tcg::lock().map_err(|e| {
            let (kind, code) = tpm_err_parts(e);
            fstart_log::error!("amd-ab rw: fstart_tcg::lock failed: {} ({:#x})", kind, code,);
            "TPM platform hierarchy lock failed"
        })?;
        fstart_log::info!("amd-ab rw: TPM platform hierarchy locked (via fstart-tcg)");
        return Ok(());
    }

    use fstart_driver_tpm::tis::{self, Tis};
    use fstart_driver_tpm::tpm20;

    // SAFETY: RO probed and initialized the discrete FIFO TPM at 0xfed4_0000;
    // fstart_tcg is inactive so we own TIS exclusively here.
    let mut tis = unsafe { Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
    if tis.probe().is_none() || tis.is_crb() {
        fstart_log::error!("amd-ab rw: TPM missing when locking platform hierarchy");
        return Err("TPM missing at platform lock");
    }
    tpm20::lock_platform_hierarchy(&mut tis).map_err(|e| {
        let (kind, code) = tpm_err_parts(e);
        fstart_log::error!(
            "amd-ab rw: direct TPM platform hierarchy lock failed: {} ({:#x})",
            kind,
            code,
        );
        "TPM platform hierarchy lock failed"
    })?;
    fstart_log::info!("amd-ab rw: TPM platform hierarchy locked (direct TIS)");
    Ok(())
}

#[cfg(feature = "payload-uefi")]
struct GuardedSubRegion<'a> {
    inner: SubRegion<'a, Tsc>,
    latch: &'static AtomicBool,
}

#[cfg(feature = "payload-uefi")]
impl FlashRegion for GuardedSubRegion<'_> {
    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ServiceError> {
        self.inner.read(offset, buf)
    }

    fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
        if self.latch.load(Ordering::Acquire) {
            return Err(ServiceError::NotSupported);
        }
        self.inner.erase(offset, length)
    }

    fn program(&mut self, offset: u32, data: &[u8]) -> Result<(), ServiceError> {
        if self.latch.load(Ordering::Acquire) {
            return Err(ServiceError::NotSupported);
        }
        self.inner.program(offset, data)
    }
}

/// Complete A/B trial confirmation, engage verified FCH SPI lockdown and TPM
/// platform hierarchy lockdown, wire CrabEFI variable storage + Capsule-on-Disk
/// backend (on UEFI builds), and boot the payload.
#[cfg_attr(not(feature = "payload-uefi"), allow(unused_variables))]
pub(crate) fn prepare_and_launch_ab<H: FchHw + Sync>(
    payload_handoff: fstart_payload::Handoff<'_>,
    fch: &H,
    timer: Tsc,
    board: &Facts,
    options: &fstart_options::Report,
) -> ! {
    let h = handoff();
    if h.rom_page != 0 {
        fstart_log::error!(
            "amd-ab rw: handoff rom_page={} (upper half); halting fail-closed",
            h.rom_page,
        );
        halt();
    }
    let Some(ab_cfg) = board.ab_update else {
        fstart_log::error!("amd-ab rw: board has no Board::AB_UPDATE configuration");
        halt();
    };
    let Some(layout) = h.slot.layout() else {
        fstart_log::error!("amd-ab rw: invalid slot layout in handoff");
        halt();
    };
    let Some(running_slot) = h.slot.slot() else {
        fstart_log::error!("amd-ab rw: invalid active slot in handoff");
        halt();
    };

    // Fault-injection hook for automated rollback testing: trigger a cold reset
    // BEFORE confirming the trial so RO debits attempts and falls back.
    if ab_cfg.fail_trial_before_confirm && h.slot.trial_active != 0 {
        fstart_log::warn!("amd-ab rw: simulating broken trial before confirmation -> cold reset");
        fstart_log::flush();
        fstart_arch::x86::boot::system_reset(true);
    }

    // Open a single SPI100 Flash covering [0, AB_RO_FLASH_OFFSET) — strictly
    // below RO — probed against a lower-half-only region (never RO, which is
    // duplicated in the upper 16 MiB half on 32 MiB dual-generation parts).
    let (probe_off, probe_bytes) = lower_half_probe(layout, h.rom_window_base.get());
    let rw_extent = Extent {
        offset: 0,
        size: AB_RO_FLASH_OFFSET,
    };

    static mut SHARED_FLASH: Option<SharedFlash<Tsc>> = None;
    // SAFETY: BSP-only at payload handoff; SPI100 is decoded at SPI_BASE and
    // owned exclusively from here on. `launch` never returns.
    let shared_slot = unsafe { &mut *core::ptr::addr_of_mut!(SHARED_FLASH) };
    let shared_flash = match unsafe { Flash::new(timer, rw_extent, probe_off, &probe_bytes) } {
        Ok(flash) => shared_slot.insert(SharedFlash::new(flash)),
        Err(_) => {
            fstart_log::error!("amd-ab rw: SPI100 command engine unavailable");
            halt();
        }
    };

    #[cfg(feature = "payload-uefi")]
    {
        use fstart_ffs::slot_state::{TrialConfirm, confirm_trial_and_sync};
        use fstart_payload::crabefi::FirmwareInfo;
        use fstart_payload::crabefi::capsule::{CapsuleConfig, FstartCapsuleBackend};
        use fstart_payload::crabefi::settings::FlashStorage;

        let Some(trust) = TrustBlock::parse(&h.trust_bytes) else {
            fstart_log::error!("amd-ab rw: invalid TrustBlock in handoff");
            halt();
        };

        let Ok(nvram_sub) = shared_flash.subregion(Extent {
            offset: AB_NVRAM_OFFSET,
            size: AB_NVRAM_SIZE,
        }) else {
            fstart_log::error!("amd-ab rw: NVRAM subregion bounds error");
            halt();
        };
        let Ok(ab_sub) = shared_flash.subregion(Extent {
            offset: 0,
            size: AB_NVRAM_OFFSET,
        }) else {
            fstart_log::error!("amd-ab rw: A/B subregion bounds error");
            halt();
        };

        let writable = shared_flash.writable();
        let mut varstore = FlashStorage::new(
            GuardedSubRegion {
                inner: nvram_sub,
                latch: &AMD_GLOBAL_SPI_LOCKED,
            },
            AB_NVRAM_SIZE,
            ERASE_SIZE,
            writable,
        );

        let fw_size = layout.extent(running_slot).map_or(0, |e| e.size);
        let mut capsule_backend = match FstartCapsuleBackend::new(
            GuardedSubRegion {
                inner: ab_sub,
                latch: &AMD_AB_WRITE_LOCKED,
            },
            || {
                protect_ab_slots_and_lock_ranges(fch, layout).map_err(|what| {
                    fstart_log::error!("amd-ab rw: RomProtect lock failed ({})", what);
                    fstart_payload::crabefi::StorageError::IoError
                })
            },
            CapsuleConfig {
                flash_capacity: AB_NVRAM_OFFSET,
                layout,
                running_slot,
                running_fw_version: h.slot.fw_version.get(),
                running_sec_version: h.slot.sec_version.get(),
                running_root_digest: h.slot.root_digest,
                effective_floor: h.slot.effective_floor.get(),
                has_tpm_floor: h.tpm_present != 0,
                trial_active: h.slot.trial_active != 0,
                sync_pending: h.slot.sync_pending != 0,
                image_family: trust.image_family,
                root_keys: trust.valid_keys(),
                firmware_info: FirmwareInfo {
                    guid: ab_cfg.firmware_guid,
                    version: h.slot.fw_version.get(),
                    lowest_supported_version: ab_cfg.lowest_supported_version,
                    fw_size,
                },
                trust_certs: ab_cfg.capsule_trust_certs,
            },
        ) {
            Ok(b) => b,
            Err(_) => {
                fstart_log::error!("amd-ab rw: CapsuleBackend init failed");
                halt();
            }
        };

        // Stage 1A: always protect immutable PSP and RO ranges before CrabEFI
        // runs. Do NOT lock SLOT_STATE/RW_A/RW_B yet so CrabEFI can run
        // `process_pending_capsules` first.
        if let Err(what) = protect_immutable_regions(fch, layout) {
            fstart_log::error!("amd-ab rw: immutable SPI protect failed ({})", what);
            halt();
        }

        if let Err(what) = ensure_tpm_platform_locked(&h) {
            fstart_log::error!("amd-ab rw: {} — halting", what);
            halt();
        }

        // Stage 1B (at TS_CRABEFI_VARSTORE_INIT — after `process_pending_capsules`
        // and `complete_import`, BEFORE `measure_initial_boot` and
        // `boot_manager::run`):
        // 1. Confirm active trial / resume sync ONLY after CrabEFI reached
        //    varstore init cleanly.
        // 2. Lock RomProtect0..2 (PSP, RO, SLOT_STATE + RW_A + RW_B) +
        //    AltSPICS[PROTECT_LOCK] + AMD_AB_WRITE_LOCKED, leaving NVRAM
        //    writable solely for CrabEFI's own trusted Boot Menu (`BootNext`
        //    deletion, Firmware Settings UI, Secure Boot UI).
        let trial_confirmed = AtomicBool::new(false);
        let post_capsule_lock = || {
            if !trial_confirmed.swap(true, Ordering::AcqRel) {
                let Ok(mut confirm_sub) = shared_flash.subregion(Extent {
                    offset: 0,
                    size: AB_NVRAM_OFFSET,
                }) else {
                    fstart_log::error!("amd-ab rw: A/B confirm subregion bounds error");
                    return Err(());
                };
                let mut trial_cfg = TrialConfirm {
                    layout,
                    running_slot,
                    running_fw_version: h.slot.fw_version.get(),
                    running_sec_version: h.slot.sec_version.get(),
                    running_root_digest: h.slot.root_digest,
                    effective_floor: h.slot.effective_floor.get(),
                    has_tpm_floor: h.tpm_present != 0,
                    trial_active: h.slot.trial_active != 0,
                    sync_pending: h.slot.sync_pending != 0,
                };
                match confirm_trial_and_sync(&mut confirm_sub, &mut trial_cfg) {
                    Ok(true) => {
                        fstart_log::info!(
                            "amd-ab rw: trial confirmed (slot={} fw={} sec={})",
                            slot_name(running_slot),
                            h.slot.fw_version.get(),
                            h.slot.sec_version.get(),
                        );
                    }
                    Ok(false) => {}
                    Err(_) => {
                        fstart_log::error!("amd-ab rw: trial confirmation failed");
                        return Err(());
                    }
                }
            }
            protect_ab_slots_and_lock_ranges(fch, layout).map_err(|what| {
                fstart_log::error!("amd-ab rw: post-capsule RomProtect lock failed ({})", what);
            })?;
            fstart_log::info!(
                "amd-ab rw: post-capsule lock engaged (RomProtect0..2 + PROTECT_LOCK; NVRAM open for CrabEFI menu)"
            );
            Ok(())
        };

        // Stage 2 (before ANY third-party EFI binary, bootloader, or OS runs:
        // TS_CRABEFI_OS_LOADER_START / EXIT_BOOT_SERVICES / quiesce_dma /
        // on_exit_boot_services): lock all SPI100 writes including NVRAM
        // (`SPIRestrictedCmd` + `SPIx00[HostRomEn=0]`).
        let pre_boot_lock = || {
            finalize_fch_flash_lock(fch, layout).map_err(|what| {
                fstart_log::error!("amd-ab rw: pre-boot SPI lockdown failed ({})", what);
            })
        };

        let launch_handoff = fstart_payload::Handoff {
            description: payload_handoff.description,
            placement: payload_handoff.placement,
            peripherals: payload_handoff.peripherals,
            variable_storage: Some(&mut varstore),
            settings: Some(crate::options::settings(options)),
            capsule_backend: Some(&mut capsule_backend),
            deferred_buffer: payload_handoff.deferred_buffer,
            post_capsule_lock: Some(&post_capsule_lock),
            pre_boot_lock: Some(&pre_boot_lock),
            pre_launch_hook: None,
        };
        fstart_log::flush();
        fstart_payload::boot(launch_handoff, ())
    }

    #[cfg(not(feature = "payload-uefi"))]
    {
        use fstart_ffs::slot_state::{TrialConfirm, confirm_trial_and_sync};

        let Ok(mut ab_sub) = shared_flash.subregion(Extent {
            offset: 0,
            size: AB_NVRAM_OFFSET,
        }) else {
            fstart_log::error!("amd-ab rw: A/B subregion bounds error");
            halt();
        };

        let mut trial_cfg = TrialConfirm {
            layout,
            running_slot,
            running_fw_version: h.slot.fw_version.get(),
            running_sec_version: h.slot.sec_version.get(),
            running_root_digest: h.slot.root_digest,
            effective_floor: h.slot.effective_floor.get(),
            has_tpm_floor: h.tpm_present != 0,
            trial_active: h.slot.trial_active != 0,
            sync_pending: h.slot.sync_pending != 0,
        };

        // Run trial confirmation + sync, full verified FCH SPI lockdown, and
        // TPM platform hierarchy lockdown inside the payload launcher's
        // `pre_launch_hook` so direct-boot Linux confirms a trial ONLY after
        // the kernel payload has been verified and placed in DRAM.
        let mut pre_launch_hook = || -> Result<(), &'static str> {
            match confirm_trial_and_sync(&mut ab_sub, &mut trial_cfg) {
                Ok(true) => {
                    fstart_log::info!(
                        "amd-ab rw: trial confirmed (slot={} fw={} sec={})",
                        slot_name(running_slot),
                        h.slot.fw_version.get(),
                        h.slot.sec_version.get(),
                    );
                }
                Ok(false) => {}
                Err(_) => return Err("A/B trial confirmation failed"),
            }
            finalize_fch_flash_lock(fch, layout)?;
            ensure_tpm_platform_locked(&h)?;
            Ok(())
        };

        let mut launch_handoff =
            fstart_payload::Handoff::new(payload_handoff.description, payload_handoff.placement);
        launch_handoff.pre_launch_hook = Some(&mut pre_launch_hook);
        fstart_log::flush();
        fstart_payload::boot(launch_handoff, ())
    }
}
