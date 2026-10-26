//! Fixed-buffer, correctable-memory-only EINJ ABI shared by ACPI and SMM.
//!
//! Two pages at the unused end of the reserved early STORE region: the
//! first contains ACPI parameters/trigger instructions; the second is
//! disposable injection scratch. Neither is allocated to the OS or heap.
//! SMM never follows a pointer supplied through the shared page.

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const BASE: u64 = crate::layout::STORE.end() - 0x1_0000;
pub const SCRATCH: u64 = BASE + 0x1000;
pub const SCRATCH_SIZE: u64 = 0x1000;
pub const SUPPORTED: u64 = 0;
pub const TRIGGER_POINTER: u64 = 8;
pub const BUSY: u64 = 16;
pub const STATUS: u64 = 24;
pub const LEGACY_TYPE: u64 = 32;
pub const PARAMETERS: u64 = 40;
pub const TRIGGER: u64 = 128;
/// Backend diagnostic words (not input): enable/begin/setup/end status.
pub const BACKEND_STATUS: u64 = 192;
pub const SW_BEGIN: u8 = 0xd3;
pub const SW_END: u8 = 0xd4;
pub const SW_EXECUTE: u8 = 0xd5;
pub const MEMORY_CORRECTABLE: u32 = 8;

/// ACPI 6.5 §18.6.4 SET_ERROR_TYPE_WITH_ADDRESS. No nested pointers.
#[derive(Debug, Clone, Copy, Default, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C, packed)]
pub struct Parameters {
    pub error_type: u32,
    pub vendor_offset: u32,
    pub flags: u32,
    pub apic_id: u32,
    pub address: u64,
    pub mask: u64,
    pub pcie_sbdf: u32,
}

impl Parameters {
    /// Return one cache line in our scratch page. Reject other types,
    /// arbitrary addresses, vendor data, and masks wider than one page.
    pub fn target(self) -> Option<u64> {
        let granularity = !self.mask;
        let line = self.address & !63;
        (self.error_type == MEMORY_CORRECTABLE
            && self.vendor_offset == 0
            && self.flags == 2
            && granularity < SCRATCH_SIZE
            && granularity.wrapping_add(1).is_power_of_two()
            && (SCRATCH..SCRATCH + SCRATCH_SIZE).contains(&line))
        .then_some(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_disposable_correctable_memory_is_accepted() {
        let p = Parameters {
            error_type: 8,
            flags: 2,
            address: SCRATCH + 127,
            mask: !4095,
            ..Parameters::default()
        };
        assert_eq!(size_of::<Parameters>(), 36);
        assert_eq!(p.target(), Some(SCRATCH + 64));
        assert_eq!(
            Parameters {
                address: SCRATCH - 1,
                ..p
            }
            .target(),
            None
        );
        assert_eq!(
            Parameters {
                address: SCRATCH + SCRATCH_SIZE,
                ..p
            }
            .target(),
            None
        );
        for error_type in [0, 1, 0x10, 0x20, 0x8000_0008] {
            assert_eq!(Parameters { error_type, ..p }.target(), None);
        }
        for mask in [0, !8191, !5] {
            assert_eq!(Parameters { mask, ..p }.target(), None);
        }
        assert_eq!(
            Parameters {
                mask: u64::MAX,
                ..p
            }
            .target(),
            Some(SCRATCH + 64)
        );
        assert_eq!(Parameters { flags: 3, ..p }.target(), None);
        assert_eq!(
            Parameters {
                vendor_offset: 8,
                ..p
            }
            .target(),
            None
        );
        assert!(crate::layout::EARLY_REGION.contains(&crate::layout::Span::new(BASE, 0x2000)));
        assert!(!crate::layout::HEAP.overlaps(&crate::layout::Span::new(BASE, 0x2000)));
    }
}
