//! Shared allocator contracts, independent of any chipset or board.
//!
//! ECAM-backed tests use the real register accessors over ordinary RAM. RAM
//! cannot emulate a device's BAR sizing masks, so allocation tests reuse the
//! existing pre-sized endpoint fixture. Enumeration tests exercise topology,
//! live fixed-BAR validation and command restoration, not hardware sizing.

use super::tests::{endpoint, test_pci};
use super::*;
use alloc::{vec, vec::Vec};

fn ranges(pool: &ResourcePool) -> Vec<(u64, u64)> {
    pool.ranges[..pool.count]
        .iter()
        .map(|range| (range.base, range.end))
        .collect()
}

fn check_pool(pool: &ResourcePool, available: &[bool]) {
    let mut observed = vec![false; available.len()];
    let mut previous_end = 0;
    for range in &pool.ranges[..pool.count] {
        assert!(range.base < range.end);
        assert!(range.base >= previous_end);
        assert!(range.end <= available.len() as u64);
        for byte in &mut observed[range.base as usize..range.end as usize] {
            assert!(!*byte, "overlapping free intervals");
            *byte = true;
        }
        previous_end = range.end;
    }
    assert_eq!(observed, available);
    assert_eq!(
        pool.free_bytes() as usize,
        available.iter().filter(|&&b| b).count()
    );
}

#[test]
fn reservations_and_allocations_match_an_exhaustive_byte_model() {
    for start in 0..=20 {
        for length in 0..=20 {
            for (size, align) in [(1, 1), (2, 2), (3, 4), (4, 8), (8, 16)] {
                let mut pool = ResourcePool::new(4, 12).unwrap();
                let mut available = [false; 24];
                available[4..16].fill(true);
                pool.reserve_range(start, length).unwrap();
                available[start as usize..(start + length).min(24) as usize].fill(false);
                check_pool(&pool, &available);
                loop {
                    let expected = (0..=24 - size).find(|&base| {
                        base % align == 0
                            && available[base as usize..(base + size) as usize]
                                .iter()
                                .all(|&b| b)
                    });
                    let before = ranges(&pool);
                    let allocated = pool.allocate_aligned(size, align);
                    assert_eq!(
                        allocated, expected,
                        "reserve {start}+{length}, {size}/{align}"
                    );
                    let Some(base) = allocated else {
                        assert_eq!(ranges(&pool), before);
                        break;
                    };
                    available[base as usize..(base + size) as usize].fill(false);
                    check_pool(&pool, &available);
                }
            }
        }
    }
}

#[test]
fn mixed_reservations_and_allocations_match_a_seeded_byte_model() {
    for seed in 0..64u64 {
        let mut state = seed;
        let mut pool = ResourcePool::new(0, 128).unwrap();
        let mut available = [true; 128];
        for step in 0..96 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let size = 1 << ((state >> 32) % 4);
            if step % 3 == 0 {
                let start = (state >> 40) % 128;
                pool.reserve_range(start, size).unwrap();
                available[start as usize..(start + size).min(128) as usize].fill(false);
            } else {
                let expected = (0..=128 - size).find(|&base| {
                    base % size == 0
                        && available[base as usize..(base + size) as usize]
                            .iter()
                            .all(|&b| b)
                });
                assert_eq!(
                    pool.allocate_aligned(size, size),
                    expected,
                    "seed {seed}, step {step}"
                );
                if let Some(base) = expected {
                    available[base as usize..(base + size) as usize].fill(false);
                }
            }
            check_pool(&pool, &available);
        }
    }
}

#[test]
fn overflow_in_one_candidate_does_not_hide_a_later_usable_range() {
    let mut pool = ResourcePool::new(u64::MAX - 7, 7).unwrap();
    pool.push_range(0x1000, 0x2000).unwrap();
    assert_eq!(pool.allocate_aligned(16, 16), Some(0x1000));
    assert_eq!(ranges(&pool), [(u64::MAX - 7, u64::MAX), (0x1010, 0x2000)]);
}

#[test]
fn range_capacity_failure_skips_a_split_without_losing_space() {
    let mut pool = ResourcePool::new(0x101, 0x180 - 0x101).unwrap();
    for index in 0..MAX_RESOURCE_RANGES - 1 {
        let base = 0x200 + index as u64 * 0x100;
        pool.push_range(base, base + 0x20).unwrap();
    }
    let free = pool.free_bytes();
    assert_eq!(pool.allocate_aligned(0x20, 0x40), Some(0x200));
    assert_eq!(ranges(&pool)[0], (0x101, 0x180));
    assert_eq!(pool.free_bytes(), free - 0x20);
}

