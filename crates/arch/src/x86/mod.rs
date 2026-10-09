//! x86/x86_64 architecture helpers.
//!
//! This crate contains concrete x86 CPU primitives. Cross-architecture traits
//! and generic firmware interfaces belong in `fstart-arch` / `fstart-core::services`;
//! x86-only implementation details such as MSRs and MTRRs live here.

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub use crate::x86_crate::*;

pub mod cpu;
pub mod lapic;
#[cfg(target_arch = "x86_64")]
pub mod legacy_pc;
pub mod mp;

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

/// Issue a 32-bit physical memory read without constructing a fabricated
/// Rust pointer. Pineview uses this as a DRAM command/read-training strobe
/// while memory is only partially initialized.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
pub unsafe fn read_phys32(addr: usize) {
    let value: u32;
    // SAFETY: caller guarantees `addr` is readable physical memory.
    unsafe {
        core::arch::asm!(
            "mov {value:e}, dword ptr [{addr}]",
            value = out(reg) value,
            addr = in(reg) addr,
            options(nostack, preserves_flags, readonly),
        );
    }
    core::hint::black_box(value);
}

/// Delay using the Intel-compatible HPET main counter.
///
/// The Pineview/ICH7 platform enables HPET before entering raminit. The
/// chipset's coreboot implementation uses 15 counter ticks per microsecond;
/// retain its wraparound-safe comparison here.
#[cfg(target_arch = "x86_64")]
pub fn hpet_udelay(us: u32) {
    const HPET_BASE: usize = 0xFED0_0000;
    const MAIN_COUNTER: usize = 0xF0;
    let delay = us.saturating_mul(15);
    let start = unsafe { fstart_core::mmio::read32((HPET_BASE + MAIN_COUNTER) as *const u32) };
    let finish = start.wrapping_add(delay);
    loop {
        let now = unsafe { fstart_core::mmio::read32((HPET_BASE + MAIN_COUNTER) as *const u32) };
        if if finish > start {
            now >= finish
        } else {
            now < start && now >= finish
        } {
            break;
        }
    }
}

#[cfg(target_arch = "x86_64")]
pub fn timestamp_us() -> u64 {
    rdtsc() / (tsc_frequency_hz() / 1_000_000).max(1)
}

// SMM builds substitute a call-free POST-port delay below so the handler does
// not retain the broad TSC-discovery call graph. Selected per build unit, so
// all other stages keep the precise TSC implementation unchanged.
#[cfg(all(target_arch = "x86_64", not(fstart_stage_env = "smm")))]
pub fn udelay(us: u32) {
    // Compute the TSC frequency once per delay.  `tsc_frequency_hz()` may use
    // CPUID/MSR reads on Core 2-era CPUs; doing that inside the polling loop is
    // both extremely slow and unsafe for early firmware delay paths.
    let hz = sanitize_tsc_frequency_hz(tsc_frequency_hz());
    udelay_tsc(us, hz);
}

/// SMM microsecond delay via POST-port writes (see above).
#[cfg(all(target_arch = "x86_64", fstart_stage_env = "smm"))]
#[inline(always)]
pub fn udelay(us: u32) {
    for _ in 0..us {
        // SAFETY: POST port 0x80 is always LPC-decoded; writes are side-effect
        // free and touch no memory, stack, or flags.
        unsafe {
            core::arch::asm!(
                "out dx, al",
                in("dx") 0x80u16,
                in("al") 0u8,
                options(nomem, nostack, preserves_flags),
            );
        }
    }
}

#[cfg(all(target_arch = "x86_64", not(fstart_stage_env = "smm")))]
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
    let (_, model) = family_model();
    if !(model == 0x0f || model == 0x17) {
        return None;
    }
    bus_clock().map(|clock| u64::from(clock.max_core_mhz()) * 1_000_000)
}

/// Display family and model from CPUID leaf 1.
#[cfg(target_arch = "x86_64")]
fn family_model() -> (u32, u32) {
    let (eax, _, _, _) = cpuid(1);
    let family = ((eax >> 8) & 0x0f) + ((eax >> 20) & 0xff);
    let model = ((eax >> 4) & 0x0f) + ((eax >> 12) & 0xf0);
    (family, model)
}

