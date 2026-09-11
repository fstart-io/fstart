//! QEMU q35 TSEG/SMM operations.
//!
//! Uses the LAPIC self-SMI relocation trigger shared by the live Intel
//! drivers, which matches coreboot's `smm_initiate_relocation()`.
//!
//! Hardware sequence mirrors coreboot's `mainboard/emulation/qemu-q35`:
//! `memmap.c` (`decode_tseg_size`, `smm_region`, `smm_open/close/lock`) and
//! `cpu.c` (`get_smm_info`, AMD64 save-state size), with SMI routing per
//! `southbridge/intel/common/smi.c` (`smm_southbridge_clear_state()` then
//! `global_smi_enable()`).

use core::cell::UnsafeCell;

use fstart_arch::mp::{SmmError, SmmInfo, SmmOps};
use fstart_core::services::memory_detect::{E820Entry, E820Kind};

use crate::q35::Q35HostBridge;

// Q35 MCH (00:00.0) SMRAM registers. Matches coreboot's
// `mainboard/emulation/qemu-q35/q35.h`.
const EXT_TSEG_MBYTES: u8 = 0x50;
const SMRAMC: u8 = 0x9d;
const G_SMRAME: u8 = 1 << 3;
const D_LCK: u8 = 1 << 4;
const D_OPEN: u8 = 1 << 6;
const C_BASE_SEG: u8 = 0b010;
const ESMRAMC: u8 = 0x9e;
const T_EN: u8 = 1 << 0;
const TSEG_SZ_MASK: u8 = 3 << 1;

/// ICH9 PMBASE programmed by [`Q35HostBridge::setup_ich9_pm_io`](crate::q35).
pub const Q35_PMBASE: u16 = 0x0600;
/// AMD64 SMM save-state size (coreboot q35 `cpu.c` uses the amd64 area).
const AMD64_SAVE_STATE_SIZE: usize = 0x400;

// ---------------------------------------------------------------------------
// Minimal PMBASE PIO accessor (ICH9 SMI/PM1/GPE0 subset)
// ---------------------------------------------------------------------------

const SMI_EN: u16 = 0x30;
const SMI_STS: u16 = 0x34;
const GPE0_STS_64: u16 = 0x20;
const PM1_STS: u16 = 0x00;
const PM1_EN: u16 = 0x02;
const TCO_BASE_OFF: u16 = 0x60;
const TCO1_STS: u16 = 0x04;

const GBL_SMI_EN: u32 = 1 << 0;
const EOS: u32 = 1 << 1;
const SLP_SMI_EN: u32 = 1 << 4;
const APMC_EN: u32 = 1 << 5;
const TCO_EN: u32 = 1 << 13;
const PWRBTN_EN: u16 = 1 << 8;
const GBL_EN: u16 = 1 << 5;

#[derive(Clone, Copy)]
struct Pm(u16);

impl Pm {
    fn read32(self, reg: u16) -> u32 {
        // SAFETY: PMBASE was programmed by the q35 init flow.
        unsafe { fstart_core::pio::inl(self.0 + reg) }
    }
    fn write32(self, reg: u16, val: u32) {
        // SAFETY: PMBASE was programmed by the q35 init flow.
        unsafe { fstart_core::pio::outl(self.0 + reg, val) }
    }
    fn read16(self, reg: u16) -> u16 {
        // SAFETY: PMBASE was programmed by the q35 init flow.
        unsafe { fstart_core::pio::inw(self.0 + reg) }
    }
    fn write16(self, reg: u16, val: u16) {
        // SAFETY: PMBASE was programmed by the q35 init flow.
        unsafe { fstart_core::pio::outw(self.0 + reg, val) }
    }
    fn setbits32(self, reg: u16, bits: u32) {
        self.write32(reg, self.read32(reg) | bits);
    }
    /// coreboot `reset_smi_status()` + `reset_pm1_status()` +
    /// `reset_tco_status()` + `reset_gpe0_status()` (64-bit GPE0 on ICH9).
    fn clear_smi_state(self) {
        let sts = self.read32(SMI_STS);
        self.write32(SMI_STS, sts);
        let pm1 = self.read16(PM1_STS);
        self.write16(PM1_STS, pm1);
        let tco = self.read32(TCO_BASE_OFF + TCO1_STS);
        self.write32(TCO_BASE_OFF + TCO1_STS, tco & !(1 << 18));
        if tco & (1 << 18) != 0 {
            self.write32(TCO_BASE_OFF + TCO1_STS, 1 << 18);
        }
        self.write32(GPE0_STS_64, 0xffff_ffff);
        self.write32(GPE0_STS_64 + 4, 0xffff_ffff);
    }
}

