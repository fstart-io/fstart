//! Post-MP SMM installation for AMD64 CPUs.
//!
//! AMD CPUs take TSEG and SMBASE from MSRs that normal mode can write until
//! HWCR.SmmLock is set, so relocation needs no SMI: every CPU programs its own
//! SMM_ADDR, SMM_MASK and SMM_BASE and locks them. The southbridge owns its SMI sources through
//! [`SmiControl`]; nothing here is chipset specific.

use core::ops::Range;
use core::sync::atomic::{AtomicU32, Ordering};

use tock_registers::LocalRegisterCopy;
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use crate::x86::cpu::amd::msr::{HWCR, Hwcr, SMM_ADDR, SMM_BASE, SMM_MASK, SmmMask};
use crate::x86::cpu::msr::{MsrAccess, NativeMsr};
use crate::x86::mp::{MAX_CPUS, MpHandle};

/// Save-state bytes reserved at the top of every SMBASE window. The AMD64
/// layout uses the top 0x200; the installer staggers windows by this much.
const SAVE_STATE_SIZE: u32 = 0x400;
/// Smallest TSEG the SMM_MASK granularity allows.
const MIN_TSEG: u32 = 128 << 10;

/// Southbridge SMI control.
pub trait SmiControl {
    /// Configuration of the southbridge's permanent SMM handler; the same type
    /// as that handler's [`fstart_smm::SmmHandler::Config`].
    type HandlerConfig: Copy;

    /// Configuration written into SMRAM for the permanent handler.
    fn smm_handler_config(&self) -> Self::HandlerConfig;
    /// Stop the chipset from generating SMIs while TSEG and SMBASE change.
    fn disable_smi(&self);
    /// Program the permanent SMI sources and let the chipset generate SMIs.
    fn enable_smi(&self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AmdSmmError {
    BadTseg,
    Locked,
    TooManyCpus,
    InstallFailed,
    MsrSetupFailed,
}

impl AmdSmmError {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::BadTseg => "BadTseg",
            Self::Locked => "Locked",
            Self::TooManyCpus => "TooManyCpus",
            Self::InstallFailed => "InstallFailed",
            Self::MsrSetupFailed => "MsrSetupFailed",
        }
    }
}

/// What the installation left behind, for later SMM users such as the PSP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledTseg {
    pub base: u64,
    /// SMM_MASK as programmed on every CPU.
    pub mask: u64,
    /// Zeroed, page-aligned TSEG bytes the handler, stacks and SMBASE windows
    /// don't use.
    pub free: Range<u64>,
}

/// A TSEG that SMM_ADDR/SMM_MASK can describe: a power of two of at least
/// 128 KiB, aligned to its size, below 4 GiB (the SMM image runs there).
fn valid_tseg(base: u64, size: u32) -> bool {
    size.is_power_of_two()
        && size >= MIN_TSEG
        && base.is_multiple_of(u64::from(size))
        && base
            .checked_add(u64::from(size))
            .is_some_and(|end| end <= 1 << 32)
}

/// SMM_MASK for a TSEG of `size` with `address_bits` physical address bits:
/// valid, write-back in SMM, no ASEG.
fn smm_mask(size: u32, address_bits: u32) -> u64 {
    let mask = !(u64::from(size) - 1) & ((1 << address_bits) - 1);
    let mut value = LocalRegisterCopy::<u64, SmmMask::Register>::new(0);
    value.modify(
        SmmMask::MASK.val(mask >> 17) + SmmMask::TM_TYPE_DRAM::WriteBack + SmmMask::T_VALID::SET,
    );
    value.get()
}

/// The TSEG bytes between the stacks at its bottom and the lowest SMBASE
/// window below the handler.
fn free_range(cpus: &[fstart_smm::CpuSmmLayout]) -> Range<u64> {
    let stacks = cpus.iter().map(|cpu| cpu.stack_top).max().unwrap_or(0);
    let windows = cpus.iter().map(|cpu| cpu.smbase).min().unwrap_or(0);
    let Some(start) = stacks.checked_next_multiple_of(0x1000) else {
        return 0..0;
    };
    start..(windows & !0xfff).max(start)
}

/// Program this CPU's TSEG and SMBASE and lock them. Returns SMM_ADDR,
/// SMM_MASK, SMM_BASE and HWCR.SmmLock as read back afterwards.
///
/// # Safety
/// AMD64 CPU in normal mode, with SMM not locked and nothing on this CPU
/// accessing TSEG from normal mode afterwards.
unsafe fn program_cpu(base: u64, mask: u64, smbase: u64) -> [u64; 4] {
    // SAFETY: the caller's contract; these MSRs exist on every AMD64 CPU.
    let cpu = unsafe { NativeMsr::new() };
    cpu.reg(SMM_ADDR).set(base);
    cpu.reg(SMM_MASK).set(mask);
    cpu.reg(SMM_BASE).set(smbase);
    cpu.reg(HWCR).modify(Hwcr::SMM_LOCK::SET);
    [
        cpu.reg(SMM_ADDR).get(),
        cpu.reg(SMM_MASK).get(),
        cpu.reg(SMM_BASE).get(),
        cpu.reg(HWCR).read(Hwcr::SMM_LOCK),
    ]
}