#[test]
fn empty_zero_and_overflow_resource_requests_are_bounded() {
    assert!(matches!(
        ResourcePool::new(u64::MAX, 1),
        Err(PciEcamError::ConfigError)
    ));
    let mut pool = ResourcePool::new(0x1000, 0x1000).unwrap();
    let before = ranges(&pool);
    for (size, align) in [(0, 1), (1, 0), (u64::MAX, 16), (0x2000, 1)] {
        assert_eq!(pool.allocate_aligned(size, align), None);
        assert_eq!(ranges(&pool), before);
    }
    assert_eq!(
        pool.reserve_range(u64::MAX, 2),
        Err(PciEcamError::ConfigError)
    );
    assert_eq!(ranges(&pool), before);
    assert_eq!(pool.allocate_aligned(0x1000, 0x1000), Some(0x1000));
    assert_eq!(pool.free_bytes(), 0);
}

fn memory_bar(address: PciAddress) -> PciFixedBar {
    PciFixedBar {
        address,
        register: PCI_BAR0,
        kind: PciFixedBarType::Memory32,
        base: 0x8000_4000,
        size: 0x1000,
        prefetchable: false,
    }
}

#[test]
fn malformed_fixed_bars_are_rejected_without_reserving_any_space() {
    let (mut pci, _memory) = test_pci();
    let good = memory_bar(PciAddress::new(0, 0, 1, 0));
    let invalid = [
        PciFixedBar { base: 0, ..good },
        PciFixedBar { size: 0, ..good },
        PciFixedBar { size: 3, ..good },
        PciFixedBar {
            base: good.base + 1,
            ..good
        },
        PciFixedBar {
            register: 0x0c,
            ..good
        },
        PciFixedBar {
            register: 0x11,
            ..good
        },
        PciFixedBar {
            register: 0x28,
            ..good
        },
        PciFixedBar {
            address: PciAddress::new(1, 0, 1, 0),
            ..good
        },
        PciFixedBar {
            address: PciAddress::new(0, 3, 1, 0),
            ..good
        },
        PciFixedBar {
            base: 0x1_0000_0000,
            ..good
        },
        PciFixedBar {
            base: u64::MAX - 0xfff,
            ..good
        },
        PciFixedBar {
            kind: PciFixedBarType::Io,
            base: 0x1_0000,
            ..good
        },
        PciFixedBar {
            kind: PciFixedBarType::Io,
            base: 0x1000,
            prefetchable: true,
            ..good
        },
        PciFixedBar {
            kind: PciFixedBarType::Memory64,
            register: 0x24,
            ..good
        },
    ];
    let before = ranges(&pci.roots[0].mmio32);
    for bar in invalid {
        assert_eq!(
            pci.add_fixed_bars(&[bar]),
            Err(PciEcamError::FixedBarInvalid),
            "{bar:?}"
        );
        assert!(pci.fixed_bars.is_empty());
        assert_eq!(ranges(&pci.roots[0].mmio32), before);
    }
}

#[test]
fn fixed_bar_batch_conflicts_and_capacity_errors_are_atomic() {
    let (mut pci, _memory) = test_pci();
    let first = memory_bar(PciAddress::new(0, 0, 1, 0));
    let conflict = PciFixedBar {
        address: PciAddress::new(0, 0, 2, 0),
        ..first
    };
    let before = ranges(&pci.roots[0].mmio32);
    assert_eq!(
        pci.add_fixed_bars(&[first, conflict]),
        Err(PciEcamError::FixedBarConflict)
    );
    assert!(pci.fixed_bars.is_empty());
    assert_eq!(ranges(&pci.roots[0].mmio32), before);
    let bars: Vec<_> = (0..=MAX_PCI_FIXED_BARS)
        .map(|index| PciFixedBar {
            address: PciAddress::new(0, 0, index as u8, 0),
            base: first.base + index as u64 * 0x1000,
            ..first
        })
        .collect();
    assert_eq!(
        pci.add_fixed_bars(&bars),
        Err(PciEcamError::ResourceRangeLimit)
    );
    assert!(pci.fixed_bars.is_empty());
    assert_eq!(ranges(&pci.roots[0].mmio32), before);
    pci.add_fixed_bars(&bars[..MAX_PCI_FIXED_BARS]).unwrap();
    let before = ranges(&pci.roots[0].mmio32);
    assert_eq!(
        pci.add_fixed_bars(&bars[MAX_PCI_FIXED_BARS..]),
        Err(PciEcamError::ResourceRangeLimit)
    );
    assert_eq!(pci.fixed_bars.len(), MAX_PCI_FIXED_BARS);
    assert_eq!(ranges(&pci.roots[0].mmio32), before);
}

