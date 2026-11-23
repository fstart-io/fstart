// SPDX-License-Identifier: MIT OR Apache-2.0
//! QEMU Q35 RO/A/B firmware update platform flow.
//!
//! Two-stage x86_64 flow over an 8 MiB `pflash_cfi01` chip (QEMU's x86
//! combined firmware ceiling):
//!
//! 1. **RO stage (`run_q35_ab_ro`)**: XIP at top of flash (`0xffe8_0000..4 GiB`)
//!    with its own small DRAM scratch (`0x0080_0000..0x00c0_0000`). Reads its
//!    build-patched [`TrustBlock`](fstart_core::ffs::trust::TrustBlock), probes
//!    the TPM 2.0 rollback floor NV index, finalizes `Phase::FloorPending`
//!    (verifying both RW slots before advancing the TPM NV floor), locks the
//!    TPM NV index via `TPM2_NV_WriteLock`, runs [`fstart_stage::slot::select_slot`],
//!    writes [`Q35AbHandoff`] to [`Q35_AB_HANDOFF_ADDR`], loads the authenticated
//!    RW `ramstage` to fixed DRAM (`0x0100_0000`) via `load_bootstrap`, and
//!    jumps to `_start_ram`.
//! 2. **RW ramstage (`run_q35_ab_ramstage`)**: runs from fixed DRAM, brings up
//!    Q35 PCI/ACPI/display/SMM/TPM, confirms any active trial (including
//!    power-fail-safe sync-before-bump), and hands off to CrabEFI with
//!    `FstartCapsuleBackend` and `FlashStorage<PflashSubRegion>` over `NVRAM`.

/// 8 MiB combined firmware window below 4 GiB:
/// - `system.flash1` (`0xff80_0000..0xffe8_0000`, 6.5 MiB): writable RW pflash
/// - `system.flash0` (`0xffe8_0000..0x1_0000_0000`, 1.5 MiB): QEMU-enforced `readonly=on` RO pflash
pub const Q35_AB_FLASH_BASE: u64 = 0xff80_0000;
/// 8 MiB combined firmware capacity (QEMU x86 `pc_system_flash` maximum).
pub const Q35_AB_FLASH_SIZE: u64 = 0x0080_0000;

/// 256 KiB EFI NVRAM variable store at flash offset 0.
pub const Q35_AB_NVRAM_OFFSET: u32 = 0x0000_0000;
pub const Q35_AB_NVRAM_SIZE: u32 = 0x0004_0000;

/// Two 4 KiB ping-pong `SLOT_STATE` journal sectors.
pub const Q35_AB_STATE_SECTOR_0: u32 = 0x0004_0000;
pub const Q35_AB_STATE_SECTOR_1: u32 = 0x0004_1000;

/// 3 MiB RW slot A (`Root` at slot offset 0, LZ4 `ramstage`).
pub const Q35_AB_RW_A_OFFSET: u32 = 0x0008_0000;
pub const Q35_AB_RW_A_SIZE: u32 = 0x0030_0000;

/// 3 MiB RW slot B (`Root` at slot offset 0, LZ4 `ramstage`).
pub const Q35_AB_RW_B_OFFSET: u32 = 0x0038_0000;
pub const Q35_AB_RW_B_SIZE: u32 = 0x0030_0000;

/// 1.5 MiB top-aligned XIP RO region (`0xffe8_0000..0x1_0000_0000`, `readonly=on` pflash0).
pub const Q35_AB_RO_BASE: u64 = 0xffe8_0000;
pub const Q35_AB_RO_SIZE: u64 = 0x0018_0000;

/// 6.5 MiB writable RW bank (`0xff80_0000..0xffe8_0000`, pflash1).
pub const Q35_AB_RW_SIZE: u32 = (Q35_AB_FLASH_SIZE - Q35_AB_RO_SIZE) as u32;

/// RO stage DRAM workspace (`0x0080_0000..0x00c0_0000`, disjoint from ramstage).
pub const Q35_AB_RO_RAM_BASE: u64 = 0x0080_0000;
pub const Q35_AB_RO_RAM_SIZE: u64 = 0x0040_0000;
pub const Q35_AB_RO_STACK_SIZE: u64 = 0x0004_0000;
pub const Q35_AB_RO_HEAP_SIZE: u64 = 0x0004_0000;

/// Dedicated 4 KiB DRAM handoff page between RO and RW ramstage.
pub const Q35_AB_HANDOFF_ADDR: u64 = 0x00ff_0000;

/// Fixed DRAM load address for RW `ramstage`.
pub const Q35_AB_RAMSTAGE_LOAD_ADDR: u64 = 0x0100_0000;
/// Maximum flat binary size of RW `ramstage` (code + rodata + initialized data).
pub const Q35_AB_RAMSTAGE_IMAGE_SIZE: u64 = 0x0080_0000;
/// Top of low RAM below the 16 MiB TSEG reservation (`0x3f00_0000..0x4000_0000`).
pub const Q35_AB_RAMSTAGE_RAM_END: u64 = 0x3f00_0000;
pub const Q35_AB_RAMSTAGE_STACK_SIZE: u64 = 0x0040_0000;
pub const Q35_AB_RAMSTAGE_HEAP_SIZE: u64 = 0x0020_0000;

/// Default ESRT / FMP firmware class GUID for QEMU Q35 A/B test builds:
/// `7c3a9f10-4b2e-4d8a-9c5e-1f2a3b4c5d6e`.
pub const Q35_AB_FIRMWARE_GUID: [u8; 16] = [
    0x10, 0x9f, 0x3a, 0x7c, 0x2e, 0x4b, 0x8a, 0x4d, 0x9c, 0x5e, 0x1f, 0x2a, 0x3b, 0x4c, 0x5d, 0x6e,
];

/// Closed configuration for the QEMU Q35 A/B RO selector stage.
#[derive(Debug, Clone, Copy)]
pub struct Q35AbRoConfig {
    pub flash_base: u64,
    /// Size of the writable RW pflash bank (`system.flash1`).
    pub flash_size: u32,
    pub state_sector_0: u32,
    pub state_sector_1: u32,
    pub slot_a_offset: u32,
    pub slot_a_size: u32,
    pub slot_b_offset: u32,
    pub slot_b_size: u32,
    /// Auto-provision the TPM 2.0 rollback floor NV index on initial factory
    /// boot (`seq == 1 && phase == Stable`).
    pub auto_provision_tpm: bool,
    /// Require a FIFO TPM 2.0 to be present (fail closed when absent so an
    /// attacker cannot bypass anti-rollback by disabling or detaching the TPM).
    pub require_tpm: bool,
    /// Require an initialized `SLOT_STATE` journal (fail closed on blank).
    pub provisioned: bool,
    /// TPM 2.0 NV index holding the monotonic rollback floor.
    pub rollback_nv_index: u32,
}

impl Q35AbRoConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            flash_base: Q35_AB_FLASH_BASE,
            flash_size: Q35_AB_RW_SIZE,
            state_sector_0: Q35_AB_STATE_SECTOR_0,
            state_sector_1: Q35_AB_STATE_SECTOR_1,
            slot_a_offset: Q35_AB_RW_A_OFFSET,
            slot_a_size: Q35_AB_RW_A_SIZE,
            slot_b_offset: Q35_AB_RW_B_OFFSET,
            slot_b_size: Q35_AB_RW_B_SIZE,
            auto_provision_tpm: true,
            require_tpm: true,
            provisioned: true,
            rollback_nv_index: 0x0140_0001,
        }
    }
}

