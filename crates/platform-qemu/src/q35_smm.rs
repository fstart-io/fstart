//! QEMU q35 TSEG geometry and SMRAM window control.
//!
//! The install/relocation flow itself is the shared Intel gen1
//! [`IntelSmm`](fstart_arch::x86::cpu::intel::smm::IntelSmm); this module supplies
//! the chipset halves: [`SmramControl`] for the MCH (coreboot's
//! `mainboard/emulation/qemu-q35/memmap.c`) and the ICH9 PM I/O block for
//! [`IchSmi`] (`southbridge/intel/common/smi.c`).

#[cfg(feature = "stage")]
use fstart_arch::x86::cpu::intel::smm::SmramControl;
use fstart_arch::x86::cpu::intel::smm::{SmmCpu, SmrrPair, X86SaveStateFormat};
use fstart_core::mmio::{MmioReadOnly, MmioReadWrite, tock_registers};
use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use fstart_driver_intel::gmch::smram;
use fstart_driver_intel::southbridge::smi::{ICH8_GPE0, IchSmi};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

use crate::q35::Q35HostBridge;
use fstart_pci::EcamDevice;

register_bitfields![u8,
    ESMRAMC [
        T_EN OFFSET(0) NUMBITS(1) [],
        TSEG_SIZE OFFSET(1) NUMBITS(2) [
            OneMiB = 0,
            TwoMiB = 1,
            EightMiB = 2,
            Extended = 3
        ],
        H_SMRAME OFFSET(7) NUMBITS(1) []
    ]
];

fstart_pci::pci_type0_config! {
    /// Q35 MCH SMRAM configuration, sharing the GMCH SMRAM field definitions.
    struct Q35SmramConfig {
        (0x40 => _q35_reserved0),
        // Retain the existing low-byte then high-byte read sequence.
        (0x50 => extended_tseg_mbytes: [MmioReadOnly<u8>; 2]),
        (0x52 => _q35_reserved1),
        (0x9d => smramc: MmioReadWrite<u8, smram::SMRAM::Register>),
        (0x9e => esmramc: MmioReadWrite<u8, ESMRAMC::Register>),
        (0x9f => _q35_reserved2),
        (0xa0 => @END),
    }
}

impl Q35SmramConfig {
    fn tseg_size(&self) -> usize {
        // Permanent SMRAM always uses TSEG: decode its configured size even
        // while T_EN is clear, then enable it after installation in close().
        match self
            .esmramc
            .read_as_enum(ESMRAMC::TSEG_SIZE)
            .expect("all two-bit TSEG size encodings are defined")
        {
            ESMRAMC::TSEG_SIZE::Value::OneMiB => 1 << 20,
            ESMRAMC::TSEG_SIZE::Value::TwoMiB => 2 << 20,
            ESMRAMC::TSEG_SIZE::Value::EightMiB => 8 << 20,
            ESMRAMC::TSEG_SIZE::Value::Extended => {
                usize::from(u16::from_le_bytes([
                    self.extended_tseg_mbytes[0].get(),
                    self.extended_tseg_mbytes[1].get(),
                ])) << 20
            }
        }
    }

    fn open(&self) -> bool {
        self.smramc.set(smram::open());
        self.esmramc.modify(ESMRAMC::T_EN::CLEAR);
        smram::is_open(self.smramc.get())
    }

    fn close(&self) {
        self.smramc.set(smram::closed());
        self.esmramc.modify(ESMRAMC::T_EN::SET);
    }

    fn lock(&self) -> bool {
        self.smramc.set(smram::locked());
        smram::is_locked(self.smramc.get()) && self.esmramc.is_set(ESMRAMC::T_EN)
    }
}

fn mch_config() -> &'static Q35SmramConfig {
    // SAFETY: the verified Q35 host bridge owns 00:00.0; shared ECAM is
    // initialized before TSEG discovery and remains mapped through SMM setup.
    unsafe { EcamDevice::new(0, 0, 0).regs() }
}

/// ICH9 PMBASE programmed by [`Q35HostBridge::setup_ich9_pm_io`](crate::q35).
pub const Q35_PMBASE: u16 = 0x0600;

/// SMI routing for the emulated ICH9: ICH8-style 64-bit GPE0 at 0x20.
pub(crate) const fn ich9_smi() -> IchSmi {
    IchSmi::new(Q35_PMBASE, ICH8_GPE0)
}

