// SPDX-License-Identifier: MIT OR Apache-2.0
//! Checked Rs runtime reservations, not a final PCI/OS resource plan.
use core::ops::Range;
use fstart_core::services::ServiceError;
use fstart_driver_amd::apob::sysmap::{Region, RegionKind};
pub const USB_CONTROLLER_MMIO_SIZE: u64 = 0x100_0000;
pub const USB_MMIO_SIZE: u64 = 2 * USB_CONTROLLER_MMIO_SIZE;
/// Separate non-PCI slots; each contains one 512-KiB IOMMU register window.
pub const IOMMU_SLOT_SIZE: u64 = 0x10_0000;
/// Where an NBIO IOAPIC lives inside its root's IOMMU slot: above the
/// IOMMU's 512-KiB register aperture, so the slot's one DF4 rule routes both.
pub const IOAPIC_SLOT_OFFSET: u64 = fstart_driver_amd::nbio::iommu::MMIO_SIZE;

/// The IOAPIC register window of the `index`th root, in the upper half of
/// that root's IOMMU slot from the `roots`-slot reservation `iommus`.
pub fn ioapic_mmio(iommus: &Range<u64>, roots: usize, index: usize) -> Option<u64> {
    if !(1..=4).contains(&roots)
        || index >= roots
        || iommus.end.checked_sub(iommus.start)? != IOMMU_SLOT_SIZE.checked_mul(roots as u64)?
        || !iommus.start.is_multiple_of(IOMMU_SLOT_SIZE)
    {
        return None;
    }
    iommus
        .start
        .checked_add(index as u64 * IOMMU_SLOT_SIZE + IOAPIC_SLOT_OFFSET)
}

fn low_mmio_covers(regions: &[Region], range: Range<u64>) -> bool {
    range.start >= 0x8000_0000
        && range.start < range.end
        && range.end <= 0xe000_0000
        && regions.iter().any(|r| {
            r.kind == RegionKind::Mmio
                && r.base <= range.start
                && r.base
                    .checked_add(r.size)
                    .is_some_and(|end| range.end <= end)
        })
}

/// Non-PCI IOMMU slots follow the USB and PCI pools in the SAME PSP MMIO
/// hole. They are never passed to the PCI BAR allocator as producer space.
pub fn iommu_mmio(
    regions: &[Region],
    usb: &Range<u64>,
    roots: usize,
    root_size: u64,
) -> Option<Range<u64>> {
    let pci = pcie_mmio(regions, usb, roots, root_size)?;
    let end = pci
        .end
        .checked_add(IOMMU_SLOT_SIZE.checked_mul(roots as u64)?)?;
    low_mmio_covers(regions, usb.start..end).then_some(pci.end..end)
}

/// The primary PSP's 1-MiB register window: the non-PCI slot after the
/// IOMMU slots (NFX maps it beside its IOMMU in the primary IOS's MMIO).
pub fn psp_mmio(regions: &[Region], iommus: &Range<u64>) -> Option<Range<u64>> {
    use fstart_driver_amd::nbio::iohc::PSP_BAR_SIZE;
    let start = iommus.end.checked_next_multiple_of(PSP_BAR_SIZE)?;
    let end = start.checked_add(PSP_BAR_SIZE)?;
    low_mmio_covers(regions, start..end).then_some(start..end)
}

/// Disjoint naturally aligned low-MMIO pools after checked USB. Budget is
/// explicit platform policy, not a discovered BAR demand. Never merge holes.
pub fn pcie_mmio(
    regions: &[Region],
    usb: &Range<u64>,
    roots: usize,
    root_size: u64,
) -> Option<Range<u64>> {
    if !(1..=fstart_pci::MAX_PCI_ROOTS).contains(&roots)
        || !root_size.is_power_of_two()
        || root_size < 0x10000
        || usb_mmio(regions).as_ref() != Some(usb)
    {
        return None;
    }
    let start = usb.end.checked_next_multiple_of(root_size)?;
    let end = start.checked_add(root_size.checked_mul(roots as u64)?)?;
    low_mmio_covers(regions, usb.start..end).then_some(start..end)
}