const ZERO_CPU_LAYOUT: fstart_smm::CpuSmmLayout = fstart_smm::CpuSmmLayout {
    smbase: 0,
    entry_addr: 0,
    save_state_base: 0,
    save_state_top: 0,
    stack_bottom: 0,
    stack_top: 0,
};

/// One SMM installation on AMD64 CPUs with the southbridge's SMI control.
pub struct AmdSmm<'a, S: SmiControl> {
    name: &'static str,
    smi: &'a S,
}

impl<'a, S: SmiControl> AmdSmm<'a, S> {
    pub const fn new(name: &'static str, smi: &'a S) -> Self {
        Self { name, smi }
    }

    /// Install the handler `image` in the TSEG at `base`, point every online
    /// CPU's SMBASE into it, lock SMM and enable the chipset's SMIs. A cold
    /// boot path: it fails if SMM is already locked.
    pub fn install(
        &self,
        mp: &MpHandle,
        image: &[u8],
        base: u64,
        size: u32,
    ) -> Result<InstalledTseg, AmdSmmError> {
        if !valid_tseg(base, size) {
            return Err(AmdSmmError::BadTseg);
        }
        let num_cpus = mp.num_cpus();
        if usize::from(num_cpus) > MAX_CPUS {
            return Err(AmdSmmError::TooManyCpus);
        }
        // SAFETY: HWCR and SMM_MASK exist on every AMD64 CPU.
        let cpu = unsafe { NativeMsr::new() };
        if cpu.reg(HWCR).is_set(Hwcr::SMM_LOCK) {
            return Err(AmdSmmError::Locked);
        }
        fstart_log::info!(
            "{} SMM: TSEG base={:#x} size={:#x}, online CPUs={}",
            self.name,
            base,
            size,
            num_cpus
        );
        self.smi.disable_smi();
        // TSEG is plain DRAM for this CPU until TValid.
        // SMM isn't locked, so SMM_MASK is writable.
        cpu.reg(SMM_MASK)
            .modify(SmmMask::T_VALID::CLEAR + SmmMask::A_VALID::CLEAR);
        // SAFETY: the caller's TSEG is reserved DRAM nothing else uses; the
        // installer validates every placement before writing.
        unsafe { core::ptr::write_bytes(base as *mut u8, 0, size as usize) };
        let handler_config = self.smi.smm_handler_config();
        let mut layouts = [ZERO_CPU_LAYOUT; MAX_CPUS];
        let installed = unsafe {
            fstart_smm::install_pic_image(
                image,
                fstart_smm::InstallConfig {
                    smram_base: base,
                    smram_size: u64::from(size),
                    num_cpus,
                    save_state_size: SAVE_STATE_SIZE,
                    handler_config: &handler_config,
                },
                &mut layouts,
            )
        }
        .map_err(|_| AmdSmmError::InstallFailed)?;
        // SAFETY: TSEG is still ordinary cacheable DRAM here.
        unsafe { crate::x86::writeback_cache_range(base as *const u8, size as usize) };
        fstart_log::info!(
            "{} SMM: handler={:#x} CR3={:#x}",
            self.name,
            installed.common_entry,
            installed.cr3
        );

        // Every address bit CPUID reports, also those memory encryption takes
        // away.
        let mask = smm_mask(size, crate::x86::physical_address_bits());
        let failures = AtomicU32::new(0);
        let cpus = installed.cpus;
        // The APs run before the BSP, which locks last as the vendor does.
        mp.scope(|scope| {
            scope.scatter(&|cpu| {
                let Some(layout) = cpus.get(cpu as usize) else {
                    failures.fetch_add(1, Ordering::AcqRel);
                    return;
                };
                // SAFETY: normal mode, SMM not locked; after this nothing
                // touches TSEG from normal mode.
                let read = unsafe { program_cpu(base, mask, layout.smbase) };
                if read != [base, mask, layout.smbase, 1] {
                    fstart_log::error!(
                        "CPU {}: SMM_ADDR {:#x} SMM_MASK {:#x} SMM_BASE {:#x} SmmLock {}",
                        cpu,
                        read[0],
                        read[1],
                        read[2],
                        read[3]
                    );
                    failures.fetch_add(1, Ordering::AcqRel);
                }
            })
        });
        if failures.load(Ordering::Acquire) != 0 {
            return Err(AmdSmmError::MsrSetupFailed);
        }
        fstart_log::info!(
            "{} SMM: {} CPUs relocated and locked, SMM_MASK={:#x}",
            self.name,
            num_cpus,
            mask
        );

        self.smi.enable_smi();
        // Exercise every CPU's permanent entry once: a broken stub or handler
        // stops the boot here instead of at the first OS SMI.
        mp.scope(|scope| {
            scope.scatter(&|_| {
                let lapic = crate::x86::lapic::Lapic::from_msr();
                lapic.send_smi_self();
                lapic.wait_ready();
            })
        });
        fstart_log::info!(
            "{} {} {} CPUs",
            self.name,
            crate::messages::SMI_RETURNED,
            num_cpus
        );
        Ok(InstalledTseg {
            base,
            mask,
            free: free_range(cpus),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tseg_must_be_a_size_aligned_power_of_two_below_4g() {
        assert!(valid_tseg(0x7f80_0000, 8 << 20));
        assert!(valid_tseg(0xffe0_0000, 128 << 10));
        assert!(!valid_tseg(0x7f90_0000, 8 << 20));
        assert!(!valid_tseg(0x7f80_0000, 6 << 20));
        assert!(!valid_tseg(0x7f80_0000, 64 << 10));
        assert!(!valid_tseg(0x1_0000_0000, 8 << 20));
        // u64 wraparound near u64::MAX must be rejected.
        assert!(!valid_tseg(0xffff_ffff_fffe_0000, 128 << 10));
    }

    #[test]
    fn smm_mask_covers_every_address_bit() {
        // lo = ~(size - 1) | TSEG WB | TValid,
        // hi = (1 << (bits - 32)) - 1.
        assert_eq!(smm_mask(8 << 20, 48), 0x0000_ffff_ff80_6002);
        assert_eq!(smm_mask(128 << 10, 43), 0x0000_07ff_fffe_6002);
    }

    #[test]
    fn free_range_lies_between_stacks_and_smbase_windows() {
        let cpu = |smbase, stack_top| fstart_smm::CpuSmmLayout {
            smbase,
            stack_top,
            ..ZERO_CPU_LAYOUT
        };
        let cpus = [cpu(0x7fef_0000, 0x7f80_0800), cpu(0x7fee_fc00, 0x7f80_1000)];
        assert_eq!(free_range(&cpus), 0x7f80_1000..0x7fee_f000);
        assert_eq!(free_range(&[cpu(0x1000, 0x1800)]), 0x2000..0x2000);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    #[kani::proof]
    fn verify_valid_tseg_matches_u128_spec() {
        let base: u64 = kani::any();
        let size: u32 = kani::any();

        let expected = size >= MIN_TSEG
            && size.is_power_of_two()
            && base % u64::from(size) == 0
            && u128::from(base) + u128::from(size) <= (1u128 << 32);

        assert_eq!(valid_tseg(base, size), expected);
    }

    #[kani::proof]
    fn verify_smm_mask_matches_exact_tseg_address_range() {
        let base: u64 = kani::any();
        let size: u32 = kani::any();
        let address_bits: u32 = kani::any();
        let addr: u64 = kani::any();

        kani::assume(valid_tseg(base, size));
        // Architectural AMD64 physical address width supported by SmmMask::MASK (bits 17..=51).
        kani::assume((32..=52).contains(&address_bits));
        kani::assume(addr < (1u64 << address_bits));

        let raw = smm_mask(size, address_bits);
        let reg = LocalRegisterCopy::<u64, SmmMask::Register>::new(raw);
        assert!(reg.is_set(SmmMask::T_VALID));
        assert!(!reg.is_set(SmmMask::A_VALID));
        assert!(reg.matches_all(SmmMask::TM_TYPE_DRAM::WriteBack));

        // AMD APM Vol. 2 hardware TSEG match equation:
        // ((addr & mask_bits) == (base & mask_bits)) iff addr in [base, base + size).
        let hw_mask = reg.read(SmmMask::MASK) << 17;
        let hw_matches = (addr & hw_mask) == (base & hw_mask);
        let in_range = addr >= base && u128::from(addr) < u128::from(base) + u128::from(size);
        assert_eq!(hw_matches, in_range);
    }

    #[kani::proof]
    fn verify_free_range_is_page_aligned_and_bounded() {
        let stack0: u64 = kani::any();
        let stack1: u64 = kani::any();
        let smbase0: u64 = kani::any();
        let smbase1: u64 = kani::any();

        let cpus = [
            fstart_smm::CpuSmmLayout {
                smbase: smbase0,
                stack_top: stack0,
                ..ZERO_CPU_LAYOUT
            },
            fstart_smm::CpuSmmLayout {
                smbase: smbase1,
                stack_top: stack1,
                ..ZERO_CPU_LAYOUT
            },
        ];
        let range = free_range(&cpus);
        assert!(range.start <= range.end);
        assert_eq!(range.start % 0x1000, 0);
        assert_eq!(range.end % 0x1000, 0);
        if range.start < range.end {
            assert!(range.start >= stack0.max(stack1));
            assert!(range.end <= smbase0.min(smbase1));
        }
    }
}
