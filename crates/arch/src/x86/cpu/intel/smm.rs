//! Post-MP SMM installation for Intel-style chipsets (and QEMU q35).
//!
//! The flow is written once and composed from three owners:
//! - the northbridge's [`SmramControl`] (TSEG geometry, open/close/lock),
//! - the southbridge's [`SmiControl`] (SMI sources and handler configuration),
//! - the CPU model driver's [`SmmCpu`] (save-state layout and SMRR pair).
//!
//! No chipset, board, or CPU-model decision is made here.

use core::sync::atomic::{AtomicU16, AtomicU32, AtomicU64, Ordering};

use crate::x86::mp::{
    MAX_CPUS, MpHandle, SMM_DEFAULT_ASEG, SMM_DEFAULT_ENTRY_STACK_TOP, SMM_DEFAULT_SMBASE,
};

use super::smrr::SmrrRange;

pub use super::smrr::SmrrPair;
pub use fstart_smm::X86SaveStateFormat;

const SAVE_STATE_SIZE: u32 = 0x400;
// Per-CPU failures are collected in a `u64` bitmap.
const _: () = assert!(MAX_CPUS <= 64);
const RELOCATION_LOCK_SPINS: u32 = 50_000_000;
const RELOCATION_DONE_SPINS: u32 = 200_000_000;

/// Northbridge SMRAM control.
pub trait SmramControl {
    /// Permanent SMRAM (TSEG) base and size, or `None` when disabled.
    fn tseg(&self) -> Option<(u64, u32)>;
    /// Make SMRAM accessible from normal mode for installation.
    fn smram_open(&self);
    /// Hide SMRAM from normal mode.
    fn smram_close(&self);
    /// Lock the SMRAM configuration until the next reset.
    fn smram_lock(&self);
}

/// Southbridge SMI control.
pub trait SmiControl {
    /// Chipset-owned source-enable snapshot restored after relocation.
    type EnableState: Copy;
    /// Configuration consumed by the southbridge's permanent SMM handler; the
    /// same type as that handler's [`fstart_smm::SmmHandler::Config`].
    type HandlerConfig: Copy;

    /// Configuration written into SMRAM for the permanent handler.
    fn smm_handler_config(&self) -> Self::HandlerConfig;
    /// Set (`true`) or clear the chipset's ACPI mode (`SCI_EN`).
    fn set_acpi_mode(&self, enabled: bool);
    /// Mask every chipset SMI source while the shared relocation stub is live
    /// and return the enables to restore.
    fn quiesce_for_relocation(&self) -> Self::EnableState;
    /// Enable the permanent SMI sources.
    fn enable_permanent_smi(&self, previous: Self::EnableState);
}

/// CPU-model facts needed to install SMM, supplied by the CPU model driver.
pub trait SmmCpu {
    /// State-save layout this CPU writes on SMI entry.
    fn smm_save_state_format(&self) -> X86SaveStateFormat;
    /// SMRR register pair of this CPU model, or `None` when it has none.
    fn smrr_pair(&self) -> Option<SmrrPair>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntelSmmError {
    TsegDisabled,
    TooManyCpus,
    InstallFailed,
    RelocationLockTimeout,
    RelocationTimeout,
    UnexpectedCpu,
    SaveStateMismatch,
    SmrrSetupFailed,
}

impl IntelSmmError {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::TsegDisabled => "TsegDisabled",
            Self::TooManyCpus => "TooManyCpus",
            Self::InstallFailed => "InstallFailed",
            Self::RelocationLockTimeout => "RelocationLockTimeout",
            Self::RelocationTimeout => "RelocationTimeout",
            Self::UnexpectedCpu => "UnexpectedCpu",
            Self::SaveStateMismatch => "SaveStateMismatch",
            Self::SmrrSetupFailed => "SmrrSetupFailed",
        }
    }
}

const ZERO_CPU_LAYOUT: fstart_smm::CpuSmmLayout = fstart_smm::CpuSmmLayout {
    smbase: 0,
    entry_addr: 0,
    save_state_base: 0,
    save_state_top: 0,
    stack_bottom: 0,
    stack_top: 0,
};

