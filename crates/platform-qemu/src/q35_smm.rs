//! QEMU q35 TSEG geometry and SMRAM window control.
//!
//! The install/relocation flow itself is the shared Intel gen1
//! [`IntelSmm`](fstart_arch::cpu_intel::smm::IntelSmm); this module supplies
//! the chipset halves: [`SmramControl`] for the MCH (coreboot's
//! `mainboard/emulation/qemu-q35/memmap.c`) and the ICH9 PM I/O block for
//! [`IchSmi`] (`southbridge/intel/common/smi.c`).

use fstart_arch::cpu_intel::smm::{Gpe0Block, SmramControl};
use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use fstart_driver_intel::southbridge::smi::IchSmi;

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

/// SMI routing for the emulated ICH9: ICH8-style 64-bit GPE0 at 0x20.
pub(crate) const fn ich9_smi() -> IchSmi {
    IchSmi::new(Q35_PMBASE, Gpe0Block::ICH8)
}

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

impl SmramControl for Q35HostBridge {
    fn tseg(&self) -> Option<(u64, u32)> {
        let size = decode_tseg_size();
        (size != 0 && self.tseg_base() != 0).then(|| (self.tseg_base(), size as u32))
    }

    fn smram_open(&self) {
        pci_write_host8(SMRAMC, D_OPEN | G_SMRAME | C_BASE_SEG);
        let esmramc = pci_read_host8(ESMRAMC);
        pci_write_host8(ESMRAMC, esmramc & !T_EN);
    }

    fn smram_close(&self) {
        pci_write_host8(SMRAMC, G_SMRAME | C_BASE_SEG);
        let esmramc = pci_read_host8(ESMRAMC);
        pci_write_host8(ESMRAMC, esmramc | T_EN);
    }

    fn smram_lock(&self) {
        pci_write_host8(SMRAMC, D_LCK | G_SMRAME | C_BASE_SEG);
    }
}