impl Default for Q35AbRoConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Closed configuration for the QEMU Q35 A/B RW ramstage.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[derive(Debug, Clone, Copy)]
pub struct Q35AbRamstageConfig {
    pub q35: crate::QemuQ35Config,
    pub flash_base: u64,
    /// Size of the writable RW pflash bank (`system.flash1`).
    pub flash_size: u32,
    pub nvram_offset: u32,
    pub nvram_size: u32,
    pub firmware_guid: [u8; 16],
    pub lowest_supported_version: u32,
    /// Trusted X.509 DER certificates for UEFI FMP PKCS#7 capsule verification.
    pub capsule_trust_certs: &'static [&'static [u8]],
    /// Fault-injection hook for automated rollback testing: when true and
    /// `trial_active` is set, trigger an immediate cold reset before
    /// confirming the trial.
    pub fail_trial_before_confirm: bool,
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
impl Q35AbRamstageConfig {
    #[must_use]
    pub const fn new(capsule_trust_certs: &'static [&'static [u8]]) -> Self {
        let mut q35 = crate::QemuQ35Config::new();
        q35.firmware_base = Q35_AB_FLASH_BASE;
        q35.firmware_size = Q35_AB_FLASH_SIZE;
        Self {
            q35,
            flash_base: Q35_AB_FLASH_BASE,
            flash_size: Q35_AB_RW_SIZE,
            nvram_offset: Q35_AB_NVRAM_OFFSET,
            nvram_size: Q35_AB_NVRAM_SIZE,
            firmware_guid: Q35_AB_FIRMWARE_GUID,
            lowest_supported_version: 1,
            capsule_trust_certs,
            fail_trial_before_confirm: false,
        }
    }

    #[must_use]
    pub const fn with_fail_trial(mut self, fail: bool) -> Self {
        self.fail_trial_before_confirm = fail;
        self
    }
}

#[cfg(all(
    any(feature = "q35-ab-ro", feature = "q35-ab-ramstage"),
    target_arch = "x86_64"
))]
mod handoff {
    use fstart_core::ffs::trust::TRUST_SIZE;
    use fstart_ffs::slot_state::Slot;
    use fstart_stage::slot::ActiveSlotHandoff;
    use zerocopy::byteorder::{LE, U32, U64};
    use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

    pub const HANDOFF_MAGIC: [u8; 4] = *b"Q3AB";
    pub const HANDOFF_VERSION: u32 = 1;

    /// Wire-safe handoff written by RO at [`super::Q35_AB_HANDOFF_ADDR`] and
    /// consumed by RW ramstage.
    #[derive(Clone, Copy, Debug, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
    #[repr(C)]
    pub struct Q35AbHandoff {
        pub magic: [u8; 4],
        pub version: U32<LE>,
        pub tpm_present: u8,
        pub reserved: [u8; 7],
        pub tpm_floor: U64<LE>,
        pub slot: ActiveSlotHandoff,
        pub trust_bytes: [u8; TRUST_SIZE],
    }

    const _: () = assert!(core::mem::size_of::<Q35AbHandoff>() <= 4096);

    pub fn slot_name(slot: Slot) -> &'static str {
        match slot {
            Slot::A => "A",
            Slot::B => "B",
            Slot::Recovery => "Recovery",
        }
    }
}

#[cfg(all(feature = "q35-ab-ro", target_arch = "x86_64"))]
mod ro {
    use super::handoff::{HANDOFF_MAGIC, HANDOFF_VERSION, Q35AbHandoff, slot_name};
    use super::{
        Q35_AB_HANDOFF_ADDR, Q35_AB_RAMSTAGE_IMAGE_SIZE, Q35_AB_RAMSTAGE_LOAD_ADDR,
        Q35_AB_RAMSTAGE_RAM_END, Q35AbRoConfig,
    };
    use crate::fw_cfg::{QemuFwCfg, QemuFwCfgIoConfig};
    use crate::pflash::Pflash;
    use fstart_core::ffs::trust::{TRUST_SIZE, TrustBlock};
    use fstart_core::services::BootMedia;
    use fstart_core::services::memory_detect::{E820Entry, E820Kind};
    use fstart_crypto::digest::Sha256Hasher;
    use fstart_driver_tpm::tis::{self, Tis};
    use fstart_driver_tpm::tpm20::{self, FloorAdvance, StartupType};
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use fstart_ffs::root::{AuthenticatedRoot, BootstrapDescriptor, BootstrapRole, RootPolicy};
    use fstart_ffs::slot_state::{
        AppendPlan, JournalScan, Phase, ScanStatus, Slot, SlotExtent, SlotLayout, plan_ro_append,
        program_record_verified, scan_journal_flash,
    };
    use fstart_stage::boot::{Measurement, MemoryPolicy, MemoryWindow, VerifiedExecutable};
    use fstart_stage::root::{SliceBootMedia, read_boot_root_with_digest};
    use fstart_stage::slot::{
        ActiveSlotHandoff, RecoveryReason, SelectorPolicy, SlotDecision, select_slot,
    };
    use zerocopy::IntoBytes;
    use zerocopy::byteorder::{U32, U64};

    fn tpm_err_parts(err: fstart_driver_tpm::Error) -> (&'static str, u32) {
        match err {
            fstart_driver_tpm::Error::Timeout => ("timeout", 0),
            fstart_driver_tpm::Error::Locality => ("locality", 0),
            fstart_driver_tpm::Error::Protocol => ("protocol", 0),
            fstart_driver_tpm::Error::Response => ("response", 0),
            fstart_driver_tpm::Error::Code(code) => ("code", code),
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

    /// Verify that QEMU's E820 RAM map covers `[start, end)` completely.
    fn e820_covers_ram(entries: &[E820Entry], start: u64, end: u64) -> bool {
        entries.iter().any(|e| {
            e.kind == E820Kind::Ram as u32
                && e.addr <= start
                && e.addr.saturating_add(e.size) >= end
        })
    }

    /// Authenticate a slot's signed `Root`, verify its `Mainstage` bootstrap
    /// descriptor bounds, and stream-hash the entire stored payload against
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
        if descriptor.load_addr != Q35_AB_RAMSTAGE_LOAD_ADDR
            || descriptor.loaded_size > Q35_AB_RAMSTAGE_IMAGE_SIZE
        {
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
            start: Q35_AB_RAMSTAGE_LOAD_ADDR,
            size: Q35_AB_RAMSTAGE_IMAGE_SIZE,
        }];
        let load_policy = MemoryPolicy {
            writable: &writable,
            reserved: &[],
            entry_alignment: 1,
        };
        let codec = const { fstart_stage::boot::codec(crate::compression::PAYLOAD) };
        // SAFETY: writable covers exclusively the ramstage image window
        // (0x0100_0000..0x0180_0000), verified against QEMU's E820 RAM map and
        // disjoint from RO's RAM (0x0080_0000..0x00c0_0000), handoff page
        // (0x00ff_0000), and pflash (0xff80_0000..4 GiB).
        unsafe {
            fstart_stage::boot::load_bootstrap(
                &slice,
                descriptor,
                &load_policy,
                codec,
                &Measurement::NONE,
            )
        }
    }

