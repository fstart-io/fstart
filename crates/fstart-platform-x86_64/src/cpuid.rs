//! x86 CPUID intrinsics for hardware detection.
//!
//! Provides typed wrappers around the `cpuid` instruction for reading
//! processor identity, topology, frequency, and cache geometry at boot.
//! Used by `SmBiosPrepare` and MADT builders to fill runtime-detected
//! fields.

/// Raw CPUID result: EAX, EBX, ECX, EDX.
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuidResult {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

/// Execute the `cpuid` instruction with leaf `eax` and sub-leaf `ecx`.
#[cfg(target_arch = "x86_64")]
#[inline]
pub fn cpuid(leaf: u32, subleaf: u32) -> CpuidResult {
    let (eax, ebx, ecx, edx): (u32, u32, u32, u32);
    // SAFETY: cpuid is always available on x86_64.
    // We save/restore rbx because LLVM reserves it as a frame pointer.
    unsafe {
        core::arch::asm!(
            "mov {tmp}, rbx",
            "cpuid",
            "xchg {tmp}, rbx",
            tmp = out(reg) ebx,
            inout("eax") leaf => eax,
            inout("ecx") subleaf => ecx,
            out("edx") edx,
            options(nostack, preserves_flags),
        );
    }
    CpuidResult { eax, ebx, ecx, edx }
}

#[cfg(not(target_arch = "x86_64"))]
pub fn cpuid(_leaf: u32, _subleaf: u32) -> CpuidResult {
    CpuidResult::default()
}

/// Maximum supported standard CPUID leaf.
pub fn max_standard_leaf() -> u32 {
    cpuid(0, 0).eax
}

/// Maximum supported extended CPUID leaf.
pub fn max_extended_leaf() -> u32 {
    cpuid(0x8000_0000, 0).eax
}

/// Read the 12-character vendor string (e.g., "GenuineIntel").
pub fn vendor_string() -> [u8; 12] {
    let r = cpuid(0, 0);
    let mut s = [0u8; 12];
    s[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    s[4..8].copy_from_slice(&r.edx.to_le_bytes());
    s[8..12].copy_from_slice(&r.ecx.to_le_bytes());
    s
}

/// Read the 48-character brand string (leaves 0x80000002–0x80000004).
///
/// Returns `None` if extended leaves aren't supported.
pub fn brand_string() -> Option<[u8; 48]> {
    if max_extended_leaf() < 0x8000_0004 {
        return None;
    }
    let mut buf = [0u8; 48];
    for i in 0..3u32 {
        let r = cpuid(0x8000_0002 + i, 0);
        let off = (i as usize) * 16;
        buf[off..off + 4].copy_from_slice(&r.eax.to_le_bytes());
        buf[off + 4..off + 8].copy_from_slice(&r.ebx.to_le_bytes());
        buf[off + 8..off + 12].copy_from_slice(&r.ecx.to_le_bytes());
        buf[off + 12..off + 16].copy_from_slice(&r.edx.to_le_bytes());
    }
    Some(buf)
}

/// Processor family, model, stepping from leaf 1.
#[derive(Debug, Clone, Copy)]
pub struct CpuSignature {
    pub stepping: u8,
    pub model: u8,
    pub family: u8,
    pub ext_model: u8,
    pub ext_family: u8,
    /// Full display model = model + (ext_model << 4) for family 6/15.
    pub display_model: u16,
    /// Full display family = family + ext_family for family 15.
    pub display_family: u16,
}

/// Read CPU signature (family/model/stepping) from CPUID leaf 1.
pub fn cpu_signature() -> CpuSignature {
    let r = cpuid(1, 0);
    let stepping = (r.eax & 0xF) as u8;
    let model = ((r.eax >> 4) & 0xF) as u8;
    let family = ((r.eax >> 8) & 0xF) as u8;
    let ext_model = ((r.eax >> 16) & 0xF) as u8;
    let ext_family = ((r.eax >> 20) & 0xFF) as u8;

    let display_family = if family == 0xF {
        family as u16 + ext_family as u16
    } else {
        family as u16
    };
    let display_model = if family == 0x6 || family == 0xF {
        model as u16 + ((ext_model as u16) << 4)
    } else {
        model as u16
    };

    CpuSignature {
        stepping,
        model,
        family,
        ext_model,
        ext_family,
        display_model,
        display_family,
    }
}

/// Logical processor count and initial APIC ID from CPUID leaf 1.
#[derive(Debug, Clone, Copy)]
pub struct CpuTopology {
    /// Initial APIC ID of the current logical processor.
    pub initial_apic_id: u8,
    /// Maximum number of addressable logical processor IDs.
    pub max_logical: u8,
}

/// Read basic topology info from CPUID leaf 1.
pub fn cpu_topology() -> CpuTopology {
    let r = cpuid(1, 0);
    CpuTopology {
        initial_apic_id: ((r.ebx >> 24) & 0xFF) as u8,
        max_logical: ((r.ebx >> 16) & 0xFF) as u8,
    }
}

/// Read core and thread counts from CPUID leaf 4 (Intel).
///
/// Returns `(cores_per_package, threads_per_core)`.
/// Falls back to (1, 1) if leaf 4 is not supported.
pub fn core_thread_count() -> (u8, u8) {
    if max_standard_leaf() < 4 {
        return (1, 1);
    }
    let r = cpuid(4, 0);
    let max_cores = ((r.eax >> 26) & 0x3F) as u8 + 1;
    let max_threads_sharing = ((r.eax >> 14) & 0xFFF) as u8 + 1;
    let threads_per_core = if max_cores > 0 {
        max_threads_sharing / max_cores
    } else {
        1
    };
    (max_cores, threads_per_core.max(1))
}

/// Cache descriptor from CPUID leaf 4.
#[derive(Debug, Clone, Copy)]
pub struct CacheInfo {
    /// Cache level (1 = L1, 2 = L2, 3 = L3).
    pub level: u8,
    /// Cache type: 1 = data, 2 = instruction, 3 = unified.
    pub cache_type: u8,
    /// Total cache size in bytes.
    pub size_bytes: u32,
    /// Associativity (ways).
    pub associativity: u16,
    /// Line size in bytes.
    pub line_size: u16,
}

/// Enumerate caches via CPUID leaf 4.
///
/// Returns up to `N` cache descriptors.
pub fn enumerate_caches<const N: usize>() -> ([CacheInfo; N], usize) {
    let mut caches = [CacheInfo {
        level: 0,
        cache_type: 0,
        size_bytes: 0,
        associativity: 0,
        line_size: 0,
    }; N];
    let mut count = 0;

    if max_standard_leaf() < 4 {
        return (caches, 0);
    }

    for idx in 0..N as u32 {
        let r = cpuid(4, idx);
        let ctype = (r.eax & 0x1F) as u8;
        if ctype == 0 {
            break;
        }
        let level = ((r.eax >> 5) & 0x7) as u8;
        let assoc = ((r.ebx >> 22) & 0x3FF) as u16 + 1;
        let partitions = ((r.ebx >> 12) & 0x3FF) as u32 + 1;
        let line_size = (r.ebx & 0xFFF) as u16 + 1;
        let sets = r.ecx + 1;
        let size = (assoc as u32) * partitions * (line_size as u32) * sets;

        caches[count] = CacheInfo {
            level,
            cache_type: ctype,
            size_bytes: size,
            associativity: assoc,
            line_size,
        };
        count += 1;
    }

    (caches, count)
}

/// Read the TSC frequency hint from CPUID leaf 0x15 (if available).
///
/// Returns `Some(freq_hz)` on processors that report it, `None` otherwise.
pub fn tsc_frequency() -> Option<u64> {
    if max_standard_leaf() < 0x15 {
        return None;
    }
    let r = cpuid(0x15, 0);
    if r.eax == 0 || r.ebx == 0 {
        return None;
    }
    // ECX = nominal frequency of crystal clock in Hz (if non-zero).
    if r.ecx != 0 {
        Some((r.ecx as u64) * (r.ebx as u64) / (r.eax as u64))
    } else {
        None
    }
}