#[test]
fn duplicate_fixed_register_and_cross_width_memory_overlap_are_rejected() {
    let (mut pci, _memory) = test_pci();
    let first = memory_bar(PciAddress::new(0, 0, 1, 0));
    pci.add_fixed_bars(&[first]).unwrap();
    for other in [
        PciFixedBar {
            base: first.base + 0x1000,
            ..first
        },
        PciFixedBar {
            address: PciAddress::new(0, 0, 2, 0),
            kind: PciFixedBarType::Memory64,
            ..first
        },
    ] {
        assert_eq!(
            pci.add_fixed_bars(&[other]),
            Err(PciEcamError::FixedBarConflict)
        );
    }
    // Numerically identical I/O and memory intervals are distinct spaces.
    pci.add_fixed_bars(&[PciFixedBar {
        address: PciAddress::new(0, 0, 2, 0),
        kind: PciFixedBarType::Io,
        base: 0x4000,
        ..first
    }])
    .unwrap();
    pci.add_fixed_bars(&[PciFixedBar {
        address: PciAddress::new(0, 0, 3, 0),
        base: 0x4000,
        ..first
    }])
    .unwrap();
}

#[test]
fn multi_root_reservation_failure_rolls_back_every_root() {
    let (mut pci, _memory) = test_pci();
    let mut other = pci.roots[0];
    other.segment = 1;
    other.mmio32 = ResourcePool::empty();
    for index in 0..MAX_RESOURCE_RANGES {
        other
            .mmio32
            .push_range(
                0x8000_0000 + index as u64 * 0x1000,
                0x8000_0800 + index as u64 * 0x1000,
            )
            .unwrap();
    }
    pci.roots.push(other).ok().unwrap();
    let before: Vec<_> = pci.roots.iter().map(|root| ranges(&root.mmio32)).collect();
    assert_eq!(
        pci.reserve_mmio_range(0x8000_0100, 0x100),
        Err(PciEcamError::ResourceRangeLimit)
    );
    let after: Vec<_> = pci.roots.iter().map(|root| ranges(&root.mmio32)).collect();
    assert_eq!(before, after);
}

#[test]
fn live_fixed_bar_type_prefetch_and_upper_address_are_checked_without_writes() {
    let (mut pci, _memory) = test_pci();
    let address = PciAddress::new(0, 0, 1, 0);
    for (kind, base, prefetchable, valid, invalid) in [
        (
            PciFixedBarType::Memory32,
            0x8000_4000,
            false,
            0x8000_4000,
            [0, 0x8000_5000, 0x8000_4001, 0x8000_4008],
        ),
        (
            PciFixedBarType::Memory32,
            0x8000_4000,
            true,
            0x8000_4008,
            [0, 0x8000_4000, 0x8000_400c, 0x8000_5008],
        ),
        (
            PciFixedBarType::Memory64,
            0x1_8000_4000,
            true,
            0x8000_400c,
            [0, 0x8000_4008, 0x8000_4004, 0x8000_500c],
        ),
        (
            PciFixedBarType::Io,
            0x4000,
            false,
            0x4001,
            [0, 0x4000, 0x5001, 0x4021],
        ),
    ] {
        pci.fixed_bars.clear();
        pci.add_fixed_bars(&[PciFixedBar {
            kind,
            base,
            prefetchable,
            ..memory_bar(address)
        }])
        .unwrap();
        pci.write32(address, PCI_BAR0 + 4, (base >> 32) as u32);
        for value in invalid {
            pci.write32(address, PCI_BAR0, value);
            assert!(matches!(
                pci.size_bar(address, 0),
                Err(PciEcamError::FixedBarInvalid)
            ));
            assert_eq!(pci.read32(address, PCI_BAR0), value);
        }
        pci.write32(address, PCI_BAR0, valid);
        let (bar, pair) = pci.size_bar(address, 0).unwrap();
        assert!(bar.fixed && bar.allocated);
        assert_eq!(pair, kind == PciFixedBarType::Memory64);
        assert_eq!(pci.read32(address, PCI_BAR0), valid);
        if pair {
            pci.write32(address, PCI_BAR0 + 4, 2);
            assert!(matches!(
                pci.size_bar(address, 0),
                Err(PciEcamError::FixedBarInvalid)
            ));
            assert_eq!(pci.read32(address, PCI_BAR0 + 4), 2);
        }
    }
}