    fn append_ro_record(
        pflash: &mut Pflash,
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
        program_record_verified(pflash, abs, record).is_ok()
    }

    /// Execute the QEMU Q35 A/B RO bootblock selector and jump to RW ramstage.
    pub fn run_q35_ab_ro(config: &'static Q35AbRoConfig, console_cfg: Ns16550Config) -> ! {
        let Ok(mut console) = Ns16550::new(console_cfg) else {
            fstart_arch::x86::boot::halt();
        };
        if console.init().is_err() {
            fstart_arch::x86::boot::halt();
        }
        // SAFETY: RO owns the console exclusively until jumping to ramstage.
        unsafe { fstart_log::init(&console) };
        fstart_log::info!("qemu-q35-ab ro: entered");

        // Validate guest RAM via fw_cfg before touching the handoff page or
        // loading ramstage into fixed DRAM.
        let mut fw_cfg = QemuFwCfg::new(QemuFwCfgIoConfig::new());
        if fw_cfg.init().is_err() {
            fstart_log::error!("qemu-q35-ab ro: fw_cfg init failed");
            fstart_arch::x86::boot::halt();
        }
        let mut e820_entries = [E820Entry {
            addr: 0,
            size: 0,
            kind: 0,
        }; 32];
        let Ok(e820_count) = fw_cfg.detect_memory(&mut e820_entries) else {
            fstart_log::error!("qemu-q35-ab ro: fw_cfg E820 detection failed");
            fstart_arch::x86::boot::halt();
        };
        let e820 = &e820_entries[..e820_count];
        if !e820_covers_ram(e820, Q35_AB_HANDOFF_ADDR, Q35_AB_HANDOFF_ADDR + 4096)
            || !e820_covers_ram(e820, Q35_AB_RAMSTAGE_LOAD_ADDR, Q35_AB_RAMSTAGE_RAM_END)
        {
            fstart_log::error!(
                "qemu-q35-ab ro: insufficient guest RAM (need >= 1 GiB covering handoff + ramstage)"
            );
            fstart_arch::x86::boot::halt();
        }

        // Read build-patched TrustBlock from RO flash via volatile byte reads.
        let mut trust_bytes = [0u8; TRUST_SIZE];
        let trust_raw = fstart_stage::fstart_trust_bytes();
        for (dst, src) in trust_bytes.iter_mut().zip(trust_raw.iter()) {
            // SAFETY: immutable build-patched static in ROM; mmio::read8 stops
            // the compiler from folding the link-time placeholder bytes.
            *dst = unsafe { fstart_core::mmio::read8(src as *const u8) };
        }
        let Some(trust) = TrustBlock::parse(&trust_bytes).filter(|t| t.key_count.get() > 0) else {
            fstart_log::error!("qemu-q35-ab ro: missing or invalid TrustBlock");
            fstart_arch::x86::boot::halt();
        };

        let layout = SlotLayout::RoAB {
            state_sector_0: config.state_sector_0,
            state_sector_1: config.state_sector_1,
            slot_a: SlotExtent {
                offset: config.slot_a_offset,
                size: config.slot_a_size,
            },
            slot_b: SlotExtent {
                offset: config.slot_b_offset,
                size: config.slot_b_size,
            },
        };
        if layout.validate(config.flash_size).is_err() {
            fstart_log::error!("qemu-q35-ab ro: invalid slot layout");
            fstart_arch::x86::boot::halt();
        }

        // SAFETY: Q35 maps writable RW pflash1 uncached at flash_base..flash_base+flash_size.
        let mut pflash = unsafe { Pflash::new(config.flash_base as usize, config.flash_size) };
        let pflash_reader = pflash.reader();
        let (s0, s1) = layout.state_sectors();
        let initial_scan = scan_journal_flash(&mut pflash, s0, s1);

        // Probe TPM 2.0 TIS, read/provision rollback floor, finalize FloorPending,
        // and unconditionally write-lock the NV index before leaving RO.
        // SAFETY: Q35 decodes the TIS window at 0xfed4_0000 when a TPM is attached.
        let mut tis = unsafe { Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
        let mut tpm_present = false;
        let mut tpm_floor: u64 = 0;

        if let Some(id) = tis.probe()
            && !tis.is_crb()
        {
            fstart_log::info!(
                "qemu-q35-ab ro: TPM {:#x}:{:#x} rev {}",
                id.vendor,
                id.device,
                id.revision,
            );
            match tpm20::startup(&mut tis, StartupType::Clear) {
                Ok(()) | Err(fstart_driver_tpm::Error::Code(tpm20::rc::INITIALIZE)) => {}
                Err(e) => {
                    let (kind, code) = tpm_err_parts(e);
                    fstart_log::error!(
                        "qemu-q35-ab ro: TPM2_Startup failed: {} ({:#x})",
                        kind,
                        code,
                    );
                    fstart_arch::x86::boot::halt();
                }
            }
            if let Err(e) = tpm20::self_test(&mut tis, false) {
                let (kind, code) = tpm_err_parts(e);
                fstart_log::error!(
                    "qemu-q35-ab ro: TPM2_SelfTest failed: {} ({:#x})",
                    kind,
                    code,
                );
                fstart_arch::x86::boot::halt();
            }

            match tpm20::read_rollback_floor(&mut tis, config.rollback_nv_index) {
                Ok(floor) => {
                    tpm_present = true;
                    tpm_floor = floor;
                    fstart_log::info!("qemu-q35-ab ro: TPM rollback floor = {}", tpm_floor);
                }
                Err(fstart_driver_tpm::Error::Code(code))
                    if tpm20::rc::base(code) == tpm20::rc::HANDLE
                        || code == tpm20::rc::NV_UNINITIALIZED =>
                {
                    let allow_factory = match &initial_scan {
                        Ok(scan) => match scan.status {
                            ScanStatus::Valid { record, .. } => {
                                config.auto_provision_tpm
                                    && record.seq == 1
                                    && record.phase == Phase::Stable
                            }
                            ScanStatus::Blank => config.auto_provision_tpm && !config.provisioned,
                            _ => false,
                        },
                        Err(_) => false,
                    };
                    if !allow_factory {
                        fstart_log::error!(
                            "qemu-q35-ab ro: TPM rollback NV index undefined on provisioned device"
                        );
                        fstart_arch::x86::boot::halt();
                    }
                    // Derive initial floor strictly from an authenticated RW Slot A
                    // (never from unauthenticated journal bytes).
                    let factory_policy = RootPolicy {
                        image_family: trust.image_family,
                        minimum_security_version: trust.minimum_security_version.get(),
                        image_size: 0,
                        max_directory_size: 64 * 1024,
                        keys: trust.valid_keys(),
                    };
                    let Some((auth_a, _, _)) = layout.extent(Slot::A).and_then(|ext| {
                        verify_slot_bootable(
                            &pflash_reader,
                            ext,
                            &factory_policy,
                            trust.minimum_security_version.get(),
                            None,
                        )
                    }) else {
                        fstart_log::error!(
                            "qemu-q35-ab ro: cannot factory-provision TPM floor without authentic Slot A"
                        );
                        fstart_arch::x86::boot::halt();
                    };
                    let init_floor = auth_a
                        .root()
                        .security_version
                        .max(trust.minimum_security_version.get());
                    let provisioned = if code == tpm20::rc::NV_UNINITIALIZED {
                        tpm20::resume_uninitialized_rollback_floor(
                            &mut tis,
                            config.rollback_nv_index,
                            init_floor,
                        )
                    } else {
                        tpm20::provision_rollback_floor(
                            &mut tis,
                            config.rollback_nv_index,
                            init_floor,
                        )
                    };
                    match provisioned {
                        Ok(floor) => {
                            tpm_present = true;
                            tpm_floor = floor;
                            fstart_log::info!(
                                "qemu-q35-ab ro: provisioned TPM rollback floor = {}",
                                tpm_floor,
                            );
                        }
                        Err(e) => {
                            let (kind, code) = tpm_err_parts(e);
                            fstart_log::error!(
                                "qemu-q35-ab ro: TPM rollback provisioning failed: {} ({:#x})",
                                kind,
                                code,
                            );
                            fstart_arch::x86::boot::halt();
                        }
                    }
                }
                Err(e) => {
                    let (kind, code) = tpm_err_parts(e);
                    fstart_log::error!(
                        "qemu-q35-ab ro: TPM rollback floor read failed: {} ({:#x})",
                        kind,
                        code,
                    );
                    fstart_arch::x86::boot::halt();
                }
            }

            // Advance TPM rollback floor when ramstage left Phase::FloorPending.
            if let Ok(scan) = &initial_scan
                && let ScanStatus::Valid { record, .. } = scan.status
                && record.phase == Phase::FloorPending
            {
                let base_policy = RootPolicy {
                    image_family: trust.image_family,
                    minimum_security_version: trust.minimum_security_version.get().max(tpm_floor),
                    image_size: 0,
                    max_directory_size: 64 * 1024,
                    keys: trust.valid_keys(),
                };
                let can_finalize = if record.sec_version <= tpm_floor {
                    // Power cut happened after NV write but before journal write.
                    true
                } else {
                    let slots_ready = if layout.is_ab() {
                        record.is_sync_done()
                            && layout.extent(Slot::A).is_some_and(|e| {
                                verify_slot_bootable(
                                    &pflash_reader,
                                    e,
                                    &base_policy,
                                    record.sec_version,
                                    Some(&record.root_digest),
                                )
                                .is_some()
                            })
                            && layout.extent(Slot::B).is_some_and(|e| {
                                verify_slot_bootable(
                                    &pflash_reader,
                                    e,
                                    &base_policy,
                                    record.sec_version,
                                    Some(&record.root_digest),
                                )
                                .is_some()
                            })
                    } else {
                        layout.extent(Slot::A).is_some_and(|e| {
                            verify_slot_bootable(
                                &pflash_reader,
                                e,
                                &base_policy,
                                record.sec_version,
                                Some(&record.root_digest),
                            )
                            .is_some()
                        })
                    };
                    if slots_ready {
                        match tpm20::advance_rollback_floor(
                            &mut tis,
                            config.rollback_nv_index,
                            record.sec_version,
                        ) {
                            Ok(FloorAdvance::Advanced(f) | FloorAdvance::AlreadyAtOrAbove(f)) => {
                                tpm_floor = f;
                                fstart_log::info!(
                                    "qemu-q35-ab ro: TPM rollback floor advanced to {}",
                                    tpm_floor,
                                );
                                true
                            }
                            Err(e) => {
                                let (kind, code) = tpm_err_parts(e);
                                fstart_log::error!(
                                    "qemu-q35-ab ro: TPM rollback floor advance failed: {} ({:#x})",
                                    kind,
                                    code,
                                );
                                // Re-read TPM NV so an ambiguous write/verify error cannot leave
                                // our in-memory tpm_floor stale relative to hardware.
                                match tpm20::read_rollback_floor(&mut tis, config.rollback_nv_index)
                                {
                                    Ok(actual) => {
                                        tpm_floor = actual;
                                        actual >= record.sec_version
                                    }
                                    Err(re_err) => {
                                        let (rk, rc) = tpm_err_parts(re_err);
                                        fstart_log::error!(
                                            "qemu-q35-ab ro: TPM floor re-read failed: {} ({:#x})",
                                            rk,
                                            rc,
                                        );
                                        fstart_arch::x86::boot::halt();
                                    }
                                }
                            }
                        }
                    } else {
                        fstart_log::warn!(
                            "qemu-q35-ab ro: FloorPending skipped (slots not synced/verified)"
                        );
                        false
                    }
                };
                if can_finalize
                    && let Some(finalized) = record.next_floor_finalized()
                    && append_ro_record(&mut pflash, layout, scan, &finalized)
                {
                    fstart_log::info!("qemu-q35-ab ro: slot state finalized to Stable");
                }
            }

            // Lock the TPM rollback floor NV index BEFORE leaving RO.
            if let Err(e) = tpm20::lock_rollback_floor(&mut tis, config.rollback_nv_index) {
                let (kind, code) = tpm_err_parts(e);
                fstart_log::error!(
                    "qemu-q35-ab ro: failed to lock TPM rollback floor: {} ({:#x})",
                    kind,
                    code,
                );
                fstart_arch::x86::boot::halt();
            }
            fstart_log::info!("qemu-q35-ab ro: TPM rollback floor locked");
        } else if config.require_tpm {
            fstart_log::error!("qemu-q35-ab ro: required FIFO TPM 2.0 absent");
            fstart_arch::x86::boot::halt();
        } else {
            fstart_log::warn!(
                "qemu-q35-ab ro: WARNING no FIFO TPM present; hardware anti-rollback disabled"
            );
            // Without a TPM, clear any stuck FloorPending state back to Stable.
            if let Ok(scan) = &initial_scan
                && let ScanStatus::Valid { record, .. } = scan.status
                && record.phase == Phase::FloorPending
                && let Some(finalized) = record.next_floor_finalized()
            {
                let _ = append_ro_record(&mut pflash, layout, scan, &finalized);
            }
        }

        let effective_floor = trust.minimum_security_version.get().max(tpm_floor);
        let policy = RootPolicy {
            image_family: trust.image_family,
            minimum_security_version: effective_floor,
            image_size: 0,
            max_directory_size: 64 * 1024,
            keys: trust.valid_keys(),
        };
        let decision = select_slot(
            layout,
            SelectorPolicy {
                provisioned: config.provisioned,
            },
            &mut pflash,
            &pflash_reader,
            &policy,
        );

        let (
            mut slot,
            mut extent,
            mut root,
            mut root_digest,
            record,
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
                    "qemu-q35-ab ro: recovery required ({})",
                    recovery_reason_str(reason),
                );
                fstart_arch::x86::boot::halt();
            }
        };