// ---------------------------------------------------------------------------
// Per-CPU layout scratch (BSP-only installer use, mirrors Intel drivers)
// ---------------------------------------------------------------------------

const ZERO_CPU_LAYOUT: fstart_smm::CpuSmmLayout = fstart_smm::CpuSmmLayout {
    smbase: 0,
    entry_addr: 0,
    save_state_base: 0,
    save_state_top: 0,
    stack_bottom: 0,
    stack_top: 0,
};

struct CpuLayoutStore(UnsafeCell<[fstart_smm::CpuSmmLayout; fstart_smm::runtime::MAX_SMM_CPUS]>);

// SAFETY: firmware runs the SMM installer on the BSP while SMRAM is open;
// no other code accesses this scratch buffer concurrently.
unsafe impl Sync for CpuLayoutStore {}

static Q35_SMM_CPU_LAYOUTS: CpuLayoutStore = CpuLayoutStore(UnsafeCell::new(
    [ZERO_CPU_LAYOUT; fstart_smm::runtime::MAX_SMM_CPUS],
));

// ---------------------------------------------------------------------------
// TSEG geometry (coreboot q35 `memmap.c`)
// ---------------------------------------------------------------------------

fn pci_read_host8(reg: u8) -> u8 {
    let aligned = reg & !3;
    let shift = ((reg & 3) as u32) * 8;
    // SAFETY: caller selects a valid Q35 MCH config register.
    ((unsafe { fstart_core::pio::pci_cfg_read32(0, 0, 0, aligned) } >> shift) & 0xff) as u8
}

fn pci_write_host8(reg: u8, val: u8) {
    let aligned = reg & !3;
    let shift = ((reg & 3) as u32) * 8;
    // SAFETY: caller selects a valid Q35 MCH config register.
    let old = unsafe { fstart_core::pio::pci_cfg_read32(0, 0, 0, aligned) };
    let new = (old & !(0xffu32 << shift)) | ((val as u32) << shift);
    // SAFETY: caller selects a valid Q35 MCH config register.
    unsafe { fstart_core::pio::pci_cfg_write32(0, 0, 0, aligned, new) };
}

pub(crate) fn decode_tseg_size() -> usize {
    let mut esmramc = pci_read_host8(ESMRAMC);
    // fstart's Q35 path always uses TSEG for permanent SMRAM. If QEMU has
    // not yet reflected T_EN, decode the configured size anyway and enable
    // TSEG in `smm_close()` after installation (coreboot `memmap.c` fakes
    // T_EN the same way under SMM_TSEG).
    esmramc |= T_EN;
    match (esmramc & TSEG_SZ_MASK) >> 1 {
        0 => 1 << 20,
        1 => 2 << 20,
        2 => 8 << 20,
        _ => {
            let lo = pci_read_host8(EXT_TSEG_MBYTES);
            let hi = pci_read_host8(EXT_TSEG_MBYTES + 1);
            ((u16::from(lo) | (u16::from(hi) << 8)) as usize) << 20
        }
    }
}

/// TSEG base from the firmware memory map: prefer an explicit reserved region
/// of the decoded size below 4 GiB (QEMU reports TSEG this way), else fall
/// back to the top of installed low RAM below the flash window (whose top
/// is exactly 4 GiB and would otherwise win the computation).
pub(crate) fn tseg_base_from_e820(entries: &[E820Entry], size: usize) -> u64 {
    let size_u64 = size as u64;
    for entry in entries {
        if entry.kind != E820Kind::Ram as u32
            && entry.size == size_u64
            && entry.addr < 0x1_0000_0000
        {
            return entry.addr;
        }
    }
    let mut top = 0u64;
    for entry in entries {
        // Below the flash/MMIO window; the flash entry tops out at 4 GiB.
        if entry.addr >= 0xf000_0000 {
            continue;
        }
        let end = entry.addr.saturating_add(entry.size).min(0x1_0000_0000);
        if end > top {
            top = end;
        }
    }
    top.saturating_sub(size_u64)
}

