//! Architecture-specific utilities.
//!
//! Provides delay functions, processor halt, and other low-level operations
//! that vary between CPU architectures (ARMv7-A, AArch64, RISC-V, x86_64).

#![no_std]

// ---------------------------------------------------------------------------
// udelay — microsecond delay (generic spin loop)
// ---------------------------------------------------------------------------

#[cfg(any(
    feature = "armv7",
    feature = "aarch64",
    feature = "riscv64",
    feature = "x86_64"
))]
pub fn udelay(us: u32) {
    for _ in 0..us.saturating_mul(100) {
        core::hint::spin_loop();
    }
}

// ---------------------------------------------------------------------------
// mdelay — millisecond delay
// ---------------------------------------------------------------------------

#[cfg(any(
    feature = "armv7",
    feature = "aarch64",
    feature = "riscv64",
    feature = "x86_64"
))]
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
// x86 MSR access
// ---------------------------------------------------------------------------

/// x86 Model-Specific Register (MSR) helpers.
///
/// Provides `rdmsr`/`wrmsr` and a typed `Msr { lo, hi }` struct with
/// idiomatic `From<u64>` / `Into<u64>` conversions.
#[cfg(feature = "x86_64")]
pub mod msr {
    /// MSR value split into 32-bit halves.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct Msr {
        pub lo: u32,
        pub hi: u32,
    }

    impl From<u64> for Msr {
        fn from(v: u64) -> Self {
            Self {
                lo: v as u32,
                hi: (v >> 32) as u32,
            }
        }
    }

    impl From<Msr> for u64 {
        fn from(m: Msr) -> Self {
            (m.lo as u64) | ((m.hi as u64) << 32)
        }
    }

    /// Read a 64-bit MSR.
    ///
    /// # Safety
    ///
    /// Caller must ensure `msr` is a valid MSR index for this CPU.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub unsafe fn rdmsr(msr: u32) -> u64 {
        let lo: u32;
        let hi: u32;
        core::arch::asm!(
            "rdmsr",
            in("ecx") msr,
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack),
        );
        ((hi as u64) << 32) | (lo as u64)
    }

    /// Write a 64-bit MSR.
    ///
    /// # Safety
    ///
    /// Caller must ensure `msr` and `val` are valid for this CPU.
    #[cfg(target_arch = "x86_64")]
    #[inline]
    pub unsafe fn wrmsr(msr: u32, val: u64) {
        let lo = val as u32;
        let hi = (val >> 32) as u32;
        core::arch::asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") lo,
            in("edx") hi,
            options(nomem, nostack),
        );
    }
}

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

#[cfg(all(feature = "x86_64", target_arch = "x86_64"))]
pub fn halt() -> ! {
    loop {
        // SAFETY: `hlt` puts the CPU in a low-power wait state until the
        // next interrupt. In firmware context interrupts are typically
        // disabled, so this is effectively a permanent halt.
        unsafe { core::arch::asm!("hlt", options(nostack, nomem, preserves_flags)) };
    }
}

#[cfg(not(any(
    all(feature = "armv7", target_arch = "arm"),
    all(feature = "aarch64", target_arch = "aarch64"),
    all(feature = "riscv64", target_arch = "riscv64"),
    all(feature = "x86_64", target_arch = "x86_64"),
)))]
pub fn halt() -> ! {
    loop {
        core::hint::spin_loop();
    }
}
