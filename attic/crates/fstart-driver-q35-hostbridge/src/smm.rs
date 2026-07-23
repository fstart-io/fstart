//! Unported Q35 TSEG/SMI sequence.
//!
//! This is archival source, not a crate. `crates/platform-qemu/src/q35.rs`
//! owns the live Q35 host-bridge flow; port these blocks into that module only
//! when `qemu-q35` enables SMM. The legacy API names below are retained as the
//! hardware-sequence reference.

use core::cell::UnsafeCell;

use fstart_mp::{SmmError, SmmInfo, SmmOps};
use fstart_services::pci::PciBdf;

// Q35 / ICH9 SMM registers.  Matches coreboot's
// `mainboard/emulation/qemu-q35/q35.h` and `i82801ix.h`.
const EXT_TSEG_MBYTES: u8 = 0x50;
const SMRAMC: u8 = 0x9d;
const G_SMRAME: u8 = 1 << 3;
const D_LCK: u8 = 1 << 4;
const D_OPEN: u8 = 1 << 6;
const C_BASE_SEG: u8 = 0b010;
const ESMRAMC: u8 = 0x9e;
const T_EN: u8 = 1 << 0;
const TSEG_SZ_MASK: u8 = 3 << 1;

const Q35_PMBASE: u16 = 0x0600;
const APM_CNT: u16 = 0x00b2;
const APM_CNT_SMI: u8 = 0xef;
const AMD64_SAVE_STATE_SIZE: usize = 0x400;

const ZERO_CPU_LAYOUT: fstart_smm::CpuSmmLayout = fstart_smm::CpuSmmLayout {
    smbase: 0,
    entry_addr: 0,
    save_state_base: 0,
    save_state_top: 0,
    stack_bottom: 0,
    stack_top: 0,
};

struct CpuLayoutStore(UnsafeCell<[fstart_smm::CpuSmmLayout; fstart_smm::runtime::MAX_SMM_CPUS]>);

// SAFETY: firmware runs the SMM installer from the BSP while SMRAM is open;
// no other Rust code accesses this scratch buffer concurrently.
unsafe impl Sync for CpuLayoutStore {}

static Q35_SMM_CPU_LAYOUTS: CpuLayoutStore = CpuLayoutStore(UnsafeCell::new(
    [ZERO_CPU_LAYOUT; fstart_smm::runtime::MAX_SMM_CPUS],
));

impl Q35HostBridge {
    fn pci_read8(bus: u8, dev: u8, func: u8, reg: u8) -> u8 {
        let aligned = reg & !3;
        let shift = ((reg & 3) as u32) * 8;
        // SAFETY: caller selects a valid PCI config register for this chipset.
        ((unsafe { fstart_pio::pci_cfg_read32(bus, dev, func, aligned) } >> shift) & 0xff) as u8
    }

    fn pci_write8(bus: u8, dev: u8, func: u8, reg: u8, val: u8) {
        let aligned = reg & !3;
        let shift = ((reg & 3) as u32) * 8;
        // SAFETY: caller selects a valid PCI config register for this chipset.
        let old = unsafe { fstart_pio::pci_cfg_read32(bus, dev, func, aligned) };
        let mask = !(0xffu32 << shift);
        let new = (old & mask) | ((val as u32) << shift);
        unsafe { fstart_pio::pci_cfg_write32(bus, dev, func, aligned, new) };
    }

    fn pci_read_host8(reg: u8) -> u8 {
        Self::pci_read8(0, 0, 0, reg)
    }

    fn pci_write_host8(reg: u8, val: u8) {
        Self::pci_write8(0, 0, 0, reg, val);
    }

    fn decode_tseg_size(&self) -> usize {
        let mut esmramc = Self::pci_read_host8(ESMRAMC);
        // fstart's Q35 path always uses TSEG for permanent SMRAM.  If QEMU
        // has not yet reflected T_EN, decode the configured size anyway and
        // enable TSEG in `smm_close()` after installation.
        esmramc |= T_EN;
        match (esmramc & TSEG_SZ_MASK) >> 1 {
            0 => 1 << 20,
            1 => 2 << 20,
            2 => 8 << 20,
            _ => {
                let mb = self
                    .ecam
                    .config_read16(PciBdf::new(0, 0, 0), EXT_TSEG_MBYTES as u16)
                    .unwrap_or(8);
                (mb as usize) << 20
            }
        }
    }

    fn tseg_base_from_e820(&self, size: usize) -> u64 {
        let state = unsafe { fstart_services::memory_detect::e820_state() };
        let entries = state.entries();
        let size_u64 = size as u64;

        // Prefer an explicit reserved E820 region of the right size at the
        // top of low memory.  QEMU/coreboot often report TSEG this way.
        for entry in entries {
            if entry.kind != 1 && entry.size == size_u64 && entry.addr < 0x1_0000_0000 {
                return entry.addr;
            }
        }

        // Fallback: compute from the top of usable RAM below 4 GiB.
        let (tolud, _) = Self::ram_tops_from_e820(entries);
        tolud.saturating_sub(size_u64)
    }