/// Where above-4-GiB PCI pools start: 1 TiB, as AGESA places them, clear of
/// DRAM and of the reserved HyperTransport hole just below 1 TiB.
pub const MMIO64_BASE: u64 = 1 << 40;

/// The CPU's physical address width for MMIO: CPUID 8000_0008h EAX[7:0]
/// less the memory-encryption reduction of 8000_001Fh EBX[11:6].
#[cfg(target_arch = "x86_64")]
pub fn physical_address_bits() -> u8 {
    let bits = fstart_arch::x86::cpuid(0x8000_0008).0 & 0xff;
    let reduction = if fstart_arch::x86::cpuid(0x8000_0000).0 >= 0x8000_001f {
        (fstart_arch::x86::cpuid(0x8000_001f).1 >> 6) & 0x3f
    } else {
        0
    };
    bits.saturating_sub(reduction) as u8
}

/// One naturally aligned above-4-GiB pool per root, from [`MMIO64_BASE`]
/// (or the first aligned address above every PSP region), inside the CPU's
/// `phys_bits` address space. Budget is explicit platform policy.
pub fn pcie_mmio64(
    regions: &[Region],
    roots: usize,
    root_size: u64,
    phys_bits: u8,
) -> Option<Range<u64>> {
    if !(1..=fstart_pci::MAX_PCI_ROOTS).contains(&roots)
        || !root_size.is_power_of_two()
        || root_size < 1 << 32
        || !(41..=52).contains(&phys_bits)
    {
        return None;
    }
    let top = regions
        .iter()
        .map(|r| r.base.checked_add(r.size))
        .try_fold(0u64, |top, end| Some(top.max(end?)))?;
    let start = top.max(MMIO64_BASE).checked_next_multiple_of(root_size)?;
    let end = start.checked_add(root_size.checked_mul(roots as u64)?)?;
    (end <= 1 << phys_bits).then_some(start..end)
}

/// Select one aligned reservation from a PSP-declared MMIO hole, never RAM,
/// ECAM or the fixed FCH/APIC apertures. DF routing must be checked separately.
pub fn usb_mmio(regions: &[Region]) -> Option<Range<u64>> {
    regions
        .iter()
        .filter(|r| r.kind == RegionKind::Mmio)
        .find_map(|r| {
            let base = r
                .base
                .max(0x8000_0000)
                .checked_next_multiple_of(USB_MMIO_SIZE)?;
            let end = base.checked_add(USB_MMIO_SIZE)?;
            (end <= r.base.checked_add(r.size)?.min(0xe000_0000)).then_some(base..end)
        })
}

/// Require a whole pre-link/runtime reservation inside one PSP RAM span.
/// This does not confer final OS ownership or erase PSP/security reservations.
pub fn ram_covers(regions: &[Region], range: Range<u64>) -> bool {
    range.start < range.end
        && regions.iter().any(|r| {
            r.kind.is_ram()
                && r.base <= range.start
                && r.base
                    .checked_add(r.size)
                    .is_some_and(|end| range.end <= end)
        })
}

