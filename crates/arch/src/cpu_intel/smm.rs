//! Intel gen1 (Core 2 / Atom, EM64T101 save state) SMM installation and
//! SMBASE relocation, composed from chipset-owned pieces.
//!
//! coreboot splits this work across `cpu/intel/smm/gen1/smmrelocate.c`
//! (CPU), the northbridge `smm_region`/`northbridge_write_smram` helpers and
//! `southbridge/intel/common/smi.c`. [`IntelSmm`] is the CPU-side flow; the
//! northbridge contributes [`SmramControl`] (TSEG geometry and the SMRAM
//! open/close/lock register) and the southbridge contributes [`SmiControl`]
//! (SMI enables and status clearing). Each chipset implements only its own
//! trait, so fixes to the relocation sequence reach every platform at once.

use crate::mp::{SmmError, SmmInfo, SmmOps};
use core::cell::UnsafeCell;

/// APM command port; writing the ACPI-disable command after the permanent
/// handler is installed matches coreboot's `*_set_acpi_mode()` on a normal boot.
const APM_CNT: u16 = 0x00b2;
const APM_CNT_ACPI_DISABLE: u8 = 0x1e;
/// EM64T101 save-state area size (QEMU's AMD64 save state is the same size).
const SAVE_STATE_SIZE: usize = 0x400;

/// Northbridge SMRAM window control.
pub trait SmramControl {
    /// TSEG `(base, size)`; `None` when TSEG is disabled.
    fn tseg(&self) -> Option<(u64, u32)>;
    /// Make SMRAM CPU-accessible outside SMM (`D_OPEN`).
    fn smram_open(&self);
    /// Hide SMRAM outside SMM again.
    fn smram_close(&self);
    /// Lock the SMRAM register until reset (`D_LCK`).
    fn smram_lock(&self);
}

/// Southbridge SMI routing for the relocation and permanent phases.
pub trait SmiControl {
    /// ACPI PM I/O base, published to the SMM runtime.
    fn pm_base(&self) -> u16;
    /// GPE0 status block as the SMM runtime expects it.
    fn gpe0(&self) -> Gpe0Block;
    /// Clear stale status and enable only what the relocation SMI needs
    /// (APMC + global SMI).
    fn enable_relocation_smi(&self);
    /// Clear all stale PM/SMI/TCO/GPE status and enable the permanent SMI
    /// sources (coreboot `smm_southbridge_clear_state()` then
    /// `global_smi_enable()`).
    fn enable_permanent_smi(&self);
}

/// GPE0 status register geometry, which differs between ICH generations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gpe0Block {
    /// Offset of `GPE0_STS` from PMBASE.
    pub sts_offset: u16,
    /// Whether the block is 64 bits wide (ICH8+) instead of 32 (ICH7).
    pub wide: bool,
}

impl Gpe0Block {
    /// ICH7/NM10: one 32-bit status word at 0x28.
    pub const ICH7: Self = Self {
        sts_offset: 0x28,
        wide: false,
    };
    /// ICH8 through ICH10: 64-bit status at 0x20.
    pub const ICH8: Self = Self {
        sts_offset: 0x20,
        wide: true,
    };
}

const ZERO_CPU_LAYOUT: fstart_smm::CpuSmmLayout = fstart_smm::CpuSmmLayout {
    smbase: 0,
    entry_addr: 0,
    save_state_base: 0,
    save_state_top: 0,
    stack_bottom: 0,
    stack_top: 0,
};

struct CpuLayoutStore(UnsafeCell<[fstart_smm::CpuSmmLayout; fstart_smm::runtime::MAX_SMM_CPUS]>);

// SAFETY: the installer runs once on the BSP while SMRAM is open, before any
// AP touches SMM state; this scratch buffer is never shared.
unsafe impl Sync for CpuLayoutStore {}

static CPU_LAYOUTS: CpuLayoutStore = CpuLayoutStore(UnsafeCell::new(
    [ZERO_CPU_LAYOUT; fstart_smm::runtime::MAX_SMM_CPUS],
));

/// [`SmmOps`] for an Intel gen1 chipset pair.
pub struct IntelSmm<'a, NB: SmramControl, SB: SmiControl> {
    name: &'static str,
    smram: &'a NB,
    smi: &'a SB,
}

impl<'a, NB: SmramControl, SB: SmiControl> IntelSmm<'a, NB, SB> {
    /// Compose the flow from a northbridge and a southbridge; `name` prefixes
    /// log lines.
    pub const fn new(name: &'static str, smram: &'a NB, smi: &'a SB) -> Self {
        Self { name, smram, smi }
    }
}

// SAFETY: the flow only borrows chipset drivers whose register accesses are
// CPU-exclusive in firmware, and the MP code serialises every callback.
unsafe impl<NB: SmramControl, SB: SmiControl> Send for IntelSmm<'_, NB, SB> {}
unsafe impl<NB: SmramControl, SB: SmiControl> Sync for IntelSmm<'_, NB, SB> {}

