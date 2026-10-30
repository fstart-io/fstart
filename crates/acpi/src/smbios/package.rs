//! Types 4 and 7 of an x86 processor package from the CPUID of one of its
//! threads, and the legacy entry point. The vendor code supplies what
//! CPUID doesn't say: the family, the socket and the clocks.

use super::{ENTRY_POINT_SIZE, ProcessorInfo, SmbiosWriter};
use alloc::format;
use fstart_arch::x86::boot::cpuid::{CacheInfo, ProcessorSnapshot};
use fstart_core::smbios::CacheAssociativity;

/// One processor package.
pub struct Package<'a> {
    /// Type 4's socket designation; the type 7 designations start with it.
    pub socket: &'a str,
    /// CPUID of a thread of this package, not copied from the BSP's.
    pub cpu: &'a ProcessorSnapshot,
    /// The package's enabled threads. A cache counts once per sharing
    /// domain among them; with none, once.
    pub apic_ids: &'a [u32],
    pub cores: u16,
    pub enabled_cores: u16,
    pub threads: u16,
    /// SMBIOS processor family ("Processor Family 2").
    pub family: u16,
    /// SMBIOS processor upgrade (socket) code.
    pub upgrade: u8,
    /// MHz, 0 if unknown.
    pub external_clock_mhz: u16,
    pub max_speed_mhz: u16,
    pub current_speed_mhz: u16,
}

/// Processor characteristics (SMBIOS 3.x 7.5.9).
const SIXTY_FOUR_BIT: u16 = 1 << 2;
const MULTI_CORE: u16 = 1 << 3;
const HARDWARE_THREAD: u16 = 1 << 4;
const EXECUTE_PROTECTION: u16 = 1 << 5;
const ENHANCED_VIRTUALIZATION: u16 = 1 << 6;
const POWER_PERFORMANCE_CONTROL: u16 = 1 << 7;

impl Package<'_> {
    fn characteristics(&self) -> u16 {
        let cpu = self.cpu;
        [
            (cpu.extended_features & 1 << 29 != 0, SIXTY_FOUR_BIT),
            (self.cores > 1, MULTI_CORE),
            (self.threads > self.enabled_cores, HARDWARE_THREAD),
            (cpu.extended_features & 1 << 20 != 0, EXECUTE_PROTECTION),
            // VMX or SVM.
            (
                cpu.standard_features_ecx & 1 << 5 != 0 || cpu.extended_features_ecx & 1 << 2 != 0,
                ENHANCED_VIRTUALIZATION,
            ),
            // Enhanced SpeedStep.
            (
                cpu.standard_features_ecx & 1 << 7 != 0,
                POWER_PERFORMANCE_CONTROL,
            ),
        ]
        .into_iter()
        .filter(|(yes, _)| *yes)
        .fold(0, |bits, (_, bit)| bits | bit)
    }

    fn caches(&self) -> impl Iterator<Item = &CacheInfo> {
        self.cpu.caches[..self.cpu.cache_count.min(self.cpu.caches.len())]
            .iter()
            .filter(|cache| (1..=3).contains(&cache.level))
    }
}

impl SmbiosWriter<'_> {
    /// Type 7 for every cache of `package`, then its type 4.
    pub fn add_package(&mut self, package: &Package) {
        let mut caches = [0xffff; 3];
        for cache in package.caches() {
            let instances = cache.instance_count(package.apic_ids).max(1);
            let Ok(size_kib) = u32::try_from(u64::from(cache.size_bytes) / 1024 * instances as u64)
            else {
                continue;
            };
            if size_kib == 0 {
                continue;
            }
            let (kind, name) = match cache.cache_type {
                1 => (0x04, "Data"),
                2 => (0x03, "Instruction"),
                3 => (0x05, "Unified"),
                _ => (0x02, "Other"),
            };
            caches[usize::from(cache.level - 1)] = self.add_cache_info(
                &format!("{} L{} {}", package.socket, cache.level, name),
                cache.level,
                size_kib,
                CacheAssociativity::from_ways(cache.associativity, cache.fully_associative)
                    .to_smbios_byte(),
                kind,
            );
        }
        let cpu = package.cpu;
        self.add_processor(&ProcessorInfo {
            socket: package.socket,
            manufacturer: cpu.manufacturer(),
            version: cpu.brand(),
            family: package.family,
            id: u64::from(cpu.signature) | u64::from(cpu.standard_features) << 32,
            external_clock_mhz: package.external_clock_mhz,
            max_speed_mhz: package.max_speed_mhz,
            current_speed_mhz: package.current_speed_mhz,
            upgrade: package.upgrade,
            core_count: package.cores,
            core_enabled: package.enabled_cores,
            thread_count: package.threads,
            characteristics: package.characteristics(),
            caches,
        });
    }
}

