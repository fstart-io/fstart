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
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[inline]
pub fn cpuid(leaf: u32, subleaf: u32) -> CpuidResult {
    let (eax, ebx, ecx, edx) = crate::x86::cpuid_count(leaf, subleaf);
    CpuidResult { eax, ebx, ecx, edx }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
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
    let (cores, threads) = cpu_core_thread_counts();
    // threads-per-core = threads / cores, preserving the legacy u8 API.
    let threads_per_core = if cores > 0 {
        (threads / cores).clamp(1, u8::MAX as u16) as u8
    } else {
        1
    };
    (
        cores.clamp(1, u8::MAX as u16) as u8,
        threads_per_core.max(1),
    )
}

/// Read total cores and threads per package, coreboot-style.
///
/// Prefers CPUID leaf 0xB (x2APIC topology) like coreboot's
/// `smbios_write_type4`: `cores = EBX(1)/EBX(0)`, `threads = EBX(1)`.
/// Falls back to deterministic cache leaf 4, then to leaf 1 `EBX[23:16]`
/// (maximum addressable logical processors). Bonnell/Pineview has no
/// leaf 0xB, so leaf 4 is what yields the correct 1C/2T there.
///
/// Returns `(cores_per_package, threads_per_package)`.
pub fn cpu_core_thread_counts() -> (u16, u16) {
    if max_standard_leaf() >= 0xb {
        let pkg = (cpuid(0xb, 1).ebx & 0xffff) as u16;
        let smt = (cpuid(0xb, 0).ebx & 0xffff) as u16;
        let threads = if pkg == 0 { smt.max(1) } else { pkg };
        let threads_per_core = if smt == 0 { 1 } else { smt };
        let cores = threads / threads_per_core.max(1);
        if threads > 0 && cores > 0 {
            return (cores, threads);
        }
    }
    if max_standard_leaf() >= 4 {
        let r = cpuid(4, 0);
        let max_cores = ((r.eax >> 26) & 0x3F) as u16 + 1;
        let max_sharing = ((r.eax >> 14) & 0xFFF) as u16 + 1;
        if max_cores > 0 && max_sharing > 0 {
            return (max_cores, max_sharing);
        }
    }
    let logical = cpu_topology().max_logical as u16;
    (logical.max(1), logical.max(1))
}

/// Deterministic cache descriptor (Intel leaf 4 or AMD leaf 8000_001Dh).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
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
    /// Maximum addressable logical IDs sharing this cache, not the number
    /// of currently enabled threads. Its rounded-up power of two defines
    /// the cache's APIC-ID domain, including holes from fused-off cores.
    pub shared_logical: u16,
    pub fully_associative: bool,
}

impl CacheInfo {
    /// Count distinct cache instances represented by enabled APIC IDs in a
    /// homogeneous package. Does not multiply one shared L3 by every core.
    pub fn instance_count(&self, apic_ids: &[u32]) -> usize {
        let shift = u32::from(self.shared_logical.max(1))
            .next_power_of_two()
            .trailing_zeros();
        apic_ids
            .iter()
            .enumerate()
            .filter(|(index, id)| {
                let domain = **id >> shift;
                !apic_ids[..*index]
                    .iter()
                    .any(|prior| *prior >> shift == domain)
            })
            .count()
    }
}

/// Enumerate deterministic caches on the executing CPU.
///
/// Returns up to `N` cache descriptors.
pub fn enumerate_caches<const N: usize>() -> ([CacheInfo; N], usize) {
    enumerate_caches_with(cpuid)
}

fn enumerate_caches_with<const N: usize>(
    query: impl Fn(u32, u32) -> CpuidResult,
) -> ([CacheInfo; N], usize) {
    let mut caches = [CacheInfo::default(); N];
    let mut count = 0;
    let standard = query(0, 0);
    let vendor = [
        standard.ebx.to_le_bytes(),
        standard.edx.to_le_bytes(),
        standard.ecx.to_le_bytes(),
    ];
    let amd = vendor == [*b"Auth", *b"enti", *b"cAMD"];
    let extended = query(0x8000_0000, 0).eax;
    let leaf = if amd && extended >= 0x8000_001d && query(0x8000_0001, 0).ecx & (1 << 22) != 0 {
        0x8000_001d
    } else if standard.eax >= 4 {
        4
    } else {
        return (caches, 0);
    };
    for idx in 0..N as u32 {
        let r = query(leaf, idx);
        let ctype = (r.eax & 0x1F) as u8;
        if ctype == 0 {
            break;
        }
        let level = ((r.eax >> 5) & 0x7) as u8;
        let assoc = ((r.ebx >> 22) & 0x3FF) as u16 + 1;
        let partitions = ((r.ebx >> 12) & 0x3FF) + 1;
        let line_size = (r.ebx & 0xFFF) as u16 + 1;
        let sets = u64::from(r.ecx) + 1;
        let Some(size) = u64::from(assoc)
            .checked_mul(u64::from(partitions))
            .and_then(|v| v.checked_mul(u64::from(line_size)))
            .and_then(|v| v.checked_mul(sets))
            .and_then(|v| u32::try_from(v).ok())
        else {
            break;
        };
        caches[count] = CacheInfo {
            level,
            cache_type: ctype,
            size_bytes: size,
            associativity: assoc,
            line_size,
            shared_logical: ((r.eax >> 14) & 0xfff) as u16 + 1,
            fully_associative: r.eax & (1 << 9) != 0,
        };
        count += 1;
    }

    (caches, count)
}

