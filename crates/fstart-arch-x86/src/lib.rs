//! x86/x86_64 architecture helpers.
//!
//! This crate contains concrete x86 CPU primitives. Cross-architecture traits
//! and generic firmware interfaces belong in `fstart-arch` / `fstart-services`;
//! x86-only implementation details such as MSRs and MTRRs live here.

#![no_std]

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub use x86;

/// Read the x86 Time Stamp Counter.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub fn rdtsc() -> u64 {
    // SAFETY: firmware runs at CPL0 and uses RDTSC only for delay loops.
    unsafe { x86::time::rdtsc() }
}

/// Delay for approximately `us` microseconds using the x86 TSC.
#[cfg(target_arch = "x86_64")]
pub fn udelay_tsc(us: u32, tsc_hz: u64) {
    let ticks = ((tsc_hz / 1_000_000).max(1)).saturating_mul(us as u64);
    let start = rdtsc();
    while rdtsc().wrapping_sub(start) < ticks {
        core::hint::spin_loop();
    }
}

#[cfg(target_arch = "x86_64")]
pub fn timestamp_us() -> u64 {
    rdtsc() / (tsc_frequency_hz() / 1_000_000).max(1)
}

#[cfg(target_arch = "x86_64")]
pub fn udelay(us: u32) {
    // Compute the TSC frequency once per delay.  `tsc_frequency_hz()` may use
    // CPUID/MSR reads on Core 2-era CPUs; doing that inside the polling loop is
    // both extremely slow and unsafe for early firmware delay paths.
    let hz = sanitize_tsc_frequency_hz(tsc_frequency_hz());
    udelay_tsc(us, hz);
}

#[cfg(target_arch = "x86_64")]
fn sanitize_tsc_frequency_hz(hz: u64) -> u64 {
    // Firmware delay loops must never turn into effectively infinite waits if
    // early CPU frequency discovery sees a bogus MSR/CPUID value.  Core 2 / X61
    // and the other x86 boards in this tree are comfortably inside this range.
    const MIN_TSC_HZ: u64 = 100_000_000;
    const MAX_TSC_HZ: u64 = 5_000_000_000;
    if (MIN_TSC_HZ..=MAX_TSC_HZ).contains(&hz) {
        hz
    } else {
        1_000_000_000
    }
}

#[cfg(target_arch = "x86_64")]
const MSR_FSB_FREQ: u32 = 0x00cd;
#[cfg(target_arch = "x86_64")]
const IA32_PERF_STATUS: u32 = 0x0198;

/// Best-effort TSC frequency for pre-Skylake firmware delays.
///
/// Core 2-era CPUs (GM965/X61) do not report CPUID.15h, so mirror coreboot's
/// `cpu/intel/common/fsb.c`: derive FSB MHz from `MSR_FSB_FREQ`, multiply by
/// the maximum bus ratio from `IA32_PERF_STATUS`, then round to the nearest
/// 100 MHz. Fall back to the old conservative 1 GHz default only for
/// unsupported CPUs.
#[cfg(target_arch = "x86_64")]
pub fn tsc_frequency_hz() -> u64 {
    if let Some(freq) = cpuid_tsc_frequency_hz() {
        return freq;
    }
    core2_tsc_frequency_hz().unwrap_or(1_000_000_000)
}

#[cfg(target_arch = "x86_64")]
fn cpuid_tsc_frequency_hz() -> Option<u64> {
    let (max_leaf, _, _, _) = cpuid(0);
    if max_leaf < 0x15 {
        return None;
    }
    let (denom, numer, crystal, _) = cpuid(0x15);
    if denom == 0 || numer == 0 || crystal == 0 {
        return None;
    }
    Some((crystal as u64).saturating_mul(numer as u64) / denom as u64)
}

#[cfg(target_arch = "x86_64")]
fn core2_tsc_frequency_hz() -> Option<u64> {
    let (eax, _, _, _) = cpuid(1);
    let family = ((eax >> 8) & 0x0f) + ((eax >> 20) & 0xff);
    let model = ((eax >> 4) & 0x0f) + ((eax >> 12) & 0xf0);
    if family != 6 || !(model == 0x0f || model == 0x17) {
        return None;
    }

    const CORE2_FSB_MHZ: [u32; 8] = [266, 133, 200, 166, 333, 100, 400, 0];
    let fsb_idx = unsafe { (x86::msr::rdmsr(MSR_FSB_FREQ) & 7) as usize };
    let fsb_mhz = CORE2_FSB_MHZ[fsb_idx];
    if fsb_mhz == 0 {
        return None;
    }
    let ratio = unsafe { ((x86::msr::rdmsr(IA32_PERF_STATUS) >> 40) & 0x1f) as u32 };
    if ratio == 0 {
        return None;
    }

    // coreboot: 100 * DIV_ROUND_CLOSEST(ratio * fsb, 100), in MHz.
    let raw_mhz = ratio.saturating_mul(fsb_mhz);
    let rounded_mhz = ((raw_mhz + 50) / 100) * 100;
    Some(rounded_mhz as u64 * 1_000_000)
}

/// Execute CPUID with ECX=0.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    cpuid_count(leaf, 0)
}

