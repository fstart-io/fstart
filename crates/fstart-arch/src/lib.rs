//! Architecture-specific utilities.
//!
//! Provides delay functions, processor halt, and other low-level operations
//! that vary between CPU architectures (ARMv7-A, AArch64, RISC-V, x86_64).

#![no_std]

// ---------------------------------------------------------------------------
// udelay — microsecond delay (generic spin loop)
// ---------------------------------------------------------------------------

#[cfg(all(
    not(all(feature = "x86_64", target_arch = "x86_64")),
    any(feature = "armv7", feature = "aarch64", feature = "riscv64")
))]
pub fn udelay(us: u32) {
    for _ in 0..us.saturating_mul(100) {
        core::hint::spin_loop();
    }
}

/// Read the x86 Time Stamp Counter.
#[cfg(all(feature = "x86_64", target_arch = "x86_64"))]
#[inline(always)]
pub fn rdtsc() -> u64 {
    let lo: u32;
    let hi: u32;
    // SAFETY: `rdtsc` reads the architectural time-stamp counter and has no
    // memory side effects. Firmware uses it only for delay loops.
    unsafe {
        core::arch::asm!(
            "rdtsc",
            out("eax") lo,
            out("edx") hi,
            options(nomem, nostack, preserves_flags),
        );
    }
    ((hi as u64) << 32) | lo as u64
}