// ---------------------------------------------------------------------------
// SMRAM window control (coreboot q35 `memmap.c` open/close/lock)
// ---------------------------------------------------------------------------

fn smm_open() {
    pci_write_host8(SMRAMC, D_OPEN | G_SMRAME | C_BASE_SEG);
    let esmramc = pci_read_host8(ESMRAMC);
    pci_write_host8(ESMRAMC, esmramc & !T_EN);
}

fn smm_close() {
    pci_write_host8(SMRAMC, G_SMRAME | C_BASE_SEG);
    let esmramc = pci_read_host8(ESMRAMC);
    pci_write_host8(ESMRAMC, esmramc | T_EN);
}

fn smm_lock() {
    pci_write_host8(SMRAMC, D_LCK | G_SMRAME | C_BASE_SEG);
}

fn smi_enable_for_relocation() {
    Pm(Q35_PMBASE).setbits32(SMI_EN, APMC_EN | GBL_SMI_EN | EOS);
}

// ---------------------------------------------------------------------------
// SmmOps
// ---------------------------------------------------------------------------

impl SmmOps for Q35HostBridge {
    fn smm_info(&self) -> Option<SmmInfo> {
        let size = decode_tseg_size();
        if size == 0 || self.tseg_base() == 0 {
            fstart_log::error!("Q35 SMM: TSEG unavailable");
            return None;
        }
        fstart_log::info!(
            "Q35 SMM: TSEG base={:#x} size={:#x}",
            self.tseg_base(),
            size
        );
        Some(SmmInfo {
            smbase: self.tseg_base(),
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
        smm_open();

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
                    cr3: fstart_arch::x86::controlregs::cr3(),
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
                fstart_arch::mp::prepare_default_smm_relocation(targets);
                let default_handler = unsafe {
                    fstart_smm::install_default_relocation_callback_stub(
                        image,
                        fstart_smm::DefaultRelocationCallbackConfig {
                            default_smbase: fstart_arch::mp::SMM_DEFAULT_SMBASE,
                            cr3: fstart_arch::x86::controlregs::cr3(),
                            callback: fstart_arch::mp::default_smm_relocation_handler as *const ()
                                as usize as u64,
                            stack_top: fstart_arch::mp::SMM_DEFAULT_ENTRY_STACK_TOP,
                        },
                    )
                };
                if default_handler.is_err() {
                    smm_close();
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
                smm_close();
                fstart_log::error!("Q35 SMM: failed to install SMM image");
                Err(SmmError::InstallFailed)
            }
        }
    }

    fn smm_relocate(&self) {
        smi_enable_for_relocation();
        // Match coreboot `smm_initiate_relocation()`: relocation is triggered
        // with a local-APIC SMI IPI to *this* CPU, not by writing APM_CNT.
        // (An APM_CNT write also reaches QEMU's SMI path, but LAPIC
        // self-SMI is what all live Intel drivers use and what coreboot uses
        // on q35 as well.)
        let lapic = fstart_arch::lapic::Lapic::from_msr();
        lapic.send_ipi_self(fstart_arch::lapic::INT_ASSERT | fstart_arch::lapic::MT_SMI);
        lapic.wait_ready();
    }

    fn pre_smm_init(&self) {
        let pm = Pm(Q35_PMBASE);
        pm.clear_smi_state();
        pm.write32(SMI_EN, APMC_EN | GBL_SMI_EN | EOS);
    }

    fn post_smm_init(&self) {
        smm_close();
        // coreboot `global_smi_enable()` after `smm_southbridge_clear_state()`.
        let pm = Pm(Q35_PMBASE);
        pm.clear_smi_state();
        pm.write16(PM1_EN, PWRBTN_EN | GBL_EN);
        pm.write32(SMI_EN, TCO_EN | APMC_EN | SLP_SMI_EN | GBL_SMI_EN | EOS);
        smm_lock();
        fstart_log::info!("Q35 SMM: global SMI enabled and SMRAM locked");
    }
}
