//! Cache-as-RAM teardown for x86 platforms.
//!
//! Mirrors coreboot's Intel non-evict postcar flow:
//! 1. switch to a DRAM stack in the caller
//! 2. disable cache, disable MTRRs, clear NEM RUN/SETUP
//! 3. re-enable cache, `invd`
//! 4. program post-CAR MTRRs and re-enable MTRRs
//!
//! This must run after DRAM training and before large DRAM/FFS work.

use core::arch::global_asm;

global_asm!(
    ".text",
    ".code64",
    ".global _car_teardown",
    ".global _postcar_mtrr_setup",
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
    // ------------------------------------------------------------------
    // _postcar_mtrr_setup — equivalent of coreboot exit_car.S tail plus
    // postcar_mtrr_setup() for fstart's fixed early MTRR layout.
    // ------------------------------------------------------------------
    "_postcar_mtrr_setup:",
    // Re-enable cache and invalidate stale cache contents.
    "movq %cr0, %rax",
    "andq $0xffffffff9fffffff, %rax", // clear CD + NW
    "movq %rax, %cr0",
    "invd",
    // MTRR0 = DRAM write-back range supplied by linker.
    "movl $0x200, %ecx",
    "movl $_dram_mtrr_base, %eax",
    "orl $0x06, %eax",
    "xorl %edx, %edx",
    "wrmsr",
    "movl $0x201, %ecx",
    "movl $_dram_mtrr_mask, %eax",
    "orl $0x800, %eax",
    // Pineview has 36 physical address bits; set high mask bits so the
    // temporary post-CAR DRAM MTRR decodes as the intended low-DRAM range,
    // not as a huge range extending over MMIO/ROM.
    "movl $0x0000000f, %edx",
    "wrmsr",
    // MTRR1 was programmed by CAR setup as ROM WP and is preserved while
    // MTRRs are disabled. Re-enable variable MTRRs with default type kept.
    "movl $0x2ff, %ecx",
    "rdmsr",
    "andl $0xfffff0ff, %eax", // preserve default type low byte only
    "orl $0x800, %eax",
    "wrmsr",
    "ret",
    options(att_syntax),
);

unsafe extern "C" {
    fn _car_teardown();
    fn _postcar_mtrr_setup();
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

/// Re-enable normal caching and install post-CAR MTRRs.
///
/// # Safety
///
/// Must be called after [`car_teardown`] while still on a DRAM stack.
pub unsafe fn postcar_mtrr_setup() {
    unsafe { _postcar_mtrr_setup() }
}
