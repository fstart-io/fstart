//! Separate runtime mechanisms and boot-firmware memory ownership.
use super::{MemoryRegion, MemoryType, RuntimePlatformConfig};

pub(super) const EMPTY_REGION: MemoryRegion = MemoryRegion {
    base: 0,
    size: 0,
    region_type: MemoryType::Reserved,
};

pub(super) fn runtime_platform_config() -> RuntimePlatformConfig<'static> {
    #[cfg(target_arch = "x86_64")]
    let (time, reset, reset_port) = (
        crabefi::time_mechanism::X86_CMOS,
        crabefi::reset_mechanism::X86_LEGACY,
        0xcf9,
    );
    #[cfg(target_arch = "aarch64")]
    let (time, reset, reset_port) = (
        // No runtime-MMIO reservation API is available at the pinned revision.
        // Do not pass an RTC address the OS is not required to retain/map.
        crabefi::time_mechanism::UNSUPPORTED,
        // The fstart UEFI flow installs TF-A at EL3 before launching CrabEFI.
        crabefi::reset_mechanism::PSCI_SMC,
        0,
    );
    #[cfg(target_arch = "riscv64")]
    let (time, reset, reset_port) = (
        crabefi::time_mechanism::UNSUPPORTED,
        crabefi::reset_mechanism::SBI_SRST,
        0,
    );
    RuntimePlatformConfig {
        time: crabefi::RuntimeTimeConfig {
            mechanism: time,
            reserved: 0,
            io_or_mmio_base: 0,
        },
        reset: crabefi::RuntimeResetConfig {
            mechanism: reset,
            reserved: 0,
            io_or_mmio_base: reset_port,
        },
        external_ranges: &[],
        // Literal old library-mode behavior: no configured deferred buffer and
        // no platform-entry linker symbols. This is not a durable NV policy.
        deferred_buffer: crabefi::DeferredBufferConfig::disabled(),
    }
}

/// Reserve initialized image/rodata, copied data, and BSS/heap/stack. These are
/// boot-firmware reservations, not runtime code/data. CrabEFI allocates and
/// describes the separate runtime image itself.
pub(super) fn firmware_reservations() -> [MemoryRegion; 3] {
    unsafe extern "C" {
        static _text_start: u8;
        static _binary_end: u8;
        static _data_start: u8;
        static _data_end: u8;
        static _bss_start: u8;
        static _writable_end: u8;
    }
    regions_from_bounds([
        (
            core::ptr::addr_of!(_text_start) as u64,
            core::ptr::addr_of!(_binary_end) as u64,
        ),
        (
            core::ptr::addr_of!(_data_start) as u64,
            core::ptr::addr_of!(_data_end) as u64,
        ),
        (
            core::ptr::addr_of!(_bss_start) as u64,
            core::ptr::addr_of!(_writable_end) as u64,
        ),
    ])
}

fn regions_from_bounds(mut bounds: [(u64, u64); 3]) -> [MemoryRegion; 3] {
    bounds.sort_unstable_by_key(|&(base, _)| base);
    let mut regions = [EMPTY_REGION; 3];
    let mut count = 0;
    for (start, end) in bounds {
        assert!(end >= start, "reversed firmware memory bounds");
        if start == end {
            continue;
        }
        let base = start & !0xfff;
        let end = end.checked_add(0xfff).expect("firmware memory overflow") & !0xfff;
        if count != 0 {
            let previous = &mut regions[count - 1];
            let previous_end = previous.base + previous.size;
            if base <= previous_end {
                previous.size = end.max(previous_end) - previous.base;
                continue;
            }
        }
        regions[count] = MemoryRegion {
            base,
            size: end - base,
            region_type: MemoryType::Reserved,
        };
        count += 1;
    }
    regions
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    #[test]
    fn selected_runtime_bundle_is_v2_and_reports_its_effective_profile() {
        let source = crabefi::BUNDLED_RUNTIME_IMAGE;
        assert_eq!(&source.bytes[..8], b"CRABRTI\0");
        assert_eq!(
            u16::from_le_bytes(source.bytes[8..10].try_into().unwrap()),
            2
        );
        let bits = u64::from_le_bytes(source.bytes[48..56].try_into().unwrap());
        assert_eq!(bits & 15, 15);
        assert_eq!(bits & !31, 0);
        // The integration gate compares this with the effective Cargo graph,
        // not with the host's requested profile (Full legitimately unifies up).
        std::println!(
            "CRAB_RUNTIME_PROFILE bytes={} bits={} sha256={:02x?}",
            source.bytes.len(),
            bits,
            source.expected_sha256
        );
    }

    #[test]
    fn firmware_reservations_are_not_returned_as_allocatable_ram() {
        let reserved = regions_from_bounds([
            (0x4000000, 0x4008123),
            (0x4008100, 0x4008123),
            (0x4400000, 0x5000000),
        ]);
        let mut output = [EMPTY_REGION; 16];
        let ram = super::super::E820Entry {
            addr: 0x100000,
            size: 0x7f00000,
            kind: 1,
        };
        let count = super::super::build_efi_memory_map_from_e820(&[ram], &reserved, &mut output);
        assert!(
            output[..count]
                .iter()
                .filter(|r| r.region_type == MemoryType::Ram)
                .all(|r| {
                    reserved
                        .iter()
                        .filter(|s| s.size != 0)
                        .all(|s| r.base + r.size <= s.base || s.base + s.size <= r.base)
                })
        );
        assert!(
            output[..count]
                .iter()
                .any(|r| r.region_type == MemoryType::Ram
                    && r.base == 0x4009000
                    && r.base + r.size == 0x4400000)
        );
    }

    #[test]
    fn reserves_compact_ram_and_xip_without_covering_the_address_gap() {
        let spans = |bounds| regions_from_bounds(bounds).map(|r| (r.base, r.size));
        assert_eq!(
            spans([
                (0x4000000, 0x4008123),
                (0x4008100, 0x4008123),
                (0x4400000, 0x5000000)
            ]),
            [(0x4000000, 0x9000), (0x4400000, 0xc00000), (0, 0)]
        );
        assert_eq!(
            spans([
                (0xfff00000, 0xfff12345),
                (0x100000, 0x101234),
                (0x101234, 0x120000)
            ]),
            [(0x100000, 0x20000), (0xfff00000, 0x13000), (0, 0)]
        );
    }

    #[test]
    fn no_unbacked_runtime_mmio_or_implicit_retained_journal() {
        let config = runtime_platform_config();
        assert!(config.external_ranges.is_empty());
        assert_eq!(config.deferred_buffer.base, 0);
        assert_eq!(config.deferred_buffer.size, 0);
    }
}