#[test]
fn ehci_debug_capability_requires_explicit_live_bar_ownership() {
    for (reserved, initialized) in [(true, false), (false, false), (true, true)] {
        let (mut pci, mut memory) = test_pci();
        memory.fill(u32::MAX); // absent functions
        let slot = PciAddress::new(0, 0, 0x1a, 0);
        let ehci = PciAddress::new(0, 0, 0x1a, 7);
        pci.write32(slot, PCI_VENDOR_ID, 0x1234_8086);
        pci.write32(slot, 8, 0x0600_0000); // no generic BARs on the sentinel function
        pci.device(slot)
            .unwrap()
            .write8(PCI_HEADER_TYPE, PCI_HEADER_TYPE_MULTI_FUNC);
        pci.write32(ehci, PCI_VENDOR_ID, 0x293c_8086);
        pci.write32(ehci, 8, 0x0c03_2000);
        pci.device(ehci).unwrap().write8(PCI_HEADER_TYPE, 0);
        let command = 0x0400 | PCI_CMD_MEMORY | PCI_CMD_BUS_MASTER;
        pci.write16(ehci, PCI_COMMAND, command);
        for index in 0..6 {
            pci.write32(ehci, PCI_BAR0 + 4 * index, 0);
        }
        let fixed = PciFixedBar {
            base: 0xfef0_0000,
            ..memory_bar(ehci)
        };
        if initialized {
            pci.write32(ehci, PCI_BAR0, fixed.base as u32);
        }
        if reserved {
            pci.add_fixed_bars(&[fixed]).unwrap();
        }
        let result = pci.enumerate_and_allocate();
        if reserved && !initialized {
            assert_eq!(result, Err(PciEcamError::FixedBarInvalid));
            assert_eq!(pci.read32(ehci, PCI_BAR0), 0);
            assert_eq!(pci.read16(ehci, PCI_COMMAND), command);
        } else {
            result.unwrap();
            assert_eq!(pci.device_count(), 2);
            assert_ne!(pci.read16(ehci, PCI_COMMAND) & PCI_CMD_MEMORY, 0);
            if reserved {
                assert_eq!(pci.read32(ehci, PCI_BAR0), fixed.base as u32);
            } else {
                assert!((0x8000_0000..0x9000_0000).contains(&pci.read32(ehci, PCI_BAR0)));
            }
        }
    }
}

#[test]
fn fixed_resource_for_an_absent_or_unscanned_function_is_rejected() {
    for function_zero_present in [false, true] {
        let (mut pci, mut memory) = test_pci();
        memory.fill(u32::MAX);
        let slot = PciAddress::new(0, 0, 1, 0);
        let hidden = PciAddress::new(0, 0, 1, 7);
        if function_zero_present {
            pci.write32(slot, PCI_VENDOR_ID, 0x1234_8086);
            pci.write32(slot, 8, 0x0600_0000);
            pci.device(slot).unwrap().write8(PCI_HEADER_TYPE, 0); // single function
        }
        pci.write32(hidden, PCI_VENDOR_ID, 0x293c_8086);
        pci.add_fixed_bars(&[memory_bar(hidden)]).unwrap();
        assert_eq!(
            pci.enumerate_and_allocate(),
            Err(PciEcamError::FixedBarInvalid)
        );
    }
}

#[test]
fn fixed_64bit_pair_survives_probing_and_allocation() {
    let (mut pci, _memory) = test_pci();
    let addr = PciAddress::new(0, 0, 1, 0);
    let fixed = PciFixedBar {
        register: 0x20,
        kind: PciFixedBarType::Memory64,
        base: 0x1_8000_0000,
        prefetchable: true,
        ..memory_bar(addr)
    };
    pci.write32(addr, PCI_VENDOR_ID, 0x1234_8086);
    pci.write32(addr, 0x20, 0x8000_000c);
    pci.write32(addr, 0x24, 1);
    pci.add_fixed_bars(&[fixed]).unwrap();
    let dev = pci.probe_device(addr).unwrap().unwrap();
    assert_eq!(dev.bars[5].bar_type, BarType::None);
    assert!(pci.devices.push(dev).is_ok());
    pci.allocate_resources().unwrap();
    assert_eq!(pci.read32(addr, 0x20), 0x8000_000c);
    assert_eq!(pci.read32(addr, 0x24), 1);
    assert!(
        pci.assigned_memory_windows()
            .any(|w| w.base == fixed.base && w.size == fixed.size)
    );
}