/// Where an OS without EFI scans for the entry point: the F segment
/// (SMBIOS 3.x 5.2.2).
pub const LEGACY_ENTRY_POINT: u64 = 0xf_0000;

/// Copy the entry point at the start of `tables` (as [`super::assemble_into`]
/// wrote it) to [`LEGACY_ENTRY_POINT`].
///
/// # Safety
/// The F segment is DRAM, routed for writes, and reserved for the firmware
/// in the memory map the OS gets.
pub unsafe fn publish_legacy_entry_point(tables: &[u8]) {
    let entry_point = &tables[..ENTRY_POINT_SIZE];
    // SAFETY: the caller's.
    unsafe {
        core::ptr::copy_nonoverlapping(
            entry_point.as_ptr(),
            LEGACY_ENTRY_POINT as usize as *mut u8,
            ENTRY_POINT_SIZE,
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smbios::assemble_into;

    fn snapshot() -> ProcessorSnapshot {
        let cache = |level, cache_type, size_bytes, shared_logical| CacheInfo {
            level,
            cache_type,
            size_bytes,
            associativity: 8,
            line_size: 64,
            shared_logical,
            fully_associative: false,
        };
        let mut brand = [0; 48];
        brand[..4].copy_from_slice(b"Test");
        ProcessorSnapshot {
            vendor: *b"AuthenticAMD",
            brand,
            signature: 0x0080_0f12,
            standard_features: 0x178b_fbff,
            standard_features_ecx: 0,
            extended_features: 1 << 29 | 1 << 20,
            extended_features_ecx: 1 << 2,
            core_count: 4,
            caches: [
                cache(1, 1, 32 << 10, 2),
                cache(2, 3, 512 << 10, 2),
                cache(3, 3, 8 << 20, 8),
                CacheInfo::default(),
            ],
            cache_count: 3,
        }
    }

    fn package<'a>(cpu: &'a ProcessorSnapshot, apic_ids: &'a [u32]) -> Package<'a> {
        Package {
            socket: "Socket0",
            cpu,
            apic_ids,
            cores: 4,
            enabled_cores: 4,
            threads: 8,
            family: 0x6b,
            upgrade: 6,
            external_clock_mhz: 0,
            max_speed_mhz: 0,
            current_speed_mhz: 0,
        }
    }

    /// The installed sizes (KiB) of the type 7 structures.
    fn cache_sizes(bytes: &[u8]) -> alloc::vec::Vec<u32> {
        let mut sizes = alloc::vec::Vec::new();
        let mut at = ENTRY_POINT_SIZE;
        // Up to the zeroes after the last structure.
        while bytes[at + 1] != 0 {
            if bytes[at] == 7 {
                sizes.push(u32::from_le_bytes(
                    bytes[at + 0x17..at + 0x1b].try_into().unwrap(),
                ));
            }
            at += usize::from(bytes[at + 1]);
            while bytes[at..at + 2] != [0, 0] {
                at += 1;
            }
            at += 2;
        }
        sizes
    }

    #[test]
    fn caches_count_per_sharing_domain() {
        let cpu = snapshot();
        let mut bytes = [0u8; 2048];
        let ids: [u32; 8] = core::array::from_fn(|i| i as u32);
        assemble_into(&mut bytes, 0x1000, |w| w.add_package(&package(&cpu, &ids))).unwrap();
        // 4 L1D and L2 instances, one L3.
        assert_eq!(cache_sizes(&bytes), [128, 2048, 8192]);
        // Without the threads: one of each.
        assemble_into(&mut bytes, 0x1000, |w| w.add_package(&package(&cpu, &[]))).unwrap();
        assert_eq!(cache_sizes(&bytes), [32, 512, 8192]);
    }

    #[test]
    fn characteristics() {
        let cpu = snapshot();
        // 64-bit, multi-core, SMT, NX, SVM.
        assert_eq!(package(&cpu, &[]).characteristics(), 0x7c);
    }
}