        fstart_log::info!(
            "qemu-q35-ab ro: selected slot {} fw={} sec={} floor={} trial={} sync={}",
            slot_name(slot),
            root.root().fw_version,
            root.root().security_version,
            effective_floor,
            u8::from(trial_active),
            u8::from(sync_pending),
        );

        let primary_loaded = root
            .root()
            .descriptors
            .iter()
            .flatten()
            .find(|d| {
                d.role == BootstrapRole::Mainstage && d.load_addr == Q35_AB_RAMSTAGE_LOAD_ADDR
            })
            .copied()
            .ok_or(fstart_stage::boot::LoadError::Bounds)
            .and_then(|desc| try_load_slot(&pflash_reader, extent, &desc));

        let verified = match primary_loaded {
            Ok(v) => v,
            Err(primary_err) => {
                fstart_log::error!(
                    "qemu-q35-ab ro: primary slot {} load failed ({:?}); trying peer slot",
                    slot_name(slot),
                    primary_err,
                );
                let peer_loaded = slot
                    .other_rw()
                    .filter(|_| layout.is_ab())
                    .and_then(|peer| Some((peer, layout.extent(peer)?)))
                    .and_then(|(peer, peer_ext)| {
                        let (peer_root, peer_digest, peer_desc) = verify_slot_bootable(
                            &pflash_reader,
                            peer_ext,
                            &policy,
                            effective_floor,
                            None,
                        )?;
                        let v = try_load_slot(&pflash_reader, peer_ext, &peer_desc).ok()?;
                        Some((peer, peer_ext, peer_root, peer_digest, v))
                    });
                let Some((peer, peer_ext, peer_root, peer_digest, v)) = peer_loaded else {
                    fstart_log::error!("qemu-q35-ab ro: peer slot fallback also failed");
                    fstart_arch::x86::boot::halt();
                };
                fstart_log::warn!(
                    "qemu-q35-ab ro: fell back to peer slot {} fw={} sec={}",
                    slot_name(peer),
                    peer_root.root().fw_version,
                    peer_root.root().security_version,
                );
                slot = peer;
                extent = peer_ext;
                root = peer_root;
                root_digest = peer_digest;
                trial_active = false;
                sync_pending = false;
                v
            }
        };

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
        let handoff = Q35AbHandoff {
            magic: HANDOFF_MAGIC,
            version: U32::new(HANDOFF_VERSION),
            tpm_present: u8::from(tpm_present),
            reserved: [0; 7],
            tpm_floor: U64::new(tpm_floor),
            slot: slot_handoff,
            trust_bytes,
        };
        // SAFETY: Q35_AB_HANDOFF_ADDR is a dedicated 4 KiB DRAM page verified
        // against QEMU's E820 RAM map and disjoint from RO and ramstage.
        unsafe {
            core::ptr::copy_nonoverlapping(
                handoff.as_bytes().as_ptr(),
                Q35_AB_HANDOFF_ADDR as *mut u8,
                core::mem::size_of::<Q35AbHandoff>(),
            );
        }