fn bar_base(pci: &PciEcam, dev: &PciDev, bar: &BarInfo) -> u64 {
    let low = pci.read32(dev.addr, bar.reg);
    match bar.bar_type {
        BarType::Io => u64::from(low & !3),
        BarType::Memory32 => u64::from(low & !15),
        BarType::Memory64 => {
            u64::from(low & !15) | (u64::from(pci.read32(dev.addr, bar.reg + 4)) << 32)
        }
        BarType::None => panic!("not a BAR"),
    }
}

#[test]
fn mixed_bar_permutations_remain_aligned_bounded_and_nonoverlapping() {
    let (mut pci, _memory) = test_pci();
    for rotation in 0..8 {
        for reverse in [false, true] {
            pci.devices.clear();
            pci.configure_windows(
                0x8000_0000,
                0x0200_0000,
                0x1_0000_0000,
                0x0200_0000,
                0x1000,
                0xf000,
            )
            .unwrap();
            pci.reserve_mmio_range(0x8001_0000, 0x10000).unwrap();
            pci.reserve_io_range(0x2000, 0x100).unwrap();
            for index in 0..8 {
                let index = if reverse { 7 - index } else { index };
                let id = (index + rotation) % 8;
                let kind = if id % 2 == 0 {
                    BarType::Memory32
                } else {
                    BarType::Memory64
                };
                let mut dev = endpoint(
                    PciAddress::new(0, 0, id + 1, 0),
                    &[
                        (0, kind, 0x1000 << (id % 4)),
                        (4, BarType::Io, 4 << (id % 4)),
                    ],
                );
                dev.original_command = 0x0400;
                dev.bars[0].prefetchable = id % 3 == 0;
                assert!(pci.devices.push(dev).is_ok());
            }
            pci.allocate_resources().unwrap();
            let mut spans = Vec::new();
            for dev in &pci.devices {
                assert_eq!(
                    pci.read16(dev.addr, PCI_COMMAND),
                    0x0400 | PCI_CMD_IO | PCI_CMD_MEMORY | PCI_CMD_BUS_MASTER
                );
                for bar in dev.bars.iter().filter(|bar| bar.bar_type != BarType::None) {
                    let base = bar_base(&pci, dev, bar);
                    let io = bar.bar_type == BarType::Io;
                    assert!(bar.allocated);
                    assert_eq!(base % bar.size, 0);
                    if io {
                        assert!(base >= 0x1000 && base + bar.size <= 0x10000);
                        assert!(base + bar.size <= 0x2000 || base >= 0x2100);
                    } else {
                        assert!(base >= 0x8000_0000 && base + bar.size <= 0x8200_0000);
                        assert!(base + bar.size <= 0x8001_0000 || base >= 0x8002_0000);
                        assert_eq!(pci.read32(dev.addr, bar.reg) & 8 != 0, bar.prefetchable);
                    }
                    for &(other_io, start, end) in &spans {
                        assert!(io != other_io || base >= end || base + bar.size <= start);
                    }
                    spans.push((io, base, base + bar.size));
                }
            }
        }
    }
}

#[test]
fn low_space_is_kept_for_32bit_bars_and_64bit_bars_fall_back_high() {
    let (mut pci, _memory) = test_pci();
    pci.configure_windows(0x8000_0000, 0x20_0000, 0x1_0000_0000, 0x40_0000, 0, 0)
        .unwrap();
    for (slot, kind) in [
        (1, BarType::Memory64),
        (2, BarType::Memory64),
        (3, BarType::Memory32),
    ] {
        assert!(
            pci.devices
                .push(endpoint(
                    PciAddress::new(0, 0, slot, 0),
                    &[(0, kind, 0x10_0000)]
                ))
                .is_ok()
        );
    }
    pci.allocate_resources().unwrap();
    assert_eq!(
        bar_base(&pci, &pci.devices[2], &pci.devices[2].bars[0]),
        0x8000_0000
    );
    assert_eq!(
        bar_base(&pci, &pci.devices[0], &pci.devices[0].bars[0]),
        0x8010_0000
    );
    assert_eq!(
        bar_base(&pci, &pci.devices[1], &pci.devices[1].bars[0]),
        0x1_0000_0000
    );
}