/// Execute CPUID with an explicit ECX subleaf.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn cpuid_count(leaf: u32, subleaf: u32) -> (u32, u32, u32, u32) {
    // CPUID is architectural on x86_64. The stdarch wrapper preserves RBX
    // correctly for LLVM's x86_64 code model.
    let result = core::arch::x86_64::__cpuid_count(leaf, subleaf);
    (result.eax, result.ebx, result.ecx, result.edx)
}

/// Return the CPU physical address width, falling back to 36 bits.
#[cfg(target_arch = "x86_64")]
pub fn physical_address_bits() -> u32 {
    let (max_ext_leaf, _, _, _) = cpuid(0x8000_0000);
    if max_ext_leaf < 0x8000_0008 {
        return 36;
    }
    let (eax, _, _, _) = cpuid(0x8000_0008);
    let bits = eax & 0xff;
    if bits == 0 {
        36
    } else {
        bits.min(52)
    }
}

/// Return the architectural MTRR physical address mask for this CPU.
#[cfg(target_arch = "x86_64")]
pub fn physical_address_mask() -> u64 {
    let bits = physical_address_bits();
    if bits >= 64 {
        !0xfffu64
    } else {
        ((1u64 << bits) - 1) & !0xfffu64
    }
}

/// Return the exclusive physical-address limit for this CPU.
#[cfg(target_arch = "x86_64")]
pub fn physical_address_limit() -> u64 {
    physical_address_mask().saturating_add(0x1000)
}

// ---------------------------------------------------------------------------
// x86 MTRR helpers
// ---------------------------------------------------------------------------

pub mod mtrr {
    use core::sync::atomic::{AtomicUsize, Ordering};

    use crate::x86::msr::{rdmsr, wrmsr};

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
    const RANGE_1M: u64 = 0x0010_0000;
    const RANGE_4G: u64 = 0x1_0000_0000;
    const MAX_WB_RANGES: usize = 8;

    #[derive(Clone, Copy)]
    struct WbRange {
        base: u64,
        size: u64,
    }