        fstart_log::info!(
            "qemu-q35-ab ro: jumping to ramstage at {:#x} (slot size {} bytes)",
            verified.entry(),
            extent.size,
        );
        fstart_arch::x86::boot::jump_to(verified.entry())
    }
}

#[cfg(all(feature = "q35-ab-ro", target_arch = "x86_64"))]
pub use ro::run_q35_ab_ro;

#[cfg(all(feature = "q35-ab-ramstage", target_arch = "x86_64"))]
mod ramstage {
    use super::handoff::{HANDOFF_MAGIC, HANDOFF_VERSION, Q35AbHandoff, slot_name};
    use super::{Q35_AB_HANDOFF_ADDR, Q35_AB_RO_BASE, Q35_AB_RO_SIZE, Q35AbRamstageConfig};
    use crate::display::{BochsDisplay, BochsDisplayConfig};
    use crate::fw_cfg::QemuFwCfg;
    #[cfg(feature = "payload-uefi")]
    use crate::pflash::{PFLASH_SECTOR_SIZE, Pflash};
    use crate::q35::Q35HostBridge;
    use crate::{QEMU_Q35_ACPI_BUFFER_SIZE, QEMU_Q35_PLATFORM_NODE};
    use core::sync::atomic::{AtomicBool, Ordering};
    #[cfg(feature = "payload-uefi")]
    use fstart_core::ffs::trust::TrustBlock;
    use fstart_core::services::memory_detect::E820State;
    use fstart_core::services::{ConsoleDevice, FlashRegion, ServiceError};
    use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
    use zerocopy::FromBytes;

    #[cfg(fstart_qemu_has_smm_image)]
    const SMM_IMAGE: Option<&'static [u8]> = Some(include_bytes!(env!("FSTART_SMM_IMAGE")));
    #[cfg(not(fstart_qemu_has_smm_image))]
    const SMM_IMAGE: Option<&'static [u8]> = None;

    #[repr(C, align(4096))]
    struct AcpiBuffer([u8; QEMU_Q35_ACPI_BUFFER_SIZE]);
    static mut ACPI_BUFFER: AcpiBuffer = AcpiBuffer([0; QEMU_Q35_ACPI_BUFFER_SIZE]);

    /// Software write-lock latch backing `FstartCapsuleBackend`'s `lock_fn` on
    /// QEMU Q35 (where ICH9 SPI BIOS_CNTL does not gate `pflash_cfi01`).
    #[cfg(feature = "payload-uefi")]
    static PFLASH_WRITE_LOCKED: AtomicBool = AtomicBool::new(false);

    #[cfg(feature = "payload-uefi")]
    struct GuardedPflash {
        inner: Pflash,
    }

    #[cfg(feature = "payload-uefi")]
    impl FlashRegion for GuardedPflash {
        fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ServiceError> {
            self.inner.read(offset, buf)
        }

        fn erase(&mut self, offset: u32, length: u32) -> Result<(), ServiceError> {
            if PFLASH_WRITE_LOCKED.load(Ordering::Acquire) {
                return Err(ServiceError::NotSupported);
            }
            self.inner.erase(offset, length)
        }

        fn program(&mut self, offset: u32, data: &[u8]) -> Result<(), ServiceError> {
            if PFLASH_WRITE_LOCKED.load(Ordering::Acquire) {
                return Err(ServiceError::NotSupported);
            }
            self.inner.program(offset, data)
        }
    }

    // Suppress unused import warning when payload-uefi is disabled.
    #[cfg(not(feature = "payload-uefi"))]
    const _: () = {
        let _ = (Ordering::Acquire, AtomicBool::new(false));
        let _ = core::mem::size_of::<&dyn FlashRegion>();
    };

    struct QemuQ35AbRamstage {
        config: &'static Q35AbRamstageConfig,
        handoff: Q35AbHandoff,
        console: Ns16550,
        fw_cfg: QemuFwCfg,
        hostbridge: Q35HostBridge,
        e820: E820State,
        acpi_rsdp: Option<u64>,
        framebuffer: Option<fstart_core::services::FramebufferInfo>,
    }

    impl QemuQ35AbRamstage {
        const TSEG_RESERVE: u64 = 0x0100_0000;