#[test]
fn exhausted_32bit_bar_does_not_truncate_a_high_address_or_modify_registers() {
    let (mut pci, _memory) = test_pci();
    pci.configure_windows(0xffff_f000, 0x1000, 0x1_0000_0000, 0x40_0000, 0, 0)
        .unwrap();
    let addr = PciAddress::new(0, 0, 1, 0);
    assert!(
        pci.devices
            .push(endpoint(addr, &[(0, BarType::Memory32, 0x2000)]))
            .is_ok()
    );
    pci.write32(addr, PCI_BAR0, 0xdead_bee0);
    pci.write16(addr, PCI_COMMAND, 0x0407);
    assert_eq!(
        pci.allocate_resources(),
        Err(PciEcamError::ResourceExhausted)
    );
    assert_eq!(pci.read32(addr, PCI_BAR0), 0xdead_bee0);
    assert_eq!(pci.read16(addr, PCI_COMMAND), 0x0407);
    assert_eq!(pci.roots[0].mmio64.free_bytes(), 0x40_0000);
    assert!(!pci.devices[0].bars[0].allocated);
}

#[test]
fn exact_32bit_and_io_upper_boundaries_do_not_wrap() {
    let (mut pci, _memory) = test_pci();
    pci.configure_windows(0xffff_f000, 0x1000, 0, 0, 0xffe0, 0x20)
        .unwrap();
    let addr = PciAddress::new(0, 0, 1, 0);
    assert!(
        pci.devices
            .push(endpoint(
                addr,
                &[(0, BarType::Memory32, 0x1000), (1, BarType::Io, 0x20)]
            ))
            .is_ok()
    );
    pci.allocate_resources().unwrap();
    assert_eq!(pci.read32(addr, PCI_BAR0), 0xffff_f000);
    assert_eq!(pci.read32(addr, PCI_BAR0 + 4), 0xffe1);
    assert_eq!(pci.roots[0].mmio32.free_bytes(), 0);
    assert_eq!(pci.roots[0].io_pool.free_bytes(), 0);
}

fn bridge(addr: PciAddress, secondary: u8, subordinate: u8) -> PciDev {
    PciDev {
        header_type: PCI_HEADER_TYPE_BRIDGE,
        secondary_bus: secondary,
        subordinate_bus: subordinate,
        ..endpoint(addr, &[])
    }
}

#[test]
fn bridge_own_bar_is_allocated_on_its_primary_bus() {
    let (mut pci, _memory) = test_pci();
    let addr = PciAddress::new(0, 0, 1, 0);
    let mut dev = bridge(addr, 1, 1);
    dev.bars[0] = endpoint(addr, &[(0, BarType::Memory32, 0x1000)]).bars[0];
    assert!(pci.devices.push(dev).is_ok());
    pci.allocate_resources().unwrap();
    assert!(pci.devices[0].bars[0].allocated);
    assert_eq!(pci.read32(addr, PCI_BAR0), 0x8000_0000);
    assert_eq!(pci.read32(addr, PCI_MEMORY_BASE), 0x0000_ffff); // no children
}

#[test]
fn empty_bridge_windows_are_disabled() {
    let (mut pci, _memory) = test_pci();
    let addr = PciAddress::new(0, 0, 1, 0);
    assert!(pci.devices.push(bridge(addr, 1, 2)).is_ok());
    pci.allocate_resources().unwrap();
    assert_eq!(pci.read32(addr, PCI_MEMORY_BASE), 0x0000_ffff);
    assert_eq!(pci.read32(addr, PCI_PREF_MEMORY_BASE), 0x0000_ffff);
    assert_eq!(pci.read32(addr, PCI_PREF_BASE_UPPER32), 0);
    assert_eq!(pci.read32(addr, PCI_PREF_LIMIT_UPPER32), 0);
    assert_eq!(pci.read32(addr, PCI_IO_BASE), 0x00ff);
}

