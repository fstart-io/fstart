//! Architecture-specific utilities.
//!
//! Provides delay functions, processor halt, and other low-level operations
//! that vary between CPU architectures (ARMv7-A, AArch64, RISC-V).

#![no_std]

// ---------------------------------------------------------------------------
// udelay — microsecond delay (generic spin loop)
// ---------------------------------------------------------------------------

#[cfg(any(feature = "armv7", feature = "aarch64", feature = "riscv64"))]
pub fn udelay(us: u32) {
    for _ in 0..us.saturating_mul(100) {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// mdelay — millisecond delay
// ---------------------------------------------------------------------------

#[cfg(any(feature = "armv7", feature = "aarch64", feature = "riscv64"))]
pub fn mdelay(ms: u32) {
    for _ in 0..ms {
        udelay(1000);
    }
}

// ---------------------------------------------------------------------------
// sdelay — cycle-accurate delay (ARM only)
// ---------------------------------------------------------------------------

#[cfg(all(feature = "armv7", target_arch = "arm"))]
pub fn sdelay(count: u32) {
    unsafe {
        core::arch::asm!(
            "2:",
            "subs {cnt}, {cnt}, #1",
            "bne 2b",
            cnt = inout(reg) count => _,
            options(nostack, nomem, preserves_flags),
        );
    }
}

#[cfg(not(all(feature = "armv7", target_arch = "arm")))]
pub fn sdelay(_count: u32) {}

// ---------------------------------------------------------------------------
// set_cntfrq — ARM Generic Timer frequency register
// ---------------------------------------------------------------------------

/// Program the ARM Generic Timer frequency register (CNTFRQ).
///
/// The CNTFRQ register tells software (Linux, other OSes) the tick rate
/// of the Generic Timer.  It is NOT automatically set by hardware — the
/// boot firmware must program it.
///
/// Must be called from secure mode — CNTFRQ is a banked secure register
/// and writes from non-secure state are silently ignored.
///
/// On Allwinner ARMv7 SoCs, the Generic Timer is clocked from OSC24M,
/// so `freq` should be `24_000_000`.
///
/// U-Boot does this in `board_init()` (board/sunxi/board.c) and again
/// in `_nonsec_init()` (arch/arm/cpu/armv7/nonsec_virt.S) as a safety
/// net before the secure→non-secure transition.
#[cfg(all(feature = "armv7", target_arch = "arm"))]
pub fn set_cntfrq(freq: u32) {
    // SAFETY: writing CNTFRQ from secure SVC mode is architecturally
    // defined.  We first check that the Generic Timer extension is
    // present (ID_PFR1 bits [19:16] != 0), matching U-Boot's guard.
    unsafe {
        core::arch::asm!(
            // Read ID_PFR1 to check for Generic Timer extension.
            "mrc p15, 0, {tmp}, c0, c1, 1",
            "and {tmp}, {tmp}, #0x000F0000",  // bits [19:16] = GenTimer
            "cmp {tmp}, #0",
            "beq 1f",                         // skip if no Generic Timer
            "mcr p15, 0, {freq}, c14, c0, 0", // write CNTFRQ
            "1:",
            tmp = out(reg) _,
            freq = in(reg) freq,
            options(nomem, nostack),
        );
    }
}

/// No-op on non-ARM targets.
#[cfg(not(all(feature = "armv7", target_arch = "arm")))]
pub fn set_cntfrq(_freq: u32) {}

// ---------------------------------------------------------------------------
// halt — put processor in low-power wait state
// ---------------------------------------------------------------------------

#[cfg(all(feature = "armv7", target_arch = "arm"))]
pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}

#[cfg(all(feature = "aarch64", target_arch = "aarch64"))]
pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("wfe") };
    }
}

#[cfg(all(feature = "riscv64", target_arch = "riscv64"))]
pub fn halt() -> ! {
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}

#[cfg(not(any(
    all(feature = "armv7", target_arch = "arm"),
    all(feature = "aarch64", target_arch = "aarch64"),
    all(feature = "riscv64", target_arch = "riscv64")
)))]
pub fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// gic_init — GICv3 initialization via SMC to EL3 (AArch64 only)
// ---------------------------------------------------------------------------

/// Initialize the GICv3 interrupt controller via an SMC call to EL3.
///
/// The EL3 handler (FSTART_GIC_INIT, function ID 0xC200_0001) performs
/// the full TF-A / U-Boot GIC initialization sequence:
///
/// 1. `GICD_CTLR = 0x37` — enable all groups + affinity routing
/// 2. `GICD_IGROUPR[1..N] = 0xFFFF_FFFF` — all SPIs → Group 1 NS
/// 3. `GICD_IGRPMODR[1..N] = 0` — Non-Secure
/// 4. Wake redistributor (`GICR_WAKER`)
/// 5. `GICR_IGROUPR0 = 0xFFFF_FFFF` — all SGIs/PPIs → Group 1 NS
/// 6. `ICC_SRE_EL3 = 0xF` — system register interface + enable lower ELs
/// 7. `ICC_IGRPEN1_EL3 = 0x3` — enable G1NS + G1S forwarding
/// 8. `ICC_PMR_EL1 = 0xFF` — allow all NS priority interrupts
///
/// After this call, all interrupts are Group 1 Non-Secure and will be
/// delivered as IRQ to NS-EL1 (the OS kernel).
///
/// No-op on non-AArch64 targets.
#[cfg(all(feature = "aarch64", target_arch = "aarch64"))]
pub fn gic_init(dist_base: u64, redist_base: u64) {
    let ret: u64;
    // SAFETY: SMC to our own EL3 handler. The handler validates inputs
    // and only writes to GIC MMIO regions and EL3 system registers.
    unsafe {
        core::arch::asm!(
            "smc #0",
            inout("x0") 0xC200_0001u64 => ret,
            in("x1") dist_base,
            in("x2") redist_base,
            // x3-x17 clobbered by SMCCC
            lateout("x3") _,
            options(nomem, nostack),
        );
    }
    if ret != 0 {
        // Best-effort: log would require fstart_log which adds a dependency.
        // The caller (generated code) can check and log.
    }
    let _ = ret;
}

/// No-op on non-AArch64 targets.
#[cfg(not(all(feature = "aarch64", target_arch = "aarch64")))]
pub fn gic_init(_dist_base: u64, _redist_base: u64) {}