/// One SMM installation composed from its three owners.
pub struct IntelSmm<'a, NB: SmramControl, SB: SmiControl, C: SmmCpu> {
    name: &'static str,
    smram: &'a NB,
    smi: &'a SB,
    cpu: &'a C,
    resume: bool,
}

impl<'a, NB: SmramControl, SB: SmiControl, C: SmmCpu> IntelSmm<'a, NB, SB, C> {
    pub const fn new(
        name: &'static str,
        smram: &'a NB,
        smi: &'a SB,
        cpu: &'a C,
        resume: bool,
    ) -> Self {
        Self {
            name,
            smram,
            smi,
            cpu,
            resume,
        }
    }

    /// Install, relocate and lock the permanent handler using the
    /// already-initialized MP subsystem.
    pub fn install(&self, mp: &MpHandle, image: &[u8]) -> Result<(), IntelSmmError> {
        let (smram_base, smram_size) = self
            .smram
            .tseg()
            .filter(|(_, size)| *size != 0)
            .ok_or(IntelSmmError::TsegDisabled)?;
        let num_cpus = mp.num_cpus();
        if num_cpus as usize > MAX_CPUS {
            return Err(IntelSmmError::TooManyCpus);
        }
        fstart_log::info!(
            "{} SMM: TSEG base={:#x} size={:#x}, online CPUs={}",
            self.name,
            smram_base,
            smram_size,
            num_cpus
        );

        let mut layouts = [ZERO_CPU_LAYOUT; MAX_CPUS];
        self.smram.smram_open();
        let result = self.install_open(mp, image, smram_base, smram_size, num_cpus, &mut layouts);
        if result.is_err() {
            self.smram.smram_close();
        }
        result?;

        // Exercise every CPU's permanent entry once, as a boot-time smoke
        // test: a broken stub or handler stops the boot here instead of at the
        // first OS SMI. Nothing outside SMRAM can observe more than the return.
        mp.scope(|scope| scope.scatter(&|_| send_smi_self()));
        fstart_log::info!(
            "{} SMM: permanent SMI returned on all {} CPUs",
            self.name,
            num_cpus
        );
        Ok(())
    }