#[test]
fn nested_bridge_windows_cover_io_low_mmio_and_high_prefetchable_children() {
    let (mut pci, _memory) = test_pci();
    pci.configure_windows(
        0x8000_0000,
        0x10_0000,
        0x1_2000_0000,
        0x80_0000,
        0x1000,
        0xf000,
    )
    .unwrap();
    let bridges = [PciAddress::new(0, 0, 1, 0), PciAddress::new(0, 1, 1, 0)];
    for (addr, bus) in bridges.into_iter().zip([1, 2]) {
        assert!(pci.devices.push(bridge(addr, bus, 2)).is_ok());
    }
    let addr = PciAddress::new(0, 2, 0, 0);
    let mut dev = endpoint(
        addr,
        &[
            (0, BarType::Io, 0x20),
            (1, BarType::Memory32, 0x2000),
            (2, BarType::Memory64, 0x20_0000),
        ],
    );
    dev.bars[2].prefetchable = true;
    assert!(pci.devices.push(dev).is_ok());
    pci.allocate_resources().unwrap();
    for addr in bridges {
        let memory = pci.read32(addr, PCI_MEMORY_BASE);
        assert_eq!(memory, 0x8000_8000);
        let pref = pci.read32(addr, PCI_PREF_MEMORY_BASE);
        // Type/capability bits are read-only in hardware; plain RAM cannot
        // model them. This fixture assumes a 64-bit-capable prefetch window
        // and checks its programmed address fields, including both uppers.
        let base = (u64::from(pci.read32(addr, PCI_PREF_BASE_UPPER32)) << 32)
            | (u64::from(pref & 0xfff0) << 16);
        let end = (u64::from(pci.read32(addr, PCI_PREF_LIMIT_UPPER32)) << 32)
            | u64::from(pref & 0xfff0_0000)
            | 0xf_ffff;
        assert_eq!(base, 0x1_2000_0000);
        assert_eq!(end, 0x1_201f_ffff);
        assert_eq!(pci.read32(addr, PCI_IO_BASE) & 0xffff, 0x1010);
    }
}

#[test]
fn nonprefetchable_bar_behind_bridge_cannot_use_a_64bit_only_window() {
    let (mut pci, _memory) = test_pci();
    pci.configure_windows(0, 0, 0x1_0000_0000, 0x40_0000, 0, 0)
        .unwrap();
    assert!(
        pci.devices
            .push(bridge(PciAddress::new(0, 0, 1, 0), 1, 1))
            .is_ok()
    );
    assert!(
        pci.devices
            .push(endpoint(
                PciAddress::new(0, 1, 0, 0),
                &[(0, BarType::Memory64, 0x1000)]
            ))
            .is_ok()
    );
    assert_eq!(
        pci.allocate_resources(),
        Err(PciEcamError::ResourceExhausted)
    );
    assert!(!pci.devices[1].bars[0].allocated);
}

#[test]
fn multifunction_enumeration_and_bus_255_do_not_wrap() {
    let (mut pci, mut memory) = test_pci();
    memory.fill(u32::MAX);
    for function in [0, 1, 7] {
        let addr = PciAddress::new(0, 0, 1, function);
        pci.write32(addr, PCI_VENDOR_ID, 0x1234_8086);
        pci.write32(addr, 8, 0x0600_0000);
        pci.device(addr).unwrap().write8(
            PCI_HEADER_TYPE,
            if function == 0 {
                PCI_HEADER_TYPE_MULTI_FUNC
            } else {
                0
            },
        );
    }
    pci.enumerate_and_allocate().unwrap();
    assert_eq!(
        pci.devices()
            .map(|addr| addr.function())
            .collect::<Vec<_>>(),
        [0, 1, 7]
    );
    // Bias ECAM so the existing RAM allocation backs bus 255, not 0.
    pci.devices.clear();
    pci.roots[0].ecam_base -= 255 << 20;
    pci.roots[0].bus_start = 255;
    pci.roots[0].bus_end = 255;
    pci.roots[0].next_bus = 256;
    let addr = PciAddress::new(0, 255, 1, 0);
    pci.device(addr)
        .unwrap()
        .write8(PCI_HEADER_TYPE, PCI_HEADER_TYPE_BRIDGE);
    assert_eq!(
        pci.enumerate_and_allocate(),
        Err(PciEcamError::ResourceExhausted)
    );
    assert_eq!(pci.roots[0].next_bus, 256);
}