/// Delay for approximately `us` microseconds using the x86 TSC.
#[cfg(all(feature = "x86_64", target_arch = "x86_64"))]
pub fn udelay_tsc(us: u32, tsc_hz: u64) {
    let ticks = ((tsc_hz / 1_000_000).max(1)).saturating_mul(us as u64);
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

#[cfg(all(feature = "x86_64", target_arch = "x86_64"))]
pub fn udelay(us: u32) {
    // Conservative default for early x86 firmware. Platform drivers with a
    // known clock should call `udelay_tsc()` directly.
    udelay_tsc(us, 1_000_000_000);
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
// x86 MTRR helpers
// ---------------------------------------------------------------------------

#[cfg(feature = "x86_64")]
pub mod mtrr {
    use core::sync::atomic::{AtomicU64, Ordering};

    use super::msr::{rdmsr, wrmsr};

    pub const IA32_MTRR_PHYSBASE0: u32 = 0x200;
    pub const IA32_MTRR_PHYSMASK0: u32 = 0x201;
    pub const IA32_MTRR_CAP: u32 = 0x0fe;
    pub const IA32_MTRR_DEF_TYPE: u32 = 0x2ff;
    pub const IA32_MTRR_FIX64K_00000: u32 = 0x250;
    pub const IA32_MTRR_FIX16K_80000: u32 = 0x258;
    pub const IA32_MTRR_FIX16K_A0000: u32 = 0x259;
    pub const IA32_MTRR_FIX4K_C0000: u32 = 0x268;

    pub const MTRR_TYPE_UNCACHEABLE: u64 = 0x00;
    pub const MTRR_TYPE_WRITE_PROTECT: u64 = 0x05;
    pub const MTRR_TYPE_WRITE_BACK: u64 = 0x06;
    const MTRR_DEF_TYPE_FIXED_ENABLE: u64 = 1 << 10;
    const MTRR_DEF_TYPE_ENABLE: u64 = 1 << 11;
    const MTRR_PHYSMASK_VALID: u64 = 1 << 11;
    const PHYS_MASK_36: u64 = 0x0000_000f_ffff_f000;
    static LOW_WB_TOP: AtomicU64 = AtomicU64::new(0);

    const fn repeat_type(ty: u64) -> u64 {
        ty | (ty << 8) | (ty << 16) | (ty << 24) | (ty << 32) | (ty << 40) | (ty << 48) | (ty << 56)
    }

    /// Return the number of variable MTRR pairs reported by IA32_MTRR_CAP.
    ///
    /// # Safety
    /// Caller must ensure the CPU supports MTRRs.
    pub unsafe fn variable_count() -> u32 {
        unsafe { (rdmsr(IA32_MTRR_CAP) & 0xff) as u32 }
    }

    /// Return whether fixed MTRRs are supported.
    ///
    /// # Safety
    /// Caller must ensure the CPU supports MTRRs.
    pub unsafe fn fixed_supported() -> bool {
        unsafe { (rdmsr(IA32_MTRR_CAP) & (1 << 8)) != 0 }
    }

    /// Program one variable MTRR on the current CPU.
    ///
    /// `base` and `size` must be naturally aligned, and `size` must be a
    /// power of two.  Pineview-class parts expose 36 physical address bits;
    /// this helper uses that mask width because fstart's current x86_64
    /// hardware support targets that generation.
    ///
    /// # Safety
    ///
    /// MTRR changes affect cacheability for physical memory. Caller must only
    /// use valid ranges and must arrange for all CPUs to use coherent MTRRs.
    pub unsafe fn set_variable(index: u32, base: u64, size: u64, ty: u64) {
        let base_msr = IA32_MTRR_PHYSBASE0 + index * 2;
        let mask_msr = IA32_MTRR_PHYSMASK0 + index * 2;
        let base_val = (base & PHYS_MASK_36) | ty;
        let mask_val = ((!size.wrapping_sub(1)) & PHYS_MASK_36) | MTRR_PHYSMASK_VALID;
        unsafe {
            wrmsr(base_msr, base_val);
            wrmsr(mask_msr, mask_val);
        }
    }

    /// Clear one variable MTRR on the current CPU.
    ///
    /// # Safety
    /// See [`set_variable`].
    pub unsafe fn clear_variable(index: u32) {
        let base_msr = IA32_MTRR_PHYSBASE0 + index * 2;
        let mask_msr = IA32_MTRR_PHYSMASK0 + index * 2;
        unsafe {
            wrmsr(mask_msr, 0);
            wrmsr(base_msr, 0);
        }
    }

    /// Read one variable MTRR on the current CPU.
    ///
    /// # Safety
    /// Caller must ensure `index` is valid for this CPU.
    pub unsafe fn read_variable(index: u32) -> (u64, u64) {
        let base_msr = IA32_MTRR_PHYSBASE0 + index * 2;
        let mask_msr = IA32_MTRR_PHYSMASK0 + index * 2;
        unsafe { (rdmsr(base_msr), rdmsr(mask_msr)) }
    }

    /// Decode whether a variable MTRR mask is valid.
    pub const fn is_valid_mask(mask: u64) -> bool {
        (mask & MTRR_PHYSMASK_VALID) != 0
    }

    /// Decode the range base from a variable MTRR base value.
    pub const fn decode_base(base: u64) -> u64 {
        base & PHYS_MASK_36
    }

    /// Decode the type from a variable MTRR base value.
    pub const fn decode_type(base: u64) -> u64 {
        base & 0xff
    }

    /// Decode the range size from a variable MTRR mask value.
    pub const fn decode_size(mask: u64) -> u64 {
        (!(mask & PHYS_MASK_36) & PHYS_MASK_36).wrapping_add(0x1000)
    }

    /// Set the trained low-memory top used by runtime MTRR setup.
    pub fn set_low_wb_top(top: u64) {
        LOW_WB_TOP.store(top, Ordering::Release);
    }

    fn low_wb_top() -> u64 {
        LOW_WB_TOP.load(Ordering::Acquire) & PHYS_MASK_36
    }

    /// Disable normal caching on the current CPU by setting CR0.CD.
    ///
    /// # Safety
    /// Caller must use the architectural MTRR update sequence and restore
    /// caching before performance-sensitive code or OS handoff.
    #[cfg(target_arch = "x86_64")]
    pub unsafe fn disable_cache() {
        unsafe {
            core::arch::asm!(
                "mov rax, cr0",
                "or rax, 0x40000000",
                "mov cr0, rax",
                "wbinvd",
                out("rax") _,
                options(nostack),
            );
        }
    }

    /// Enable normal caching on the current CPU by clearing CR0.CD/NW.
    ///
    /// # Safety
    /// Caller must ensure MTRRs/PAT describe valid cacheability state.
    #[cfg(target_arch = "x86_64")]
    pub unsafe fn enable_cache() {
        unsafe {
            core::arch::asm!(
                "mov rax, cr0",
                "and rax, 0xffffffff9fffffff",
                "mov cr0, rax",
                out("rax") _,
                options(nostack),
            );
        }
    }

    fn largest_mtrr_chunk(base: u64, remaining: u64) -> u64 {
        let max_by_remaining = 1u64 << (63 - remaining.leading_zeros());
        if base == 0 {
            return max_by_remaining;
        }
        let max_by_alignment = base & base.wrapping_neg();
        max_by_remaining.min(max_by_alignment)
    }

    /// Program fixed MTRRs for conventional low memory.
    ///
    /// 0x00000..0x9ffff is write-back RAM. 0xa0000..0xfffff remains
    /// uncacheable for VGA/MMIO/legacy ROM holes.
    ///
    /// # Safety
    /// Must be called on every active CPU with identical values.
    pub unsafe fn setup_fixed_low_memory() {
        unsafe {
            if !fixed_supported() {
                return;
            }
            wrmsr(IA32_MTRR_FIX64K_00000, repeat_type(MTRR_TYPE_WRITE_BACK));
            wrmsr(IA32_MTRR_FIX16K_80000, repeat_type(MTRR_TYPE_WRITE_BACK));
            wrmsr(IA32_MTRR_FIX16K_A0000, repeat_type(MTRR_TYPE_UNCACHEABLE));
            for msr in IA32_MTRR_FIX4K_C0000..=0x26f {
                wrmsr(msr, repeat_type(MTRR_TYPE_UNCACHEABLE));
            }
        }
    }

    /// Install fstart's ramstage cacheability layout on this CPU.
    ///
    /// The write-back range is derived from the chipset-published low DRAM
    /// top, then split into naturally aligned power-of-two MTRR chunks using
    /// the same approach as coreboot's early MTRR helper. No artificial 1 GiB
    /// ceiling or next-power-of-two expansion is applied.
    ///
    /// # Safety
    /// Must be called on every active CPU during MP setup.
    pub unsafe fn setup_low_1g_wb() {
        unsafe {
            disable_cache();
            let fixed = if fixed_supported() {
                MTRR_DEF_TYPE_FIXED_ENABLE
            } else {
                0
            };
            let def_type = rdmsr(IA32_MTRR_DEF_TYPE) | MTRR_DEF_TYPE_ENABLE | fixed;
            wrmsr(IA32_MTRR_DEF_TYPE, def_type & !MTRR_DEF_TYPE_ENABLE);
            setup_fixed_low_memory();

            let count = variable_count();
            for index in 0..count {
                clear_variable(index);
            }

            let mut base = 0u64;
            let top = low_wb_top();
            let mut index = 0u32;
            // Keep the last variable MTRR free for temporary BSP-only ROM WP.
            let usable_count = count.saturating_sub(1);
            while base < top && index < usable_count {
                let remaining = top - base;
                let size = largest_mtrr_chunk(base, remaining).max(0x1000);
                set_variable(index, base, size, MTRR_TYPE_WRITE_BACK);
                base += size;
                index += 1;
            }
            wrmsr(IA32_MTRR_DEF_TYPE, def_type);
            enable_cache();
        }
    }

    /// Install/remove the BSP-only 16 MiB top-of-4G flash ROM WP MTRR.
    ///
    /// # Safety
    /// Caller must ensure this is only used on the BSP and removed before OS
    /// handoff so AP MTRRs remain coherent with the BSP.
    pub unsafe fn set_boot_rom_wp(enable: bool) {
        unsafe {
            let index = variable_count().saturating_sub(1);
            if enable {
                set_variable(index, 0xff00_0000, 0x0100_0000, MTRR_TYPE_WRITE_PROTECT);
            } else {
                clear_variable(index);
            }
        }
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