    fn install_open(
        &self,
        mp: &MpHandle,
        image: &[u8],
        smram_base: u64,
        smram_size: u32,
        num_cpus: u16,
        layouts: &mut [fstart_smm::CpuSmmLayout; MAX_CPUS],
    ) -> Result<(), IntelSmmError> {
        let format = self.cpu.smm_save_state_format();
        let handler_config = self.smi.smm_handler_config();
        let installed = unsafe {
            fstart_smm::install_pic_image(
                image,
                fstart_smm::InstallConfig {
                    smram_base,
                    smram_size: u64::from(smram_size),
                    num_cpus,
                    save_state_size: SAVE_STATE_SIZE,
                    handler_config: &handler_config,
                },
                layouts,
            )
        }
        .map_err(|_| IntelSmmError::InstallFailed)?;

        // Like coreboot, a TSEG that SMRR cannot describe only disables SMRR.
        let smrr = self.cpu.smrr_pair().and_then(|pair| {
            SmrrRange::new(smram_base, smram_size)
                .map(|range| (pair, range))
                .map_err(|_| {
                    fstart_log::warn!(
                        "{} SMM: TSEG is not a naturally aligned power of two; SMRR disabled",
                        self.name
                    )
                })
                .ok()
        });
        RELOCATION_BRIDGE.prepare(format, smrr.map(|(_, range)| range))?;
        let relocation = &RELOCATION_BRIDGE;
        let relocation_cr3 =
            unsafe { fstart_smm::build_relocation_identity_tables(SMM_DEFAULT_SMBASE) };

        // Disable every chipset event source before publishing the shared
        // default-SMBASE callback. Relocation itself uses LAPIC self-SMIs.
        // Any later installation failure is boot-fatal and deliberately
        // leaves the sources quiesced.
        let previous_smi_enables = self.smi.quiesce_for_relocation();
        unsafe {
            fstart_smm::install_default_relocation_callback_stub(
                image,
                fstart_smm::DefaultRelocationCallbackConfig {
                    default_smbase: SMM_DEFAULT_SMBASE,
                    cr3: relocation_cr3,
                    callback: default_smm_relocation_handler as *const () as usize as u64,
                    callback_arg: core::ptr::from_ref(relocation) as u64,
                    stack_top: SMM_DEFAULT_ENTRY_STACK_TOP,
                },
            )
        }
        .map_err(|_| IntelSmmError::InstallFailed)?;

        unsafe {
            crate::x86::writeback_cache_range(smram_base as *const u8, smram_size as usize);
            let (base, end) = SMM_DEFAULT_ASEG;
            crate::x86::writeback_cache_range(base as *const u8, (end - base) as usize);
        }
        fstart_log::info!(
            "{} SMM: handler={:#x} CR3={:#x} format={}",
            self.name,
            installed.common_entry,
            installed.cr3,
            format.name()
        );

        let failures = AtomicU64::new(0);
        let smrr_programmed = AtomicU32::new(0);
        mp.scope(|scope| {
            scope.scatter(&|cpu| {
                let target = installed.cpus[cpu as usize].smbase;
                let pair = smrr
                    .map(|(pair, _)| pair)
                    .filter(|pair| pair.usable_on_current_cpu());
                match relocate_one(relocation, target, pair) {
                    Ok(()) if pair.is_some() => {
                        smrr_programmed.fetch_add(1, Ordering::AcqRel);
                    }
                    Ok(()) => {}
                    Err(error) => {
                        failures.fetch_or(1u64 << cpu, Ordering::AcqRel);
                        let _ = relocation.last_error.compare_exchange(
                            0,
                            error as u32 + 1,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        );
                    }
                }
            });
        });
        if failures.load(Ordering::Acquire) != 0 {
            return Err(error_from_raw(
                relocation.last_error.load(Ordering::Acquire),
            ));
        }

        fstart_log::info!(
            "{} SMM: selected {} revision {:#010x}; {} CPUs relocated",
            self.name,
            format.name(),
            relocation.revision.load(Ordering::Acquire),
            relocation.done.load(Ordering::Acquire)
        );
        let programmed = smrr_programmed.load(Ordering::Acquire);
        if smrr.is_some() && programmed != u32::from(num_cpus) {
            fstart_log::warn!(
                "{} SMM: SMRR programmed on {}/{} CPUs (IA32_FEATURE_CONTROL locked without SMRR?)",
                self.name,
                programmed,
                num_cpus
            );
        }

        // Resume must preserve the OS's ACPI mode; a cold boot hands the
        // payload the legacy-mode state.
        self.smi.set_acpi_mode(self.resume);
        self.smram.smram_close();
        self.smi.enable_permanent_smi(previous_smi_enables);
        self.smram.smram_lock();
        fstart_log::info!("{} SMM: permanent SMI enabled and SMRAM locked", self.name);
        Ok(())
    }
}

/// Persistent bridge between ramstage and the default-SMBASE callback.
///
/// A late callback after a timeout must never observe a dead stack object. The
/// bridge therefore lives for the firmware image's lifetime. Once an SMI has
/// timed out, its serialization lock deliberately remains held so no later CPU
/// can publish a different target underneath that callback.
const RELOCATION_UNLOCKED: u32 = 0;
const RELOCATION_LOCKED: u32 = 1;
const RELOCATION_POISONED: u32 = 2;

const RESULT_OK: u32 = 1;
const RESULT_SAVE_STATE: u32 = 2;
const RESULT_UNEXPECTED_CPU: u32 = 3;
const RESULT_SMRR: u32 = 4;

struct RelocationState {
    phase: AtomicU32,
    format: AtomicU16,
    target_smbase: AtomicU64,
    expected_lapic_id: AtomicU32,
    /// [`SmrrPair`] to program on the CPU currently relocating, or 0.
    smrr_pair: AtomicU32,
    smrr_base: AtomicU64,
    smrr_mask: AtomicU64,
    done: AtomicU32,
    revision: AtomicU32,
    result: AtomicU32,
    /// First failure, as `IntelSmmError as u32 + 1`; 0 when none.
    last_error: AtomicU32,
}