#[test]
fn cardbus_header_preserves_its_window_registers_and_limits_reserved_buses() {
    let (mut pci, mut memory) = test_pci();
    memory.fill(u32::MAX);
    let addr = PciAddress::new(0, 0, 1, 0);
    pci.write32(addr, PCI_VENDOR_ID, 0x1234_8086);
    pci.write32(addr, 8, 0x0607_0000);
    pci.device(addr)
        .unwrap()
        .write8(PCI_HEADER_TYPE, PCI_HEADER_TYPE_CARDBUS);
    pci.write32(addr, PCI_BAR0, 0);
    for register in [0x1c, 0x20, 0x24, 0x28, 0x2c, 0x30, 0x34] {
        pci.write32(addr, register, 0xa5a5_5a5a);
    }
    pci.enumerate_and_allocate().unwrap();
    assert_eq!(pci.devices[0].secondary_bus, 1);
    assert_eq!(pci.devices[0].subordinate_bus, 2);
    assert_eq!(pci.read32(addr, PCI_PRIMARY_BUS) & 0xffffff, 0x02_0100);
    assert!(
        pci.devices[0].bars[1..]
            .iter()
            .all(|bar| bar.bar_type == BarType::None)
    );
    for register in [0x1c, 0x20, 0x24, 0x28, 0x2c, 0x30, 0x34] {
        assert_eq!(pci.read32(addr, register), 0xa5a5_5a5a);
    }
}

#[test]
fn domains_do_not_borrow_resources_or_config_registers() {
    let (mut pci, _first_memory) = test_pci();
    let (mut second, _second_memory) = test_pci();
    pci.configure_windows(0x8000_0000, 0x1000, 0, 0, 0, 0)
        .unwrap();
    second
        .configure_windows(0xa000_0000, 0x8000, 0, 0, 0, 0)
        .unwrap();
    second.roots[0].segment = 1;
    pci.roots.push(second.roots[0]).ok().unwrap();
    let addresses = [PciAddress::new(0, 0, 1, 0), PciAddress::new(1, 0, 1, 0)];
    for (root, address) in addresses.into_iter().enumerate() {
        let mut dev = endpoint(address, &[(0, BarType::Memory32, 0x2000)]);
        dev.root = root;
        assert!(pci.devices.push(dev).is_ok());
    }
    assert_eq!(
        pci.allocate_resources(),
        Err(PciEcamError::ResourceExhausted)
    );
    assert_eq!(pci.roots[1].mmio32.free_bytes(), 0x8000);
    assert_eq!(pci.read32(addresses[1], PCI_BAR0), 0);
    pci.devices[0].bars[0].size = 0x1000;
    pci.allocate_resources().unwrap();
    assert_eq!(pci.read32(addresses[0], PCI_BAR0), 0x8000_0000);
    assert_eq!(pci.read32(addresses[1], PCI_BAR0), 0xa000_0000);
    assert_eq!(pci.roots[0].mmio32.free_bytes(), 0);
    assert_eq!(pci.roots[1].mmio32.free_bytes(), 0x6000);
}

#[test]
fn nonprefetchable_64bit_child_uses_low_space_when_a_high_pool_exists() {
    let (mut pci, _memory) = test_pci();
    pci.configure_windows(0x8000_0000, 0x10_0000, 0x1_0000_0000, 0x40_0000, 0, 0)
        .unwrap();
    let parent = PciAddress::new(0, 0, 1, 0);
    let child = PciAddress::new(0, 1, 0, 0);
    assert!(pci.devices.push(bridge(parent, 1, 1)).is_ok());
    assert!(
        pci.devices
            .push(endpoint(child, &[(0, BarType::Memory64, 0x1000)]))
            .is_ok()
    );
    pci.allocate_resources().unwrap();
    assert_eq!(pci.read32(child, PCI_BAR0), 0x8000_0004);
    assert_eq!(pci.read32(child, PCI_BAR0 + 4), 0);
    assert_eq!(pci.read32(parent, PCI_MEMORY_BASE), 0x8000_8000);
    assert_eq!(pci.roots[0].mmio64.free_bytes(), 0x40_0000);
}

#[test]
fn mmio_reservations_and_allocations_do_not_shrink_the_os_parent_aperture() {
    let (mut pci, _memory) = test_pci();
    pci.configure_windows(0x8000_0000, 0x10000, 0, 0, 0, 0)
        .unwrap();
    pci.reserve_mmio_range(0x8000_4000, 0x4000).unwrap();
    for slot in 1..=3 {
        assert!(
            pci.devices
                .push(endpoint(
                    PciAddress::new(0, 0, slot, 0),
                    &[(0, BarType::Memory32, 0x4000)]
                ))
                .is_ok()
        );
    }
    pci.allocate_resources().unwrap();
    assert_eq!(pci.roots[0].mmio32.free_bytes(), 0);
    let spans: Vec<_> = pci
        .assigned_memory_windows()
        .map(|w| (w.base, w.size))
        .collect();
    assert_eq!(
        spans,
        [
            (0x8000_0000, 0x4000),
            (0x8000_8000, 0x4000),
            (0x8000_c000, 0x4000)
        ]
    );
}