/// Front-side bus clock and maximum non-turbo bus ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BusClock {
    pub fsb_mhz: u32,
    pub max_ratio: u32,
}

impl BusClock {
    /// Maximum core clock; coreboot: `100 * DIV_ROUND_CLOSEST(ratio * fsb, 100)`.
    pub fn max_core_mhz(self) -> u32 {
        (self.max_ratio.saturating_mul(self.fsb_mhz) + 50) / 100 * 100
    }
}

/// FSB-era CPU clocks, mirroring coreboot `cpu/intel/common/fsb.c`. Newer
/// families report their clocks through CPUID and return `None`.
#[cfg(target_arch = "x86_64")]
pub fn bus_clock() -> Option<BusClock> {
    const CORE_FSB_MHZ: [u32; 8] = [0, 133, 0, 166, 0, 100, 0, 0];
    const CORE2_FSB_MHZ: [u32; 8] = [266, 133, 200, 166, 333, 100, 400, 0];
    let table = match family_model() {
        // Core Solo/Duo and Atom.
        (6, 0x0e | 0x1c) => &CORE_FSB_MHZ,
        // Core 2 and Enhanced Core.
        (6, 0x0f | 0x17) => &CORE2_FSB_MHZ,
        _ => return None,
    };
    // SAFETY: both MSRs exist on every model selected above.
    let (fsb_freq, perf_status) = unsafe {
        (
            x86::msr::rdmsr(MSR_FSB_FREQ),
            x86::msr::rdmsr(IA32_PERF_STATUS),
        )
    };
    let clock = BusClock {
        fsb_mhz: table[(fsb_freq & 7) as usize],
        max_ratio: ((perf_status >> 40) & 0x1f) as u32,
    };
    (clock.fsb_mhz != 0 && clock.max_ratio != 0).then_some(clock)
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
    if bits == 0 { 36 } else { bits.min(52) }
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

/// Write every cache line intersecting `addr..addr + len` back to memory.
///
/// APs leave INIT with normal caching disabled, so startup code and page tables
/// prepared by the BSP must reach DRAM before a SIPI. This mirrors coreboot's
/// `write_back_cached_data()` before it starts application processors.
///
/// # Safety
///
/// The range must be readable for `len` bytes. The caller must ensure no other
/// CPU concurrently mutates it until the returned writeback fence completes.
#[cfg(target_arch = "x86_64")]
pub unsafe fn writeback_cache_range(addr: *const u8, len: usize) {
    if len == 0 {
        return;
    }

    let (_, ebx, _, edx) = cpuid(1);
    if edx & (1 << 19) == 0 {
        // SAFETY: firmware runs at CPL0. The caller invokes this while normal
        // caching is enabled, so WBINVD is a safe conservative fallback.
        unsafe { writeback_invalidate_caches() };
        return;
    }

    let line_size = (((ebx >> 8) & 0xff) as usize * 8).max(8);
    let start = (addr as usize) & !(line_size - 1);
    let end = (addr as usize).saturating_add(len);
    let mut line = start;
    while line < end {
        // SAFETY: `line` intersects the caller-provided readable range and
        // CLFLUSH accepts any byte address within the cache line.
        unsafe {
            core::arch::asm!(
                "clflush [{line}]",
                line = in(reg) line,
                options(nostack, preserves_flags)
            )
        };
        line = line.saturating_add(line_size);
    }
    // Order all writebacks before publishing the range to another CPU.
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) };
}

/// Write back and invalidate all caches while ordinary RAM caching is enabled.
///
/// # Safety
/// CPL0, never CAR, and CR0.CD must be clear. Supported i945/Atom hardware can
/// hang on WBINVD with caching disabled; callers must not use it under CD=1.
#[cfg(target_arch = "x86_64")]
pub unsafe fn writeback_invalidate_caches() {
    unsafe { core::arch::asm!("wbinvd", options(nostack, preserves_flags)) };
}

// ---------------------------------------------------------------------------
// x86 MTRR helpers
// ---------------------------------------------------------------------------

pub mod mtrr;
