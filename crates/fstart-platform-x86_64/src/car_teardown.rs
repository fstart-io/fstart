//! Cache-as-RAM teardown — runs at the **start of ramstage**.
//!
//! This module is separate from [`car`] (setup) because teardown must
//! run in a *different* stage: the one whose stack is already in DRAM.
//! Calling teardown from the CAR stage would destroy its own stack.
//!
//! ## When to call
//!
//! The generated ramstage entry calls `car_teardown()` as its very
//! first action, before any capability handlers. At this point:
//!
//! - DRAM has been trained (romstage completed `DramInit`)
//! - The linker placed ramstage's stack in DRAM
//! - The old CAR MTRRs are still active but harmless
//!
//! ## What it does
//!
//! 1. Disable cache (CR0.CD = 1)
//! 2. Invalidate all cachelines (WBINVD)
//! 3. Disable MTRRs (clear MTRR_DEF_TYPE_EN)
//! 4. If NEM was active (MSR 0x2E0 bits [1:0] set), clear RUN then SETUP
//! 5. Re-enable cache with DRAM-appropriate MTRR config
//!
//! After this, the CAR region is dead and the CPU uses normal DRAM-backed
//! caching.

core::arch::global_asm!(
    ".att_syntax prefix",
    ".section .text, \"ax\"",
    ".code64",
    ".global _car_teardown",
    // ==================================================================
    // _car_teardown — Tear down CAR, called from ramstage (64-bit mode).
    //
    // At this point we have a DRAM-backed stack, so normal call/ret works.
    // The old CAR MTRR and NEM state are cleaned up so DRAM caching can
    // be configured properly.
    // ==================================================================
    "_car_teardown:",
    // Disable cache.
    "movq %cr0, %rax",
    "orq $0x40000000, %rax", // CR0.CD
    "movq %rax, %cr0",
    // Writeback + invalidate all cachelines.
    // WBINVD flushes any dirty CAR lines (which write to nowhere since
    // there's no backing RAM — but that's fine, the data is stale).
    "wbinvd",
    // Disable MTRRs.
    "movl $0x2FF, %ecx", // MTRR_DEF_TYPE
    "rdmsr",
    "andl $0xFFFFF7FF, %eax", // clear MTRR_DEF_TYPE_EN
    "wrmsr",
    // If NEM was active, clear MSR 0x2E0.
    // Read the NEM MSR — if bits [1:0] are non-zero, NEM was used.
    "movl $0x2E0, %ecx",
    "rdmsr",
    "testl $3, %eax",
    "jz 1f",
    // Clear RUN (bit 1) first, then SETUP (bit 0).
    "andl $0xFFFFFFFD, %eax",
    "wrmsr",
    "andl $0xFFFFFFFE, %eax",
    "wrmsr",
    "1:",
    // Clear the CAR MTRR (MTRR0) so it doesn't shadow DRAM.
    "movl $0x200, %ecx", // MTRR_PHYS_BASE(0)
    "xorl %eax, %eax",
    "xorl %edx, %edx",
    "wrmsr",
    "movl $0x201, %ecx", // MTRR_PHYS_MASK(0)
    "xorl %eax, %eax",
    "xorl %edx, %edx",
    "wrmsr",
    // Re-enable MTRRs (the ROM MTRR1 is still valid for XIP).
    "movl $0x2FF, %ecx",
    "rdmsr",
    "orl $0x800, %eax", // MTRR_DEF_TYPE_EN
    "wrmsr",
    // Defensive flush after MTRR layout change. Caches should be empty
    // from the first wbinvd (CD=1 prevented new fills), but some Intel
    // errata recommend flushing again after MTRR reprogramming.
    "wbinvd",
    // Re-enable cache.
    "movq %cr0, %rax",
    "andq $0xFFFFFFFF9FFFFFFF, %rax", // clear CD + NW
    "movq %rax, %cr0",
    "ret",
);

extern "C" {
    fn _car_teardown();
}

/// Tear down Cache-as-RAM.
///
/// Call this at the very start of ramstage, after the stack has moved
/// to DRAM. Disables NEM (if active), clears the CAR MTRR, and
/// re-enables normal DRAM caching.
///
/// # Safety
///
/// - Must be called from a stage whose stack is in DRAM (not CAR).
/// - Must be called before re-programming MTRRs for the DRAM layout.
/// - After this, the old CAR region at `_car_base` is dead memory.
pub unsafe fn car_teardown() {
    unsafe { _car_teardown() }
}