    fn smm_open(&self) {
        // Open ASEG-compatible SMRAM access and disable TSEG hiding while the
        // BSP copies the permanent image.  Mirrors coreboot q35 `smm_open()`.
        Self::pci_write_host8(SMRAMC, D_OPEN | G_SMRAME | C_BASE_SEG);
        let esmramc = Self::pci_read_host8(ESMRAMC);
        Self::pci_write_host8(ESMRAMC, esmramc & !T_EN);
    }

    fn smm_close(&self) {
        Self::pci_write_host8(SMRAMC, G_SMRAME | C_BASE_SEG);
        let esmramc = Self::pci_read_host8(ESMRAMC);
        Self::pci_write_host8(ESMRAMC, esmramc | T_EN);
    }

    fn smm_lock(&self) {
        Self::pci_write_host8(SMRAMC, D_LCK | G_SMRAME | C_BASE_SEG);
    }
}

impl SmmOps for Q35HostBridge {
    fn smm_info(&self) -> Option<SmmInfo> {
        let size = self.decode_tseg_size();
        if size == 0 {
            return None;
        }
        let base = self.tseg_base_from_e820(size);
        fstart_log::info!("Q35 SMM: TSEG base={:#x} size={:#x}", base, size);
        Some(SmmInfo {
            smbase: base,
            smsize: size,
            save_state_size: AMD64_SAVE_STATE_SIZE,
        })
    }

    fn install_smm_handlers(
        &self,
        info: &SmmInfo,
        num_cpus: u16,
        image: &[u8],
    ) -> Result<(), SmmError> {
        self.smm_open();

        let layouts = unsafe { &mut *Q35_SMM_CPU_LAYOUTS.0.get() };
        let result = unsafe {
            fstart_smm::install_pic_image(
                image,
                fstart_smm::InstallConfig {
                    smram_base: info.smbase,
                    smram_size: info.smsize as u64,
                    num_cpus,
                    save_state_size: info.save_state_size as u32,
                    page_table_size: 0,
                    cr3: fstart_arch_x86::x86::controlregs::cr3(),
                    platform_kind: fstart_smm::SMM_PLATFORM_INTEL_ICH,
                    platform_flags: fstart_smm::SMM_PLATFORM_FLAG_ICH_GPE0_64BIT,
                    platform_data: [Q35_PMBASE as u64, 0x20, 0, 0],
                },
                layouts,
            )
        };

        match result {
            Ok(installed) => {
                let targets = &installed.cpus[..num_cpus as usize];
                fstart_mp::prepare_default_smm_relocation(targets);
                let default_handler = unsafe {
                    fstart_smm::install_default_relocation_callback_stub(
                        image,
                        fstart_smm::DefaultRelocationCallbackConfig {
                            default_smbase: fstart_mp::SMM_DEFAULT_SMBASE,
                            cr3: fstart_arch_x86::x86::controlregs::cr3(),
                            callback: fstart_mp::default_smm_relocation_handler as *const ()
                                as usize as u64,
                            stack_top: fstart_mp::SMM_DEFAULT_ENTRY_STACK_TOP,
                        },
                    )
                };
                if default_handler.is_err() {
                    self.smm_close();
                    fstart_log::error!("Q35 SMM: failed to install default relocation handler");
                    return Err(SmmError::InstallFailed);
                }

                fstart_log::info!(
                    "Q35 SMM: installed image common={:#x} entry={:#x} cpus={}",
                    installed.common_base,
                    installed.common_entry,
                    installed.cpus.len()
                );
                Ok(())
            }
            Err(_) => {
                self.smm_close();
                fstart_log::error!("Q35 SMM: failed to install SMM image");
                Err(SmmError::InstallFailed)
            }
        }
    }

    fn smm_relocate(&self) {
        // APMC writes synchronously trigger software SMI when APMC_EN is set.
        // Refresh EOS before every relocation SMI so multi-CPU relocation is
        // not blocked by the previous SMI cycle.
        let pm = fstart_pmio_ich::PmIo::new(Q35_PMBASE);
        pm.setbits32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
        unsafe { fstart_pio::outb(APM_CNT, APM_CNT_SMI) };
        // QEMU TCG may defer SMI injection until the translation block ends.
        // `pause` forces a new TB so each CPU takes the SMI before it advances
        // to the next MP rendezvous step.
        core::hint::spin_loop();
    }

    fn pre_smm_init(&self) {
        self.setup_ich9_pm_io();
        let pm = fstart_pmio_ich::PmIo::new(Q35_PMBASE);
        pm.reset_smi_status();
        pm.write32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
    }

    fn post_smm_init(&self) {
        self.smm_close();
        let pm = fstart_pmio_ich::PmIo::new(Q35_PMBASE);
        pm.write32(
            fstart_pmio_ich::SMI_EN,
            fstart_pmio_ich::APMC_EN | fstart_pmio_ich::GBL_SMI_EN | fstart_pmio_ich::EOS,
        );
        self.smm_lock();
        fstart_log::info!("Q35 SMM: global SMI enabled and SMRAM locked");
    }
}