impl RelocationState {
    const fn new() -> Self {
        Self {
            phase: AtomicU32::new(RELOCATION_UNLOCKED),
            format: AtomicU16::new(0),
            target_smbase: AtomicU64::new(0),
            expected_lapic_id: AtomicU32::new(u32::MAX),
            smrr_pair: AtomicU32::new(0),
            smrr_base: AtomicU64::new(0),
            smrr_mask: AtomicU64::new(0),
            done: AtomicU32::new(0),
            revision: AtomicU32::new(0),
            result: AtomicU32::new(0),
            last_error: AtomicU32::new(0),
        }
    }

    fn prepare(
        &self,
        format: X86SaveStateFormat,
        smrr: Option<SmrrRange>,
    ) -> Result<(), IntelSmmError> {
        if self.phase.load(Ordering::Acquire) != RELOCATION_UNLOCKED {
            return Err(IntelSmmError::RelocationTimeout);
        }
        self.format.store(format as u16, Ordering::Release);
        self.target_smbase.store(0, Ordering::Release);
        self.expected_lapic_id.store(u32::MAX, Ordering::Release);
        self.smrr_pair.store(0, Ordering::Release);
        self.smrr_base
            .store(smrr.map_or(0, SmrrRange::base), Ordering::Release);
        self.smrr_mask
            .store(smrr.map_or(0, SmrrRange::mask), Ordering::Release);
        self.done.store(0, Ordering::Release);
        self.revision.store(0, Ordering::Release);
        self.result.store(0, Ordering::Release);
        self.last_error.store(0, Ordering::Release);
        Ok(())
    }
}

static RELOCATION_BRIDGE: RelocationState = RelocationState::new();

fn format_from_raw(raw: u16) -> Option<X86SaveStateFormat> {
    [X86SaveStateFormat::IntelEm64t, X86SaveStateFormat::Amd64]
        .into_iter()
        .find(|format| *format as u16 == raw)
}

fn error_from_raw(raw: u32) -> IntelSmmError {
    [
        IntelSmmError::RelocationLockTimeout,
        IntelSmmError::RelocationTimeout,
        IntelSmmError::UnexpectedCpu,
        IntelSmmError::SmrrSetupFailed,
    ]
    .into_iter()
    .find(|error| *error as u32 + 1 == raw)
    .unwrap_or(IntelSmmError::SaveStateMismatch)
}

fn relocate_one(
    state: &RelocationState,
    target_smbase: u64,
    smrr: Option<SmrrPair>,
) -> Result<(), IntelSmmError> {
    let mut spins = 0;
    while state
        .phase
        .compare_exchange(
            RELOCATION_UNLOCKED,
            RELOCATION_LOCKED,
            Ordering::Acquire,
            Ordering::Relaxed,
        )
        .is_err()
    {
        core::hint::spin_loop();
        spins += 1;
        if spins == RELOCATION_LOCK_SPINS {
            state.phase.store(RELOCATION_POISONED, Ordering::Release);
            return Err(IntelSmmError::RelocationLockTimeout);
        }
    }
    let lapic_id = crate::x86::lapic::Lapic::from_msr().id();
    state.expected_lapic_id.store(lapic_id, Ordering::Release);
    state.target_smbase.store(target_smbase, Ordering::Release);
    state
        .smrr_pair
        .store(SmrrPair::to_raw(smrr), Ordering::Release);
    state.result.store(0, Ordering::Release);
    let before = state.done.load(Ordering::Acquire);
    send_smi_self();
    let mut spins = 0;
    while state.done.load(Ordering::Acquire) == before {
        core::hint::spin_loop();
        spins += 1;
        if spins == RELOCATION_DONE_SPINS {
            // The callback may still be in flight and consumes the currently
            // published target when it eventually arrives.
            state.phase.store(RELOCATION_POISONED, Ordering::Release);
            return Err(IntelSmmError::RelocationTimeout);
        }
    }
    finish_relocation(state, state.result.load(Ordering::Acquire))
}