/// Package identity and per-cache geometry captured on an executing CPU.
/// No SMBIOS encoding or board/socket labels belong in this architectural record.
#[derive(Debug, Clone, Copy)]
pub struct ProcessorSnapshot {
    pub vendor: [u8; 12],
    pub brand: [u8; 48],
    pub signature: u32,
    pub standard_features: u32,
    pub standard_features_ecx: u32,
    pub extended_features: u32,
    pub extended_features_ecx: u32,
    pub core_count: u16,
    pub caches: [CacheInfo; 4],
    pub cache_count: usize,
}

impl ProcessorSnapshot {
    pub fn read() -> Self {
        let leaf1 = cpuid(1, 0);
        let ext1 = if max_extended_leaf() >= 0x8000_0001 {
            cpuid(0x8000_0001, 0)
        } else {
            CpuidResult::default()
        };
        let (caches, cache_count) = enumerate_caches();
        Self {
            vendor: vendor_string(),
            brand: brand_string().unwrap_or([0; 48]),
            signature: leaf1.eax,
            standard_features: leaf1.edx,
            standard_features_ecx: leaf1.ecx,
            extended_features: ext1.edx,
            extended_features_ecx: ext1.ecx,
            core_count: cpu_core_thread_counts().0,
            caches,
            cache_count,
        }
    }
    pub fn manufacturer(&self) -> &str {
        core::str::from_utf8(&self.vendor).unwrap_or("")
    }
    pub fn brand(&self) -> &str {
        let end = self
            .brand
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(self.brand.len());
        core::str::from_utf8(&self.brand[..end])
            .unwrap_or("")
            .trim_ascii()
    }
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

#[cfg(test)]
mod cache_tests {
    use super::*;

    fn query(amd: bool, leaf: u32, sub: u32) -> CpuidResult {
        match leaf {
            0 => CpuidResult {
                eax: 4,
                ebx: u32::from_le_bytes(if amd { *b"Auth" } else { *b"Genu" }),
                edx: u32::from_le_bytes(if amd { *b"enti" } else { *b"ineI" }),
                ecx: u32::from_le_bytes(if amd { *b"cAMD" } else { *b"ntel" }),
            },
            0x8000_0000 => CpuidResult {
                eax: 0x8000_001d,
                ..Default::default()
            },
            0x8000_0001 => CpuidResult {
                ecx: 1 << 22,
                ..Default::default()
            },
            4 | 0x8000_001d if sub == 0 => {
                assert_eq!(leaf, if amd { 0x8000_001d } else { 4 });
                CpuidResult {
                    eax: 3 | (3 << 5) | (15 << 14),
                    ebx: (31 << 22) | 63,
                    ecx: 16383,
                    edx: 0,
                }
            }
            _ => CpuidResult::default(),
        }
    }
    #[test]
    fn intel_and_amd_leaf_selection_and_cache_size() {
        for amd in [false, true] {
            let (caches, count) = enumerate_caches_with::<4>(|leaf, sub| query(amd, leaf, sub));
            assert_eq!(count, 1);
            assert_eq!(caches[0].size_bytes, 32 * 1024 * 1024);
            assert_eq!(caches[0].shared_logical, 16);
            assert_eq!(caches[0].associativity, 32);
        }
    }
    #[test]
    fn brand_terminates_at_the_first_nul() {
        let mut cpu = ProcessorSnapshot::read();
        cpu.brand.fill(b' ');
        cpu.brand[..8].copy_from_slice(b"AMD\0junk");
        assert_eq!(cpu.brand(), "AMD");
    }

    #[test]
    fn sharing_domains_include_fused_core_holes() {
        let (caches, _) = enumerate_caches_with::<4>(|leaf, sub| query(true, leaf, sub));
        // Four active threads in each of two distinct 16-ID cache domains.
        assert_eq!(caches[0].instance_count(&[0, 1, 8, 9, 16, 17, 28, 29]), 2);
        assert_eq!(caches[0].instance_count(&[]), 0);
        let private = CacheInfo {
            shared_logical: 2,
            ..caches[0]
        };
        assert_eq!(private.instance_count(&[0, 1, 8, 9, 16, 17, 28, 29]), 4);
        let non_power = CacheInfo {
            shared_logical: 12,
            ..caches[0]
        };
        assert_eq!(non_power.instance_count(&[0, 1, 16, 17]), 2);
    }
}