impl<NB: SmramControl, SB: SmiControl> SmmOps for IntelSmm<'_, NB, SB> {
    fn smm_info(&self) -> Option<SmmInfo> {
        let Some((base, size)) = self.smram.tseg().filter(|(_, size)| *size != 0) else {
            fstart_log::error!("{} SMM: TSEG is disabled", self.name);
            return None;
        };
        fstart_log::info!("{} SMM: TSEG base={:#x} size={:#x}", self.name, base, size);
        Some(SmmInfo {
            smbase: base,
            smsize: size as usize,
            save_state_size: SAVE_STATE_SIZE,
        })
    }

    fn install_smm_handlers(
        &self,
        info: &SmmInfo,
        num_cpus: u16,
        image: &[u8],
    ) -> Result<(), SmmError> {
        let gpe0 = self.smi.gpe0();
        self.smram.smram_open();
        let result = self.install(info, num_cpus, image, gpe0);
        if result.is_err() {
            self.smram.smram_close();
        }
        result
    }

    fn smm_relocate(&self) {
        // coreboot `smm_initiate_relocation`: a self SMI through the local
        // APIC. It must stay per-CPU because every CPU shares the
        // architectural default SMBASE (and one save state) until it has
        // relocated itself.
        let lapic = crate::lapic::Lapic::from_msr();
        lapic.send_smi_self();
        lapic.wait_ready();
    }

    fn pre_smm_init(&self) {
        // The permanent handler is not live yet: enable only the relocation
        // SMI; the full status cleanup happens in `post_smm_init`.
        self.smi.enable_relocation_smi();
    }

    fn post_smm_init(&self) {
        self.smram.smram_close();
        self.smi.enable_permanent_smi();
        // Have the freshly installed handler clear PM1_CNT.SCI_EN and the
        // stale PM/GPE/TCO status. The FADT advertises the ACPI-enable
        // command, so the OS re-enables SCI only once ACPICA owns it.
        // SAFETY: APM_CNT is the architectural APM command port.
        unsafe { fstart_core::pio::outb(APM_CNT, APM_CNT_ACPI_DISABLE) };
        self.smram.smram_lock();
        fstart_log::info!("{} SMM: permanent SMI enabled and SMRAM locked", self.name);
    }
}

impl<NB: SmramControl, SB: SmiControl> IntelSmm<'_, NB, SB> {
    fn install(
        &self,
        info: &SmmInfo,
        num_cpus: u16,
        image: &[u8],
        gpe0: Gpe0Block,
    ) -> Result<(), SmmError> {
        // SAFETY: BSP-only, SMRAM open, no concurrent user of the scratch.
        let layouts = unsafe { &mut *CPU_LAYOUTS.0.get() };
        // SAFETY: SMRAM is open and `info` describes the TSEG window.
        let installed = unsafe {
            fstart_smm::install_pic_image(
                image,
                fstart_smm::InstallConfig {
                    smram_base: info.smbase,
                    smram_size: info.smsize as u64,
                    num_cpus,
                    save_state_size: info.save_state_size as u32,
                    page_table_size: 0,
                    cr3: crate::x86::controlregs::cr3(),
                    platform_kind: fstart_smm::SMM_PLATFORM_INTEL_ICH,
                    platform_flags: if gpe0.wide {
                        fstart_smm::SMM_PLATFORM_FLAG_ICH_GPE0_64BIT
                    } else {
                        0
                    },
                    platform_data: [
                        u64::from(self.smi.pm_base()),
                        u64::from(gpe0.sts_offset),
                        0,
                        0,
                    ],
                },
                layouts,
            )
        }
        .map_err(|_| {
            fstart_log::error!("{} SMM: failed to install SMM image", self.name);
            SmmError::InstallFailed
        })?;

        let targets = &installed.cpus[..num_cpus as usize];
        crate::mp::prepare_default_smm_relocation(targets);
        // The relocation stub enters long mode from the default SMBASE and
        // cannot rely on the interrupted context's CR3, so it gets its own
        // identity map next to the stub.
        // SAFETY: the default SMBASE region is writable low memory and unused
        // until the stub itself is installed there.
        let relocation_cr3 =
            unsafe { fstart_smm::build_relocation_identity_tables(crate::mp::SMM_DEFAULT_SMBASE) };
        // SAFETY: same region; the callback is a plain `extern "C"` firmware fn.
        unsafe {
            fstart_smm::install_default_relocation_callback_stub(
                image,
                fstart_smm::DefaultRelocationCallbackConfig {
                    default_smbase: crate::mp::SMM_DEFAULT_SMBASE,
                    cr3: relocation_cr3,
                    callback: crate::mp::default_smm_relocation_handler as *const () as usize
                        as u64,
                    stack_top: crate::mp::SMM_DEFAULT_ENTRY_STACK_TOP,
                },
            )
        }
        .map_err(|_| {
            fstart_log::error!(
                "{} SMM: failed to install default relocation handler",
                self.name
            );
            SmmError::InstallFailed
        })?;

        fstart_log::info!(
            "{} SMM: installed image common={:#x} entry={:#x} cpus={}",
            self.name,
            installed.common_base,
            installed.common_entry,
            installed.cpus.len()
        );
        Ok(())
    }
}