fn finish_relocation(state: &RelocationState, result: u32) -> Result<(), IntelSmmError> {
    if result == RESULT_OK {
        state
            .phase
            .compare_exchange(
                RELOCATION_LOCKED,
                RELOCATION_UNLOCKED,
                Ordering::Release,
                Ordering::Acquire,
            )
            .map(|_| ())
            .map_err(|_| IntelSmmError::RelocationLockTimeout)
    } else {
        // Fail closed: an unexpected entrant or revision mismatch may leave a
        // late callback in flight. Never publish another target afterward.
        state.phase.store(RELOCATION_POISONED, Ordering::Release);
        Err(match result {
            RESULT_UNEXPECTED_CPU => IntelSmmError::UnexpectedCpu,
            RESULT_SMRR => IntelSmmError::SmrrSetupFailed,
            _ => IntelSmmError::SaveStateMismatch,
        })
    }
}

fn send_smi_self() {
    let lapic = crate::x86::lapic::Lapic::from_msr();
    lapic.send_smi_self();
    lapic.wait_ready();
}

/// Temporary default-SMBASE callback. The serialized caller publishes the one
/// target, SMRR pair, and full LAPIC ID for the CPU currently in the shared
/// relocation window.
///
/// # Safety
///
/// `params` must point to the live default-SMBASE entry parameters installed
/// by `install_default_relocation_callback_stub`.
pub unsafe extern "C" fn default_smm_relocation_handler(params: *mut fstart_smm::SmmEntryParams) {
    if params.is_null() {
        return;
    }
    let state_addr = unsafe { core::ptr::read_unaligned(core::ptr::addr_of!((*params).runtime)) };
    if state_addr == 0 {
        return;
    }
    let state = unsafe { &*(state_addr as *const RelocationState) };
    let finish = |result: u32| {
        state.result.store(result, Ordering::Release);
        state.done.fetch_add(1, Ordering::AcqRel);
    };
    let Some(format) = format_from_raw(state.format.load(Ordering::Acquire)) else {
        return finish(RESULT_SAVE_STATE);
    };
    if crate::x86::lapic::Lapic::from_msr().id() != state.expected_lapic_id.load(Ordering::Acquire)
    {
        return finish(RESULT_UNEXPECTED_CPU);
    }
    let top = (SMM_DEFAULT_SMBASE + 0x1_0000) as *mut u8;
    let save_state = unsafe { fstart_smm::X86SaveState::from_top(top, format) };
    let target = state.target_smbase.load(Ordering::Acquire) as u32;
    let Ok(revision) = save_state.revision() else {
        return finish(RESULT_SAVE_STATE);
    };
    state.revision.store(revision, Ordering::Release);
    if save_state.write_smbase(target).is_err() {
        return finish(RESULT_SAVE_STATE);
    }
    if let Some(pair) = SmrrPair::from_raw(state.smrr_pair.load(Ordering::Acquire)) {
        let range = SmrrRange::from_raw(
            state.smrr_base.load(Ordering::Acquire),
            state.smrr_mask.load(Ordering::Acquire),
        );
        // SAFETY: the caller checked `usable_on_current_cpu` for this pair
        // and we are in SMM.
        if unsafe { pair.program(range) }.is_err() {
            return finish(RESULT_SMRR);
        }
    }
    finish(RESULT_OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiter_timeout_poison_prevents_owner_unlock() {
        let state = RelocationState::new();
        state.phase.store(RELOCATION_LOCKED, Ordering::Release);

        // A waiter times out while the owner is still handling its callback.
        state.phase.store(RELOCATION_POISONED, Ordering::Release);

        assert_eq!(
            finish_relocation(&state, RESULT_OK),
            Err(IntelSmmError::RelocationLockTimeout)
        );
        assert_eq!(state.phase.load(Ordering::Acquire), RELOCATION_POISONED);
    }

    #[test]
    fn error_encoding_round_trips() {
        for error in [
            IntelSmmError::RelocationLockTimeout,
            IntelSmmError::RelocationTimeout,
            IntelSmmError::UnexpectedCpu,
            IntelSmmError::SmrrSetupFailed,
            IntelSmmError::SaveStateMismatch,
        ] {
            assert_eq!(error_from_raw(error as u32 + 1), error);
        }
    }
}