    static mut WB_RANGES: [WbRange; MAX_WB_RANGES] = [WbRange { base: 0, size: 0 }; MAX_WB_RANGES];
    static WB_RANGE_COUNT: AtomicUsize = AtomicUsize::new(0);

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
    /// power of two. The physical-address mask is derived from CPUID so Q35
    /// guests with RAM above Pineview's 36-bit limit are handled correctly.
    ///
    /// # Safety
    ///
    /// MTRR changes affect cacheability for physical memory. Caller must only
    /// use valid ranges and must arrange for all CPUs to use coherent MTRRs.
    pub unsafe fn set_variable(index: u32, base: u64, size: u64, ty: u64) {
        let phys_mask = crate::physical_address_mask();
        let base_msr = IA32_MTRR_PHYSBASE0 + index * 2;
        let mask_msr = IA32_MTRR_PHYSMASK0 + index * 2;
        let base_val = (base & phys_mask) | ty;
        let mask_val = ((!size.wrapping_sub(1)) & phys_mask) | MTRR_PHYSMASK_VALID;
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
    pub fn decode_base(base: u64) -> u64 {
        base & crate::physical_address_mask()
    }

    /// Decode the type from a variable MTRR base value.
    pub const fn decode_type(base: u64) -> u64 {
        base & 0xff
    }

    /// Decode the range size from a variable MTRR mask value.
    pub fn decode_size(mask: u64) -> u64 {
        let phys_mask = crate::physical_address_mask();
        (!(mask & phys_mask) & phys_mask).wrapping_add(0x1000)
    }

    /// Replace the RAM ranges that should become write-back cacheable.
    ///
    /// Ranges are consumed by [`setup_ram_wb`] on each CPU. Callers should pass
    /// physical RAM ranges in ascending order; zero-sized ranges are ignored.
    /// Ranges beyond the fixed storage limit are dropped.
    pub fn set_ram_wb_ranges(ranges: &[(u64, u64)]) {
        set_ram_wb_ranges_from(ranges.iter().copied());
    }

    /// Replace the RAM ranges from an iterator of `(base, size)` pairs.
    pub fn set_ram_wb_ranges_from<I>(ranges: I)
    where
        I: IntoIterator<Item = (u64, u64)>,
    {
        let mut out = 0usize;
        for (base, size) in ranges {
            if size == 0 || out >= MAX_WB_RANGES {
                continue;
            }
            // SAFETY: firmware populates this table during single-threaded
            // memory detection before MP CPU MTRR setup reads it.
            unsafe {
                WB_RANGES[out] = WbRange {
                    base: base & crate::physical_address_mask(),
                    size,
                };
            }
            out += 1;
        }
        let count = out;
        // SAFETY: clear stale entries after the new logical end.
        unsafe {
            while out < MAX_WB_RANGES {
                WB_RANGES[out] = WbRange { base: 0, size: 0 };
                out += 1;
            }
        }
        WB_RANGE_COUNT.store(count.min(MAX_WB_RANGES), Ordering::Release);
    }

    fn wb_range(index: usize) -> Option<WbRange> {
        if index >= WB_RANGE_COUNT.load(Ordering::Acquire).min(MAX_WB_RANGES) {
            return None;
        }
        // SAFETY: readers run after single-threaded publication and entries are
        // never mutated concurrently with MP MTRR setup.
        let range = unsafe { WB_RANGES[index] };
        (range.size != 0).then_some(range)
    }

    /// Disable normal caching on the current CPU by setting CR0.CD.
    ///
    /// # Safety
    /// Caller must use the architectural MTRR update sequence and restore
    /// caching before performance-sensitive code or OS handoff.
    #[cfg(target_arch = "x86_64")]
    pub unsafe fn disable_cache() {
        unsafe {
            let cr0 = x86::controlregs::cr0() | x86::controlregs::Cr0::CR0_CACHE_DISABLE;
            x86::controlregs::cr0_write(cr0);
            // x86 0.52 does not expose WBINVD, so keep the one instruction here.
            core::arch::asm!("wbinvd", options(nostack));
        }
    }

    /// Enable normal caching on the current CPU by clearing CR0.CD/NW.
    ///
    /// # Safety
    /// Caller must ensure MTRRs/PAT describe valid cacheability state.
    #[cfg(target_arch = "x86_64")]
    pub unsafe fn enable_cache() {
        unsafe {
            let cr0 = x86::controlregs::cr0()
                & !(x86::controlregs::Cr0::CR0_CACHE_DISABLE
                    | x86::controlregs::Cr0::CR0_NOT_WRITE_THROUGH);
            x86::controlregs::cr0_write(cr0);
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

    fn count_mtrr_chunks(mut base: u64, mut size: u64) -> u32 {
        let mut count = 0;
        while size != 0 {
            let chunk = largest_mtrr_chunk(base, size).max(0x1000);
            base += chunk;
            size -= chunk;
            count += 1;
        }
        count
    }

    fn optimized_hole_end(base: u64, end: u64, limit: u64, carve_hole: bool) -> u64 {
        let mut best_end = end;
        let mut best_count = count_mtrr_chunks(base, end - base);
        let first_align = end.trailing_zeros() + 1;
        let last_align = 63 - end.leading_zeros();
        for align in first_align..=last_align {
            let align_size = 1u64 << align;
            let hole_end = (end + align_size - 1) & !(align_size - 1);
            if hole_end > limit || hole_end <= end {
                break;
            }
            let mut count = count_mtrr_chunks(base, hole_end - base);
            if carve_hole {
                count += count_mtrr_chunks(end, hole_end - end);
            }
            if count < best_count {
                best_count = count;
                best_end = hole_end;
            }
        }
        best_end
    }

    unsafe fn set_range_chunks(index: &mut u32, limit: u32, mut base: u64, mut size: u64, ty: u64) {
        unsafe {
            while size != 0 && *index < limit {
                let chunk = largest_mtrr_chunk(base, size).max(0x1000);
                set_variable(*index, base, chunk, ty);
                base += chunk;
                size -= chunk;
                *index += 1;
            }
        }
    }

    /// Install fstart's RAM cacheability layout on this CPU.
    ///
    /// Variable MTRRs are derived from all published RAM ranges, including RAM
    /// above 4 GiB. The splitter follows coreboot's optimized UC-default MTRR
    /// strategy: naturally aligned power-of-two chunks, with optional top-hole
    /// alignment plus UC carve-outs when that reduces total variable MTRR use.
    ///
    /// # Safety
    /// Must be called on every active CPU during MP setup with identical
    /// published RAM ranges.
    pub unsafe fn setup_ram_wb() {
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

            let mut mtrr_index = 0u32;
            // Keep the last variable MTRR free for temporary BSP-only ROM WP.
            let usable_count = count.saturating_sub(1);
            let range_count = WB_RANGE_COUNT.load(Ordering::Acquire).min(MAX_WB_RANGES);
            for range_index in 0..range_count {
                let Some(range) = wb_range(range_index) else {
                    continue;
                };
                let mut base = range.base;
                let end = range
                    .base
                    .saturating_add(range.size)
                    .min(crate::physical_address_limit());
                if end <= RANGE_1M || end <= base {
                    continue;
                }
                if base <= RANGE_1M {
                    base = 0;
                }

                let next = if range_index + 1 < range_count {
                    wb_range(range_index + 1)
                } else {
                    None
                };
                let (limit, carve_hole) = if let Some(next) = next {
                    (next.base, true)
                } else if base < RANGE_4G {
                    let align = 1u64 << (63 - end.leading_zeros());
                    ((end + align - 1) & !(align - 1), true)
                } else {
                    (end, false)
                };
                let optimized_end = optimized_hole_end(base, end, limit, carve_hole);
                set_range_chunks(
                    &mut mtrr_index,
                    usable_count,
                    base,
                    optimized_end - base,
                    MTRR_TYPE_WRITE_BACK,
                );
                if carve_hole && optimized_end != end {
                    set_range_chunks(
                        &mut mtrr_index,
                        usable_count,
                        end,
                        optimized_end - end,
                        MTRR_TYPE_UNCACHEABLE,
                    );
                }
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
