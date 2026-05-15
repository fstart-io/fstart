//! Cache-as-RAM teardown for x86 platforms.
//!
//! Mirrors coreboot's Intel non-evict postcar flow:
//! 1. switch to a DRAM stack in the caller
//! 2. disable cache, disable MTRRs, clear NEM RUN/SETUP
//! 3. program post-CAR MTRRs while cache/MTRRs are disabled
//! 4. re-enable MTRRs, re-enable cache, `invd`
//!
//! This must run after DRAM training and before large DRAM/FFS work.

use core::arch::{asm, global_asm};

use fstart_arch_x86::{msr, mtrr};

global_asm!(
    ".text",
    ".code64",
    ".global _car_teardown",
    // ------------------------------------------------------------------
    // _car_teardown — Intel non-evict CAR teardown.
    //
    // Keep this deliberately close to coreboot
    // cpu/intel/car/non-evict/exit_car.S. Do not WBINVD dirty CAR lines:
    // NEM CAR has no backing memory and coreboot uses INVD later.
    // ------------------------------------------------------------------
    "_car_teardown:",
    // Disable cache: CR0.CD=1. Leave NW unchanged here, like coreboot.
    "movq %cr0, %rax",
    "orq $0x40000000, %rax",
    "movq %rax, %cr0",
    // Disable MTRRs.
    "movl $0x2ff, %ecx",
    "rdmsr",
    "andl $0xfffff7ff, %eax",
    "wrmsr",
    // Disable no-evict mode RUN then SETUP.
    "movl $0x2e0, %ecx",
    "rdmsr",
    "andl $0xfffffffd, %eax",
    "wrmsr",
    "andl $0xfffffffe, %eax",
    "wrmsr",
    "ret",
    options(att_syntax),
);

unsafe extern "C" {
    fn _car_teardown();

    static _dram_mtrr_base: u8;
    static _dram_mtrr_size: u8;
    static _rom_mtrr_base: u8;
    static _rom_mtrr_size: u8;
}

/// Tear down Cache-as-RAM non-evict mode.
///
/// # Safety
///
/// Caller must already be executing on a DRAM stack and must not return to
/// CAR-backed data after this call.
pub unsafe fn car_teardown() {
    unsafe { _car_teardown() }
}

unsafe fn invalidate_cache_after_reenable() {
    unsafe {
        asm!("invd", options(nostack, preserves_flags));
    }
}

/// Re-enable normal caching and install post-CAR MTRRs.
///
/// # Safety
///
/// Must be called after [`car_teardown`] while still on a DRAM stack and before
/// any large DRAM or memory-mapped flash copies. This function runs the MTRR
/// update with cache and MTRRs still disabled by [`car_teardown`].
pub unsafe fn postcar_mtrr_setup() {
    unsafe {
        let count = mtrr::variable_count();
        for index in 0..count {
            mtrr::clear_variable(index);
        }

        let dram_base = core::ptr::addr_of!(_dram_mtrr_base) as u64;
        let dram_size = core::ptr::addr_of!(_dram_mtrr_size) as u64;
        if dram_size != 0 && count > 0 {
            mtrr::set_variable(0, dram_base, dram_size, mtrr::MTRR_TYPE_WRITE_BACK);
        }

        let rom_base = core::ptr::addr_of!(_rom_mtrr_base) as u64;
        let rom_size = core::ptr::addr_of!(_rom_mtrr_size) as u64;
        if rom_size != 0 && count > 1 {
            mtrr::set_variable(1, rom_base, rom_size, mtrr::MTRR_TYPE_WRITE_PROTECT);
        }

        let def_type = msr::rdmsr(mtrr::IA32_MTRR_DEF_TYPE);
        msr::wrmsr(mtrr::IA32_MTRR_DEF_TYPE, (def_type & 0xff) | (1 << 11));
        mtrr::enable_cache();
        invalidate_cache_after_reenable();
    }
}
