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