/// SMM facts of QEMU's emulated CPU, whatever `-cpu` model is chosen.
pub(crate) struct QemuSmmCpu;

impl SmmCpu for QemuSmmCpu {
    /// QEMU writes the AMD64 layout (revision `0x20064`) for every CPU
    /// model with long mode, like coreboot's q35 `relocation_handler`
    /// expects. For one without, such as `-cpu coreduo`, QEMU 9.0 and newer
    /// write the legacy 32-bit layout and older versions still AMD64.
    fn smm_save_state_format(&self) -> X86SaveStateFormat {
        if long_mode() {
            X86SaveStateFormat::Amd64
        } else {
            X86SaveStateFormat::Amd64OrIntelLegacy
        }
    }

    /// QEMU does not emulate SMRR.
    fn smrr_pair(&self) -> Option<SmrrPair> {
        None
    }
}

fn long_mode() -> bool {
    use fstart_arch::x86::cpuid;
    const LM: u32 = 1 << 29;
    cpuid(0x8000_0000).0 >= 0x8000_0001 && cpuid(0x8000_0001).3 & LM != 0
}

// ---------------------------------------------------------------------------
// TSEG geometry (coreboot q35 `memmap.c`)
// ---------------------------------------------------------------------------

// Called after Q35HostBridge enables and initializes the shared ECAM region.
pub(crate) fn decode_tseg_size() -> usize {
    mch_config().tseg_size()
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

#[cfg(feature = "stage")]
impl SmramControl for Q35HostBridge {
    fn tseg(&self) -> Option<(u64, u32)> {
        let size = decode_tseg_size();
        (size != 0 && self.tseg_base() != 0).then(|| (self.tseg_base(), size as u32))
    }

    fn smram_open(&self) -> bool {
        mch_config().open()
    }

    fn smram_close(&self) {
        mch_config().close();
    }

    fn smram_lock(&self) -> bool {
        mch_config().lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::cell::UnsafeCell;

    #[repr(C, align(4))]
    struct ConfigBytes(UnsafeCell<[u8; 0xa0]>);

    impl ConfigBytes {
        fn regs(&self) -> &Q35SmramConfig {
            // SAFETY: aligned, initialized backing storage with interior
            // mutability; the complete overlay fits and cannot outlive it.
            unsafe { &*self.0.get().cast::<Q35SmramConfig>() }
        }

        fn byte(&self, offset: usize) -> u8 {
            // SAFETY: tests access valid offsets in this owned storage.
            unsafe { (*self.0.get())[offset] }
        }
    }

    #[test]
    fn window_control_preserves_tseg_configuration_and_neighbor_bytes() {
        let image = ConfigBytes(UnsafeCell::new([0xa5; 0xa0]));
        let regs = image.regs();
        assert!(regs.open());
        assert_eq!(image.byte(0x9d), 0x4a);
        assert_eq!(image.byte(0x9e), 0xa4);
        regs.close();
        assert_eq!(image.byte(0x9d), 0x0a);
        assert_eq!(image.byte(0x9e), 0xa5);
        assert!(regs.lock());
        assert_eq!(image.byte(0x9d), 0x1a);
        assert_eq!(image.byte(0x9e), 0xa5);
        assert_eq!(image.byte(0x9c), 0xa5);
        assert_eq!(image.byte(0x9f), 0xa5);
    }

    #[test]
    fn configured_size_is_decoded_while_tseg_is_disabled() {
        let mut image = ConfigBytes(UnsafeCell::new([0; 0xa0]));
        image.0.get_mut()[0x50..0x52].copy_from_slice(&0x0110u16.to_le_bytes());
        let regs = image.regs();
        for (size, mbytes) in [
            (ESMRAMC::TSEG_SIZE::OneMiB, 1),
            (ESMRAMC::TSEG_SIZE::TwoMiB, 2),
            (ESMRAMC::TSEG_SIZE::EightMiB, 8),
            (ESMRAMC::TSEG_SIZE::Extended, 0x0110),
        ] {
            regs.esmramc.write(size + ESMRAMC::T_EN::CLEAR);
            assert_eq!(regs.tseg_size(), mbytes << 20);
            assert!(!regs.esmramc.is_set(ESMRAMC::T_EN));
        }
    }
}