/// Final OS map from the checked Rs region list: unknown PSP holes remain
/// reserved, then carve PC legacy/scratch areas and every live firmware/DMA
/// reservation. Known MMIO holes are absent, NEVER RAM: ACPI describes their
/// producer/device ownership. Marking a PCI aperture E820_RESERVED makes Linux
/// exclude it from PCI BAR allocation even when the root's _CRS offers it.
/// No inferred RAM, TSEG allocation or recovery-image addresses.
pub fn os_memory_map(
    regions: &[Region],
    reserved: &[Range<u64>],
) -> Result<fstart_core::services::memory_detect::E820State, ServiceError> {
    use fstart_core::services::memory_detect::{E820Entry, E820Kind, E820State};
    let mut map = E820State::new();
    if regions.is_empty() || regions.len() > map.entries_mut().len() {
        return Err(ServiceError::InvalidParam);
    }
    let mut end = 0;
    let mut count = 0;
    for region in regions {
        if region.size == 0 || region.base < end {
            return Err(ServiceError::InvalidParam);
        }
        end = region
            .base
            .checked_add(region.size)
            .ok_or(ServiceError::InvalidParam)?;
        if region.kind == RegionKind::Mmio {
            continue;
        }
        map.entries_mut()[count] = E820Entry::new(
            region.base,
            region.size,
            if region.kind.is_ram() {
                E820Kind::Ram
            } else {
                E820Kind::Reserved
            },
        );
        count += 1;
    }
    if count == 0 {
        return Err(ServiceError::InvalidParam);
    }
    map.set_detected(count, 0)?;
    // IVT/BDA and architectural AP/S3 scratch, then EBDA/VGA/legacy ROM.
    for range in [0..0x10000, 0x9f000..0x100000].iter().chain(reserved) {
        let size = range
            .end
            .checked_sub(range.start)
            .filter(|n| *n > 0)
            .ok_or(ServiceError::InvalidParam)?;
        map.reserve_range(range.start, size)?;
    }
    let total = map
        .entries()
        .iter()
        .filter(|e| e.kind == E820Kind::Ram as u32)
        .map(|e| e.size)
        .sum();
    map.set_detected(map.count(), total)?;
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn os_map_preserves_measured_siena_holes_and_firmware_not_pci_apertures() {
        use fstart_driver_amd::apob::{
            rs::Rs,
            sysmap::{self, Hole},
        };
        let holes = [
            Hole {
                base: 0xb0000000,
                size: 0x50000000,
                kind: 1,
            },
            Hole {
                base: 0x604d8c0000,
                size: 0x1040000,
                kind: 11,
            },
            Hole {
                base: 0x604e900000,
                size: 0x100000,
                kind: 15,
            },
            Hole {
                base: 0x604ea00000,
                size: 0x200000,
                kind: 14,
            },
            Hole {
                base: 0x604ec00000,
                size: 0x1400000,
                kind: 2,
            },
        ];
        let regions = sysmap::memory_map(Rs, 0x6050000000, holes).unwrap();
        // S8040 runtime: cache window and the Linux boot parameters.
        let cache = 0x7500_0000..0x7600_0000;
        let params = 0x90000..0x92000;
        let map = os_memory_map(&regions, &[cache.clone(), params.clone()]).unwrap();
        assert!(map.total_ram() > 0x5f00000000);
        for (base, end) in [
            (0, 0x10000),
            (0x9f000, 0x100000),
            (cache.start, cache.end),
            (params.start, params.end),
            (0x604d8c0000, 0x6050000000),
        ] {
            assert!(!map.entries().iter().any(|e| e.kind
                == fstart_core::services::memory_detect::E820Kind::Ram as u32
                && e.addr < end
                && base < e.addr + e.size));
            assert!(map.entries().iter().any(|e| e.kind
                == fstart_core::services::memory_detect::E820Kind::Reserved as u32
                && e.addr <= base
                && e.addr + e.size >= end));
        }
        assert!(
            !map.entries()
                .iter()
                .any(|e| e.addr < 0xe0000000 && 0xb0000000 < e.addr + e.size)
        );
        assert!(map.entries().iter().any(|e| e.kind
            == fstart_core::services::memory_detect::E820Kind::Ram as u32
            && e.addr == 0x100000000
            && e.addr + e.size == 0x604d8c0000));
        for pair in map.entries().windows(2) {
            assert!(pair[0].addr + pair[0].size <= pair[1].addr);
        }
    }

    #[test]
    fn mmio64_pools_start_at_one_tib_above_every_region() {
        let ram = Region {
            base: 0x1_0000_0000,
            size: 0x5f_4d8c_0000,
            kind: RegionKind::Ram,
        };
        assert_eq!(
            pcie_mmio64(&[ram], 3, 1 << 38, 52),
            Some(0x100_0000_0000..0x100_0000_0000 + 3 * (1 << 38))
        );
        let huge = Region {
            base: 0x100_0000_0000,
            size: 0x10_0000,
            kind: RegionKind::Reserved(11),
        };
        assert_eq!(
            pcie_mmio64(&[ram, huge], 1, 1 << 38, 52),
            Some(0x140_0000_0000..0x180_0000_0000)
        );
        assert_eq!(pcie_mmio64(&[ram], 4, 1 << 40, 42), None);
        assert_eq!(pcie_mmio64(&[ram], 1, 0x1000_0000, 52), None);
        assert_eq!(pcie_mmio64(&[ram], 0, 1 << 38, 52), None);
    }
    #[test]
    fn ioapics_share_their_iommu_slot_above_its_register_aperture() {
        let iommus = 0xd8000000..0xd8400000;
        assert_eq!(ioapic_mmio(&iommus, 4, 0), Some(0xd8080000));
        assert_eq!(ioapic_mmio(&iommus, 4, 3), Some(0xd8380000));
        assert_eq!(ioapic_mmio(&iommus, 4, 4), None);
        assert_eq!(ioapic_mmio(&iommus, 3, 0), None);
        assert_eq!(ioapic_mmio(&(0xd8000001..0xd8400001), 4, 0), None);
        assert_eq!(
            ioapic_mmio(&(0xd8000000..0xd8300000), 3, 2),
            Some(0xd8280000)
        );
    }
    #[test]
    fn usb_reservation_is_complete_aligned_and_excludes_fixed_windows() {
        let r = |base, size, kind| Region { base, size, kind };
        assert_eq!(
            usb_mmio(&[r(0xb0000000, 0x50000000, RegionKind::Mmio)]),
            Some(0xb0000000..0xb2000000)
        );
        assert_eq!(
            usb_mmio(&[r(0xb1000000, 0x5000000, RegionKind::Mmio)]),
            Some(0xb2000000..0xb4000000)
        );
        assert_eq!(
            usb_mmio(&[r(0xb0000000, 0x1000000, RegionKind::Mmio)]),
            None
        );
        assert_eq!(
            usb_mmio(&[r(0xe0000000, 0x20000000, RegionKind::Mmio)]),
            None
        );
        assert_eq!(
            usb_mmio(&[r(0x80000000, 0x60000000, RegionKind::Ram)]),
            None
        );
        assert_eq!(
            usb_mmio(&[r(u64::MAX - 0x100000, 0x200000, RegionKind::Mmio)]),
            None
        );
    }
    #[test]
    fn pci_pools_are_bounded_disjoint_from_usb_and_inside_one_mmio_hole() {
        let region = Region {
            base: 0xb0000000,
            size: 0x50000000,
            kind: RegionKind::Mmio,
        };
        let usb = usb_mmio(&[region]).unwrap();
        assert_eq!(
            pcie_mmio(&[region], &usb, 4, 0x1000000),
            Some(0xb2000000..0xb6000000)
        );
        assert_eq!(pcie_mmio(&[region], &usb, 0, 0x1000000), None);
        assert_eq!(pcie_mmio(&[region], &usb, 9, 0x1000000), None);
        assert_eq!(
            pcie_mmio(&[region], &(0xb1000000..0xb3000000), 4, 0x1000000),
            None
        );
        assert_eq!(
            pcie_mmio(
                &[Region {
                    size: 0x3000000,
                    ..region
                }],
                &usb,
                4,
                0x1000000
            ),
            None
        );
        assert_eq!(
            pcie_mmio(
                &[Region {
                    kind: RegionKind::Ram,
                    ..region
                }],
                &usb,
                4,
                0x1000000
            ),
            None
        );
    }

    #[test]
    fn iommu_slots_are_non_pci_and_require_one_complete_psp_hole() {
        let r = Region {
            base: 0xb0000000,
            size: 0x6400000,
            kind: RegionKind::Mmio,
        };
        let usb = usb_mmio(&[r]).unwrap();
        assert_eq!(
            iommu_mmio(&[r], &usb, 4, 0x1000000),
            Some(0xb6000000..0xb6400000)
        );
        assert_eq!(
            iommu_mmio(
                &[Region {
                    size: r.size - 0x10000,
                    ..r
                }],
                &usb,
                4,
                0x1000000
            ),
            None
        );
        assert_eq!(
            iommu_mmio(
                &[
                    Region {
                        size: 0x6000000,
                        ..r
                    },
                    Region {
                        base: 0xb6000000,
                        size: 0x400000,
                        ..r
                    }
                ],
                &usb,
                4,
                0x1000000
            ),
            None
        );
        assert_eq!(iommu_mmio(&[r], &usb, 0, 0x1000000), None);
        assert_eq!(iommu_mmio(&[r], &usb, usize::MAX, 0x1000000), None);
        assert_eq!(
            iommu_mmio(
                &[Region {
                    kind: RegionKind::Ram,
                    ..r
                }],
                &usb,
                4,
                0x1000000
            ),
            None
        );
    }

    #[test]
    fn larger_explicit_gpu_budget_aligns_pools_and_reserves_the_gap() {
        let region = Region {
            base: 0xb0000000,
            size: 0x50000000,
            kind: RegionKind::Mmio,
        };
        let usb = usb_mmio(&[region]).unwrap();
        assert_eq!(
            pcie_mmio(&[region], &usb, 4, 0x8000000),
            Some(0xb8000000..0xd8000000)
        );
        assert_eq!(
            iommu_mmio(&[region], &usb, 4, 0x8000000),
            Some(0xd8000000..0xd8400000)
        );
        for invalid in [0, 0x8000, 0x1800000, u64::MAX] {
            assert_eq!(pcie_mmio(&[region], &usb, 4, invalid), None);
        }
        assert_eq!(
            iommu_mmio(
                &[Region {
                    size: 0x283fffff,
                    ..region
                }],
                &usb,
                4,
                0x8000000
            ),
            None
        );
    }

    #[test]
    fn pci_mmio_is_not_e820_reserved_or_ram_and_unknown_holes_stay_reserved() {
        let regions = [
            Region {
                base: 0,
                size: 0xb0000000,
                kind: RegionKind::Ram,
            },
            Region {
                base: 0xb0000000,
                size: 0x50000000,
                kind: RegionKind::Mmio,
            },
            Region {
                base: 0x100000000,
                size: 0x100000,
                kind: RegionKind::Reserved(99),
            },
            Region {
                base: 0x100100000,
                size: 0x40000000,
                kind: RegionKind::Ram,
            },
        ];
        let map = os_memory_map(&regions, &[0x8000000..0xc000000]).unwrap();
        for entry in map.entries() {
            assert!(entry.addr + entry.size <= 0xb0000000 || entry.addr >= 0x100000000);
        }
        assert!(
            map.entries()
                .iter()
                .any(|e| e.addr == 0x100000000 && e.size == 0x100000 && e.kind == 2)
        );
        assert!(os_memory_map(&[regions[1], regions[0]], &[]).is_err());
        assert!(os_memory_map(&regions[1..2], &[]).is_err());
    }
    #[test]
    fn final_os_map_keeps_psp_unknown_holes_and_live_dma_out_of_ram() {
        let regions = [
            Region {
                base: 0,
                size: 0x20000000,
                kind: RegionKind::Ram,
            },
            Region {
                base: 0x20000000,
                size: 0x100000,
                kind: RegionKind::Reserved(99),
            },
            Region {
                base: 0x20100000,
                size: 0x10000000,
                kind: RegionKind::Ram,
            },
        ];
        let map = os_memory_map(&regions, &[0x8000000..0xc000000, 0x20000..0x22000]).unwrap();
        for entry in map.entries().iter().filter(|e| e.kind == 1) {
            let range = entry.addr..entry.addr + entry.size;
            for reserved in [
                0..0x10000,
                0x9f000..0x100000,
                0x8000000..0xc000000,
                0x20000..0x22000,
                0x20000000..0x20100000,
            ] {
                assert!(range.end <= reserved.start || reserved.end <= range.start);
            }
        }
        assert!(os_memory_map(&[regions[2], regions[0]], &[]).is_err());
    }
    #[test]
    fn runtime_storage_cannot_cross_holes_or_overflow() {
        let ram = Region {
            base: 0,
            size: 0xb0000000,
            kind: RegionKind::Ram,
        };
        assert!(ram_covers(&[ram], 0x8000000..0xa800000));
        assert!(!ram_covers(&[ram], 0..0));
        assert!(!ram_covers(&[ram], 0xafff0000..0xb0010000));
        let hole = Region {
            kind: RegionKind::Reserved(1),
            ..ram
        };
        assert!(!ram_covers(&[hole], 0x8000000..0xa800000));
    }
}