        fn new(
            config: &'static Q35AbRamstageConfig,
            console_cfg: Ns16550Config,
        ) -> Result<Self, ServiceError> {
            let mut bytes = [0u8; core::mem::size_of::<Q35AbHandoff>()];
            // SAFETY: RO wrote Q35AbHandoff at Q35_AB_HANDOFF_ADDR before jumping.
            unsafe {
                core::ptr::copy_nonoverlapping(
                    Q35_AB_HANDOFF_ADDR as *const u8,
                    bytes.as_mut_ptr(),
                    bytes.len(),
                );
            }
            let handoff =
                Q35AbHandoff::read_from_bytes(&bytes).map_err(|_| ServiceError::InvalidParam)?;
            Ok(Self {
                config,
                handoff,
                console: Ns16550::new(console_cfg).map_err(|_| ServiceError::HardwareError)?,
                fw_cfg: QemuFwCfg::new(config.q35.fw_cfg),
                hostbridge: Q35HostBridge::new(config.q35.hostbridge)?,
                e820: E820State::new(),
                acpi_rsdp: None,
                framebuffer: None,
            })
        }

        fn init_console(&mut self) -> Result<(), ServiceError> {
            self.console
                .init()
                .map_err(|_| ServiceError::HardwareError)?;
            // SAFETY: ramstage owns the console until payload handoff.
            unsafe { fstart_log::init(&self.console) };
            fstart_log::info!("qemu-q35-ab {}", crate::messages::RAMSTAGE_ENTERED);
            if self.handoff.magic != HANDOFF_MAGIC || self.handoff.version.get() != HANDOFF_VERSION
            {
                fstart_log::error!("qemu-q35-ab ramstage: invalid RO handoff");
                return Err(ServiceError::InvalidParam);
            }
            let Some(slot) = self.handoff.slot.slot() else {
                fstart_log::error!("qemu-q35-ab ramstage: invalid active slot in handoff");
                return Err(ServiceError::InvalidParam);
            };
            fstart_log::info!(
                "qemu-q35-ab ramstage: slot={} fw={} sec={} floor={} trial={} sync={} last_ver={} last_status={}",
                slot_name(slot),
                self.handoff.slot.fw_version.get(),
                self.handoff.slot.sec_version.get(),
                self.handoff.slot.effective_floor.get(),
                self.handoff.slot.trial_active,
                self.handoff.slot.sync_pending,
                self.handoff.slot.last_attempt_ver.get(),
                self.handoff.slot.last_attempt_status.get(),
            );
            Ok(())
        }

        fn detect_memory_and_tables(&mut self) -> Result<(), ServiceError> {
            self.fw_cfg.init()?;
            let count = self.fw_cfg.detect_memory(self.e820.entries_mut())?;
            let total = self.fw_cfg.total_ram_bytes()?;
            self.e820.set_detected(count, total)?;
            self.reserve_tseg()?;
            fstart_log::info!(
                "Detected {} MiB RAM, {} e820 entries from {}",
                self.e820.total_ram() >> 20,
                self.e820.count(),
                QEMU_Q35_PLATFORM_NODE,
            );
            self.hostbridge.setup_ich9_pm_io()?;
            // SAFETY: firmware init is single-threaded; buffer written once.
            let acpi = unsafe { &mut (*core::ptr::addr_of_mut!(ACPI_BUFFER)).0 };
            self.acpi_rsdp = Some(self.fw_cfg.load_acpi_tables(acpi)?);
            Ok(())
        }

        fn reserve_tseg(&mut self) -> Result<(), ServiceError> {
            self.hostbridge.enable_config_access()?;
            let size = crate::q35_smm::decode_tseg_size() as u64;
            if size == 0 {
                return Ok(());
            }
            let base = crate::q35_smm::tseg_base_from_e820(self.e820.entries(), size as usize);
            if base == 0 {
                fstart_log::error!("Q35: unable to locate TSEG");
                return Err(ServiceError::HardwareError);
            }
            self.e820.reserve_range(base, size)?;
            let total = self.e820.total_ram().saturating_sub(size);
            let count = self.e820.count();
            self.e820.set_detected(count, total)?;
            fstart_log::info!("Q35: TSEG reserved base={:#x} size={:#x}", base, size);
            Ok(())
        }

        fn init_pci(&mut self) -> Result<(), ServiceError> {
            self.hostbridge.init_with_e820(self.e820.entries())?;
            fstart_log::info!(
                "qemu-q35-ab: {} ({} devices)",
                crate::messages::PCI_ROOT_READY,
                self.hostbridge.device_count(),
            );
            Ok(())
        }

        fn init_display(&mut self) -> Result<(), ServiceError> {
            let info = BochsDisplay::probe_and_init(
                self.hostbridge.ecam(),
                BochsDisplayConfig {
                    width: 1024,
                    height: 768,
                },
            )
            .map_err(|_| ServiceError::HardwareError)?;
            if let Some(info) = info {
                fstart_log::info!(
                    "qemu-q35-ab: display {}x{} fb={:#x}",
                    info.width,
                    info.height,
                    info.base_addr,
                );
                self.framebuffer = Some(info);
            }
            Ok(())
        }

        fn map_memory(&self) -> Result<(), ServiceError> {
            use fstart_core::services::memory_detect::E820Kind;
            let ram = self
                .e820
                .entries()
                .iter()
                .filter(|entry| entry.kind == E820Kind::Ram as u32)
                .map(|entry| (entry.addr, entry.size));
            let bars = self
                .hostbridge
                .ecam()
                .assigned_memory_windows()
                .map(|window| (window.base, window.size));
            // SAFETY: BSP only, before APs and payload.
            let tables =
                unsafe { fstart_arch::x86::boot::paging::IdentityTables::build(ram.chain(bars))? };
            let base = unsafe { tables.install() };
            fstart_log::info!("qemu-q35-ab: identity tables at {:#x}", base);
            Ok(())
        }

        fn init_mp_smm(&self) -> Result<(), ServiceError> {
            let cpu = fstart_arch::x86::mp::GenericX86CpuDriver;
            let drivers: [&dyn fstart_arch::x86::mp::CpuDriver; 1] = [&cpu];
            if SMM_IMAGE.is_some() {
                let size = crate::q35_smm::decode_tseg_size() as u64;
                let base = self.hostbridge.tseg_base();
                unsafe extern "C" {
                    static _stack_top: u8;
                }
                // SAFETY: `_stack_top` is defined by the linker script.
                let stack_top = unsafe { &_stack_top as *const u8 as u64 };
                if size == 0 || base == 0 || size > Self::TSEG_RESERVE || base < stack_top {
                    fstart_log::error!(
                        "qemu-q35-ab: TSEG {:#x}+{:#x} exceeds reservation (stack top {:#x})",
                        base,
                        size,
                        stack_top,
                    );
                    return Err(ServiceError::InvalidParam);
                }
            }
            let num_cpus = self.fw_cfg.cpu_count();
            let max_cpus = if let Some(image) = SMM_IMAGE {
                let header = fstart_smm::header::SmmImageHeader::parse(image)
                    .map_err(|_| ServiceError::InvalidParam)?;
                header.entry_count
            } else {
                num_cpus
            };
            let mp = fstart_arch::x86::mp::mp_init(&fstart_arch::x86::mp::MpConfig {
                cpu_drivers: &drivers,
                num_cpus,
                max_cpus,
            })
            .map_err(|_| ServiceError::HardwareError)?;
            if let Some(image) = SMM_IMAGE {
                #[cfg(feature = "tpm")]
                fstart_tcg::measure_blob(
                    0,
                    fstart_tcg::event::EV_POST_CODE2,
                    fstart_tcg::event::post_code::SMM_CODE,
                    image.as_ptr() as u64,
                    image,
                );
                let smi = crate::q35_smm::ich9_smi();
                let smm = fstart_arch::x86::cpu::intel::smm::IntelSmm::new(
                    "Q35",
                    &self.hostbridge,
                    &smi,
                    &crate::q35_smm::QemuSmmCpu,
                    false,
                );
                if let Err(error) = smm.install(&mp, image) {
                    fstart_log::error!("Q35 SMM installation failed: {}", error.name());
                    return Err(ServiceError::HardwareError);
                }
            }
            Ok(())
        }

        #[cfg(feature = "tpm")]
        fn init_tpm(&mut self) -> Result<(), ServiceError> {
            use fstart_driver_tpm::tis::{self, Tis};
            let Some((offset, size)) = crate::fw_cfg::acpi_allocation("etc/tpm/log") else {
                fstart_log::info!("qemu-q35-ab: no TPM log table");
                return Ok(());
            };
            static mut TIS: Option<Tis<fstart_arch::x86::Udelay>> = None;
            // SAFETY: QEMU decodes TIS at 0xfed4_0000 when a TPM is configured.
            let tis = unsafe { Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
            let Some(id) = tis.probe() else {
                return Ok(());
            };
            if tis.is_crb() {
                return Ok(());
            }
            fstart_log::info!(
                "qemu-q35-ab: TPM {:#x}:{:#x} rev {}",
                id.vendor,
                id.device,
                id.revision,
            );
            // SAFETY: single-threaded init; written once.
            let tis = unsafe { (*core::ptr::addr_of_mut!(TIS)).insert(tis) };
            // SAFETY: ACPI_BUFFER is a firmware static reserved from the OS.
            let log = unsafe {
                core::slice::from_raw_parts_mut(
                    (core::ptr::addr_of_mut!(ACPI_BUFFER) as *mut u8).add(offset),
                    size,
                )
            };
            let uefi = cfg!(feature = "payload-uefi");
            match fstart_tcg::start(tis, log, fstart_tcg::PlatformClass::Client, uefi) {
                Ok(_) => {
                    fstart_tcg::measure_crtm_version(fstart_stage::tcg::CRTM_VERSION);
                    // SAFETY: RO flash (`system.flash0`, readonly=on) is permanently
                    // mapped read-only in Read Array mode at Q35_AB_RO_BASE.
                    let ro_flash = unsafe {
                        core::slice::from_raw_parts(
                            Q35_AB_RO_BASE as *const u8,
                            Q35_AB_RO_SIZE as usize,
                        )
                    };
                    fstart_tcg::measure_blob(
                        0,
                        fstart_tcg::event::EV_S_CRTM_CONTENTS,
                        "ro-flash",
                        Q35_AB_RO_BASE,
                        ro_flash,
                    );
                    fstart_tcg::measure_blob(
                        0,
                        fstart_tcg::event::EV_S_CRTM_CONTENTS,
                        "ro-trust",
                        Q35_AB_HANDOFF_ADDR,
                        &self.handoff.trust_bytes,
                    );
                    let slot_offset = self
                        .handoff
                        .slot
                        .layout()
                        .and_then(|l| l.extent(self.handoff.slot.slot()?))
                        .map_or(0, |e| u64::from(e.offset));
                    fstart_tcg::measure_blob(
                        0,
                        fstart_tcg::event::EV_S_CRTM_CONTENTS,
                        "rw-root",
                        self.config.flash_base + slot_offset,
                        &self.handoff.slot.root_digest,
                    );
                }
                Err(error) => {
                    error.report();
                    if error.remedy() == fstart_tcg::Remedy::Halt {
                        return Err(ServiceError::HardwareError);
                    }
                    fstart_log::warn!("qemu-q35-ab: measured boot off");
                }
            }
            Ok(())
        }

        #[cfg(feature = "tpm")]
        fn ensure_tpm_platform_locked(&self, uefi_payload: bool) {
            if self.handoff.tpm_present == 0 {
                return;
            }
            if fstart_tcg::log().is_some() {
                // For UEFI, fstart_payload::boot invokes fstart_tcg::handoff(Locked).
                // For non-UEFI payloads (e.g. Halt), finalize and lock here.
                if !uefi_payload && fstart_tcg::handoff(fstart_tcg::Handoff::Final).is_err() {
                    fstart_log::error!(
                        "qemu-q35-ab ramstage: failed to finalize/lock TPM before payload"
                    );
                    fstart_arch::x86::boot::halt();
                }
                return;
            }
            // TCG log was not started (e.g. missing etc/tpm/log): lock the TPM 2.0
            // platform hierarchy directly so OS/third-party EFI cannot touch NV/platform auth.
            use fstart_driver_tpm::tis::{self, Tis};
            // SAFETY: QEMU decodes TIS at 0xfed4_0000 when a TPM is present.
            let mut tis = unsafe { Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
            if tis.probe().is_none() || tis.is_crb() {
                fstart_log::error!("qemu-q35-ab ramstage: TPM disappeared before platform lock");
                fstart_arch::x86::boot::halt();
            }
            if let Err(e) = fstart_driver_tpm::tpm20::lock_platform_hierarchy(&mut tis) {
                let (kind, code) = match e {
                    fstart_driver_tpm::Error::Timeout => ("timeout", 0u32),
                    fstart_driver_tpm::Error::Locality => ("locality", 0),
                    fstart_driver_tpm::Error::Protocol => ("protocol", 0),
                    fstart_driver_tpm::Error::Response => ("response", 0),
                    fstart_driver_tpm::Error::Code(c) => ("code", c),
                };
                fstart_log::error!(
                    "qemu-q35-ab ramstage: failed to lock TPM platform hierarchy: {} ({:#x})",
                    kind,
                    code,
                );
                fstart_arch::x86::boot::halt();
            }
            fstart_log::info!("qemu-q35-ab ramstage: TPM platform hierarchy locked");
        }

        fn hand_off(&self) -> ! {
            use fstart_payload::{AcpiTables, Description, Handoff, PciRootInfo, Placement, Range};
            let acpi_buffer = Range::new(
                core::ptr::addr_of!(ACPI_BUFFER) as u64,
                QEMU_Q35_ACPI_BUFFER_SIZE as u64,
            );
            let tables = [acpi_buffer];
            let hb = self.config.q35.hostbridge;
            let mut description = Description::new(
                self.e820.entries(),
                Range::new(self.config.q35.firmware_base, self.config.q35.firmware_size),
            );
            description.console = Some(&self.console);
            description.uart = self.console.uart_port();
            description.pci_root = Some(PciRootInfo {
                ecam_base: hb.ecam_base,
                segment: 0,
                bus_start: hb.bus_start,
                bus_end: hb.bus_end,
            });
            description.framebuffer = self.framebuffer;
            description.acpi = self.acpi_rsdp.map(|rsdp| AcpiTables {
                rsdp,
                region: acpi_buffer,
            });
            let acpi_s5 = [fstart_core::description::AcpiSleepControl::new(
                crate::q35::Q35_PMBASE + 4,
                0,
            )];
            description.acpi_s5 = &acpi_s5;
            description.mainboard = ("QEMU", "Q35-AB");
            description.tables = &tables;

            #[cfg(feature = "payload-uefi")]
            {
                use fstart_payload::crabefi::FirmwareInfo;
                use fstart_payload::crabefi::capsule::{
                    CapsuleConfig, FstartCapsuleBackend, nvram_requests_file_capsule,
                };
                use fstart_payload::crabefi::settings::FlashStorage;

                let Some(layout) = self.handoff.slot.layout() else {
                    fstart_log::error!("qemu-q35-ab ramstage: invalid slot layout in handoff");
                    fstart_arch::x86::boot::halt();
                };
                let Some(running_slot) = self.handoff.slot.slot() else {
                    fstart_log::error!("qemu-q35-ab ramstage: invalid slot in handoff");
                    fstart_arch::x86::boot::halt();
                };
                let Some(trust) = TrustBlock::parse(&self.handoff.trust_bytes) else {
                    fstart_log::error!("qemu-q35-ab ramstage: invalid TrustBlock in handoff");
                    fstart_arch::x86::boot::halt();
                };
                if self.config.nvram_offset != 0 {
                    fstart_log::error!("qemu-q35-ab ramstage: NVRAM must start at offset 0");
                    fstart_arch::x86::boot::halt();
                }

                // SAFETY: writable RW pflash1 is mapped uncached below RO at flash_base.
                let pflash =
                    unsafe { Pflash::new(self.config.flash_base as usize, self.config.flash_size) };
                let Ok((mut nvram_region, rw_pflash)) =
                    pflash.split_low_sub_region(self.config.nvram_size)
                else {
                    fstart_log::error!("qemu-q35-ab ramstage: NVRAM sub-region split failed");
                    fstart_arch::x86::boot::halt();
                };
                let mut nvram_buf = alloc::vec![0u8; self.config.nvram_size as usize];
                if nvram_region.read(0, &mut nvram_buf).is_err() {
                    fstart_log::error!("qemu-q35-ab ramstage: failed to read NVRAM");
                    fstart_arch::x86::boot::halt();
                }
                let capsule_requested = nvram_requests_file_capsule(&nvram_buf);
                drop(nvram_buf);

                let mut varstore = FlashStorage::new(
                    nvram_region,
                    self.config.nvram_size,
                    PFLASH_SECTOR_SIZE,
                    true,
                );

                let fw_size = layout.extent(running_slot).map_or(0, |e| e.size);
                let mut capsule_backend = match FstartCapsuleBackend::new(
                    GuardedPflash { inner: rw_pflash },
                    || {
                        PFLASH_WRITE_LOCKED.store(true, Ordering::Release);
                        Ok(())
                    },
                    CapsuleConfig {
                        flash_capacity: self.config.flash_size,
                        layout,
                        running_slot,
                        running_fw_version: self.handoff.slot.fw_version.get(),
                        running_sec_version: self.handoff.slot.sec_version.get(),
                        running_root_digest: self.handoff.slot.root_digest,
                        effective_floor: self.handoff.slot.effective_floor.get(),
                        has_tpm_floor: self.handoff.tpm_present != 0,
                        trial_active: self.handoff.slot.trial_active != 0,
                        sync_pending: self.handoff.slot.sync_pending != 0,
                        image_family: trust.image_family,
                        root_keys: trust.valid_keys(),
                        firmware_info: FirmwareInfo {
                            guid: self.config.firmware_guid,
                            version: self.handoff.slot.fw_version.get(),
                            lowest_supported_version: self.config.lowest_supported_version,
                            fw_size,
                        },
                        trust_certs: self.config.capsule_trust_certs,
                    },
                ) {
                    Ok(b) => b,
                    Err(_) => {
                        fstart_log::error!("qemu-q35-ab ramstage: CapsuleBackend init failed");
                        fstart_arch::x86::boot::halt();
                    }
                };

                // Optional fault injection BEFORE trial confirmation (tests trial debit + fallback).
                if self.config.fail_trial_before_confirm && self.handoff.slot.trial_active != 0 {
                    fstart_log::warn!(
                        "qemu-q35-ab ramstage: simulating broken trial before confirmation -> cold reset"
                    );
                    fstart_arch::x86::boot::system_reset(true);
                }

                match capsule_backend.confirm_trial_if_active() {
                    Ok(true) => {
                        fstart_log::info!(
                            "qemu-q35-ab ramstage: trial confirmed (slot={} fw={} sec={})",
                            slot_name(running_slot),
                            self.handoff.slot.fw_version.get(),
                            self.handoff.slot.sec_version.get(),
                        );
                    }
                    Ok(false) => {}
                    Err(_) => {
                        fstart_log::error!("qemu-q35-ab ramstage: trial confirmation failed");
                        fstart_arch::x86::boot::halt();
                    }
                }

                // On normal boots without a pending Capsule-on-Disk, lock RW flash
                // immediately before entering CrabEFI / third-party EFI code.
                if !capsule_requested {
                    if capsule_backend.lock_flash().is_err() {
                        fstart_log::error!("qemu-q35-ab ramstage: failed to lock flash");
                        fstart_arch::x86::boot::halt();
                    }
                    fstart_log::info!("qemu-q35-ab ramstage: flash write-locked for normal boot");
                } else {
                    fstart_log::info!(
                        "qemu-q35-ab ramstage: OsIndications requests Capsule-on-Disk; deferring flash lock"
                    );
                }

                #[cfg(feature = "tpm")]
                self.ensure_tpm_platform_locked(true);

                fstart_log::info!("qemu-q35-ab {}", crate::messages::READY);
                let mut handoff = Handoff::new(description, Placement::default());
                handoff.variable_storage = Some(&mut varstore);
                handoff.capsule_backend = Some(&mut capsule_backend);
                fstart_payload::boot(handoff, ())
            }

            #[cfg(not(feature = "payload-uefi"))]
            {
                if self.config.fail_trial_before_confirm && self.handoff.slot.trial_active != 0 {
                    fstart_log::warn!(
                        "qemu-q35-ab ramstage: simulating broken trial before confirmation -> cold reset"
                    );
                    fstart_arch::x86::boot::system_reset(true);
                }
                #[cfg(feature = "tpm")]
                self.ensure_tpm_platform_locked(false);
                fstart_log::info!("qemu-q35-ab {}", crate::messages::READY);
                fstart_payload::boot(Handoff::new(description, Placement::default()), ())
            }
        }
    }

    /// Execute the QEMU Q35 A/B RW ramstage and launch the selected payload.
    pub fn run_q35_ab_ramstage(
        config: &'static Q35AbRamstageConfig,
        console_cfg: Ns16550Config,
    ) -> ! {
        let Ok(mut mainstage) = QemuQ35AbRamstage::new(config, console_cfg) else {
            fstart_arch::x86::boot::halt();
        };
        use fstart_stage::step;
        step("console", || mainstage.init_console());
        step("memory_detect", || mainstage.detect_memory_and_tables());
        #[cfg(feature = "tpm")]
        step("tpm", || mainstage.init_tpm());
        step("bus_scan", || mainstage.init_pci());
        step("display", || mainstage.init_display());
        step("page_tables", || mainstage.map_memory());
        step("mp_smm", || mainstage.init_mp_smm());
        mainstage.hand_off()
    }
}

#[cfg(all(feature = "q35-ab-ramstage", target_arch = "x86_64"))]
pub use ramstage::run_q35_ab_ramstage;
