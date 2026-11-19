//! The window search ([`super::solver`]): layouts the subtree allocator
//! misses, what each window may forward, and, on tiny generated
//! topologies, against a brute-force oracle over BAR addresses and
//! forwarding choices. Layouts are read back from the registers the
//! allocator wrote to RAM-backed config space ([`valid`]).

use super::master_tests::{Rng, describe, pools_of, typed, valid};
use super::solver::{self, Budget};
use super::tests::{
    bridge_at, endpoint, ethanol_x, ethanol_x_functions, io_bar_base, memory_window,
    prefetchable_window, test_pci_with_buses,
};
use super::*;
use alloc::{vec, vec::Vec};

const MIB: u64 = 1 << 20;
const LOW: u64 = 0x8000_0000;
const HIGH: u64 = 1 << 32;

/// The allocators firmware runs, and the search alone.
const MODES: [Allocators; 2] = [
    Allocators::Both(Budget::FIRMWARE),
    Allocators::Search(Budget::FIRMWARE),
];

/// A root of six buses with `low` and `high` ranges of memory, `ports`
/// from 0x1000 and `devices`, run by `allocators`; the free ranges,
/// inclusive, for [`valid`].
fn root(
    low: &[(u64, u64)],
    high: &[(u64, u64)],
    ports: u64,
    devices: Vec<PciDev>,
    allocators: Allocators,
) -> (PciEcam, Vec<u32>, Vec<(u64, u64)>) {
    let (mut pci, space) = test_pci_with_buses(6);
    let pool = |ranges: &[(u64, u64)]| {
        let mut pool = ResourcePool::empty();
        for &(base, end) in ranges {
            pool.push_range(base, end).unwrap();
        }
        pool
    };
    let io = [(0x1000, 0x1000 + ports)];
    (pci.roots[0].mmio32, pci.roots[0].mmio64) = (pool(low), pool(high));
    pci.roots[0].io_pool = pool(&io[..usize::from(ports != 0)]);
    pci.allocators = allocators;
    for device in devices {
        assert!(pci.devices.push(device).is_ok());
    }
    let free = low
        .iter()
        .chain(high)
        .chain(&io[..usize::from(ports != 0)])
        .map(|&(base, end)| (base, end - 1))
        .collect();
    (pci, space, free)
}

/// Allocate, and check what was written decodes as meant.
fn allocate(pci: &mut PciEcam, free: &[(u64, u64)]) -> Result<(), PciEcamError> {
    let result = pci.allocate_resources();
    if result.is_ok()
        && let Err(error) = valid(pci, free).and_then(|()| typed(pci, &pools_of(pci)))
    {
        panic!("{error}\n{}", describe(pci, free));
    }
    result
}

/// An endpoint with 64-bit prefetchable BARs of `sizes`.
fn prefetchable(address: PciAddress, sizes: &[u64]) -> PciDev {
    let definitions: Vec<_> = sizes
        .iter()
        .enumerate()
        .map(|(n, &size)| (2 * n, BarType::Memory64, size))
        .collect();
    let mut device = endpoint(address, &definitions);
    for bar in device
        .bars
        .iter_mut()
        .filter(|bar| bar.bar_type != BarType::None)
    {
        bar.prefetchable = true;
    }
    device
}

/// Where the memory BAR at `index` of `address` was written.
fn base(pci: &PciEcam, address: PciAddress, index: usize) -> u64 {
    let register = PCI_BAR0 + 4 * index as u16;
    let low = pci.read32(address, register);
    let high = match low & 0x6 {
        0x4 => u64::from(pci.read32(address, register + 4)) << 32,
        _ => 0,
    };
    high | u64::from(low & !0xf)
}

/// The reviewer's layout: 13 MiB below 4 GiB, 8 MiB above, behind one
/// bridge 64-bit prefetchable BARs of 8, 4, 4 and 4 MiB and two 32 KiB
/// ones. No single move of a BAR from above 4 GiB to below fits; 12 MiB of
/// prefetchable BARs and both small ones below, through the memory window
/// (13 MiB), and the rest above, through the prefetchable window, does.
#[test]
fn a_split_no_single_move_reaches() {
    let bridge = PciAddress::new(0, 0, 1, 0);
    let functions = [0, 1].map(|function| PciAddress::new(0, 1, 0, function));
    let devices = || {
        let mut devices = vec![bridge_at(bridge, 1, 1)];
        for (address, sizes) in functions
            .into_iter()
            .zip([[8 * MIB, 4 * MIB], [4 * MIB; 2]])
        {
            let mut device = prefetchable(address, &sizes);
            device.bars[4] = endpoint(address, &[(4, BarType::Memory32, 0x8000)]).bars[4];
            devices.push(device);
        }
        devices
    };
    let (low, high) = ([(LOW, LOW + 13 * MIB)], [(HIGH, HIGH + 8 * MIB)]);
    let (mut pci, _space, free) = root(&low, &high, 0, devices(), Allocators::FastPaths);
    assert_eq!(
        allocate(&mut pci, &free),
        Err(PciEcamError::ResourceExhausted)
    );
    for allocators in MODES {
        let (mut pci, _space, free) = root(&low, &high, 0, devices(), allocators);
        allocate(&mut pci, &free).unwrap();
        let memory = memory_window(&pci, bridge);
        let prefetchable = prefetchable_window(&pci, bridge);
        assert!(*prefetchable.start() >= HIGH, "{prefetchable:x?}");
        let mut below = 0;
        for address in functions {
            for index in [0, 2] {
                let size = pci.devices[1 + address.function() as usize].bars[index].size;
                let at = base(&pci, address, index);
                let window = if at < HIGH { &memory } else { &prefetchable };
                assert!(window.contains(&at) && window.contains(&(at + size - 1)));
                below += if at < HIGH { size } else { 0 };
            }
            let small = base(&pci, address, 4);
            assert!(small.is_multiple_of(0x8000) && memory.contains(&small));
        }
        assert_eq!(below, 12 * MIB);
        assert_eq!(memory.end() + 1 - memory.start(), 13 * MIB);
    }
}

/// A 1 MiB BAR and a 4 MiB 32-bit prefetchable one in
/// [0x8030_0000, 0x8080_0000): the first at its front, the second aligned
/// above it, however the windows are found.
#[test]
fn a_32bit_prefetchable_bar_aligned_above_a_small_one() {
    let device = PciAddress::new(0, 1, 0, 0);
    for allocators in MODES.into_iter().chain([Allocators::FastPaths]) {
        let mut endpoint = endpoint(
            device,
            &[(0, BarType::Memory32, MIB), (1, BarType::Memory32, 4 * MIB)],
        );
        endpoint.bars[1].prefetchable = true;
        let devices = vec![bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1), endpoint];
        let low = [(0x8030_0000, 0x8080_0000)];
        let (mut pci, _space, free) =
            root(&low, &[(HIGH, HIGH + 256 * MIB)], 0, devices, allocators);
        allocate(&mut pci, &free).unwrap();
        assert_eq!(pci.read32(device, PCI_BAR0), 0x8030_0000);
        assert_eq!(pci.read32(device, crate::PCI_BAR1), 0x8040_0008);
    }
}

/// 8 MiB below 4 GiB and 4 MiB above, prefetchable 64-bit BARs of 8 and
/// 4 MiB: behind a 64-bit bridge the 8 MiB one below through the memory
/// window, the 4 MiB one above through the prefetchable window; on the
/// root bus the same without a window; behind a bridge whose prefetchable
/// window is 32-bit, nowhere.
#[test]
fn prefetchable_bars_split_across_4_gib_where_they_may() {
    let (low, high) = ([(LOW, LOW + 8 * MIB)], [(HIGH, HIGH + 4 * MIB)]);
    let bridge = PciAddress::new(0, 0, 1, 0);
    for allocators in MODES {
        let device = PciAddress::new(0, 1, 0, 0);
        let devices = vec![
            bridge_at(bridge, 1, 1),
            prefetchable(device, &[8 * MIB, 4 * MIB]),
        ];
        let (mut pci, _space, free) = root(&low, &high, 0, devices, allocators);
        allocate(&mut pci, &free).unwrap();
        assert_eq!(base(&pci, device, 0), LOW);
        assert_eq!(base(&pci, device, 2), HIGH);
        assert_eq!(memory_window(&pci, bridge), LOW..=LOW + 8 * MIB - 1);
        assert_eq!(prefetchable_window(&pci, bridge), HIGH..=HIGH + 4 * MIB - 1);

        let device = PciAddress::new(0, 0, 2, 0);
        let devices = vec![prefetchable(device, &[8 * MIB, 4 * MIB])];
        let (mut pci, _space, free) = root(&low, &high, 0, devices, allocators);
        allocate(&mut pci, &free).unwrap();
        assert_eq!(base(&pci, device, 0), LOW);
        assert_eq!(base(&pci, device, 2), HIGH);
    }
    for allocators in MODES.into_iter().chain([Allocators::FastPaths]) {
        let mut narrow = bridge_at(bridge, 1, 1);
        narrow.prefetch = PrefetchWindow::Bits32;
        let device = prefetchable(PciAddress::new(0, 1, 0, 0), &[8 * MIB, 4 * MIB]);
        let (mut pci, _space, free) = root(&low, &high, 0, vec![narrow, device], allocators);
        assert_eq!(
            allocate(&mut pci, &free),
            Err(PciEcamError::ResourceExhausted)
        );
    }
}

/// Behind a 64-bit bridge, a bridge whose prefetchable window is 32-bit and
/// a 64-bit one: the first's prefetchable memory below 4 GiB, inside the
/// parent's memory window, while the parent's prefetchable window, above,
/// holds the other's.
#[test]
fn a_low_prefetchable_window_inside_the_parents_memory_window() {
    let parent = PciAddress::new(0, 0, 1, 0);
    let narrow = PciAddress::new(0, 1, 0, 0);
    let wide = PciAddress::new(0, 1, 1, 0);
    let (low_bar, high_bar) = (PciAddress::new(0, 2, 0, 0), PciAddress::new(0, 3, 0, 0));
    for allocators in MODES {
        let mut narrow_bridge = bridge_at(narrow, 2, 2);
        narrow_bridge.prefetch = PrefetchWindow::Bits32;
        let mut low_device = endpoint(low_bar, &[(0, BarType::Memory32, 2 * MIB)]);
        low_device.bars[0].prefetchable = true;
        let devices = vec![
            bridge_at(parent, 1, 3),
            narrow_bridge,
            bridge_at(wide, 3, 3),
            low_device,
            prefetchable(high_bar, &[8 * MIB]),
        ];
        let (low, high) = ([(LOW, LOW + 4 * MIB)], [(HIGH, HIGH + 8 * MIB)]);
        let (mut pci, _space, free) = root(&low, &high, 0, devices, allocators);
        allocate(&mut pci, &free).unwrap();
        let parent_memory = memory_window(&pci, parent);
        let parent_prefetchable = prefetchable_window(&pci, parent);
        assert!(parent_memory.contains(&base(&pci, low_bar, 0)));
        assert!(*parent_prefetchable.start() >= HIGH);
        assert!(parent_prefetchable.contains(&base(&pci, high_bar, 0)));
        assert!(prefetchable_window(&pci, wide).contains(&base(&pci, high_bar, 0)));
    }
}

/// A 32-bit prefetchable window above a 64-bit one keeps what is behind
/// both below 4 GiB.
#[test]
fn a_32bit_window_above_keeps_64bit_bars_below() {
    let device = PciAddress::new(0, 2, 0, 0);
    for allocators in MODES.into_iter().chain([Allocators::FastPaths]) {
        for (low, fits) in [(2 * MIB, false), (4 * MIB, true)] {
            let mut narrow = bridge_at(PciAddress::new(0, 0, 1, 0), 1, 2);
            narrow.prefetch = PrefetchWindow::Bits32;
            let devices = vec![
                narrow,
                bridge_at(PciAddress::new(0, 1, 0, 0), 2, 2),
                prefetchable(device, &[4 * MIB]),
            ];
            let pools = ([(LOW, LOW + low)], [(HIGH, HIGH + 8 * MIB)]);
            let (mut pci, _space, free) = root(&pools.0, &pools.1, 0, devices, allocators);
            let result = allocate(&mut pci, &free);
            match fits {
                true => assert!(result.is_ok() && base(&pci, device, 0) < HIGH),
                false => assert_eq!(result, Err(PciEcamError::ResourceExhausted)),
            }
        }
    }
}

/// A bridge's own BAR is on its primary bus: beside its windows, not in
/// them ([`valid`] checks no window holds a BAR not behind it).
#[test]
fn a_bridges_own_bar_is_beside_its_windows() {
    let address = PciAddress::new(0, 0, 1, 0);
    for allocators in MODES {
        for prefetchable in [false, true] {
            let mut bridge = bridge_at(address, 1, 1);
            bridge.bars[0] = endpoint(address, &[(0, BarType::Memory64, MIB)]).bars[0];
            bridge.bars[0].prefetchable = prefetchable;
            let mut device = endpoint(PciAddress::new(0, 1, 0, 0), &[(0, BarType::Memory64, MIB)]);
            device.bars[0].prefetchable = prefetchable;
            let low = [(LOW, LOW + 2 * MIB)];
            let (mut pci, _space, free) = root(&low, &[], 0, vec![bridge, device], allocators);
            allocate(&mut pci, &free).unwrap();
            assert!(!memory_window(&pci, address).contains(&base(&pci, address, 0)));
        }
    }
}

/// Sibling bridges' windows are 1 MiB each, apart: a 512 KiB BAR behind
/// each needs 2 MiB, not the 1.5 MiB the BARs alone would.
#[test]
fn siblings_do_not_share_padding() {
    for allocators in MODES.into_iter().chain([Allocators::FastPaths]) {
        for (size, fits) in [(0x18_0000, false), (2 * MIB, true)] {
            let mut devices = Vec::new();
            for slot in 1..=2 {
                devices.push(bridge_at(PciAddress::new(0, 0, slot, 0), slot, slot));
                let device = PciAddress::new(0, slot, 0, 0);
                devices.push(endpoint(device, &[(0, BarType::Memory32, 0x8_0000)]));
            }
            let (mut pci, _space, free) = root(&[(LOW, LOW + size)], &[], 0, devices, allocators);
            let result = allocate(&mut pci, &free);
            assert_eq!(result.is_ok(), fits, "{allocators:?} {size:#x}");
        }
    }
}

/// A prefetchable 64-bit BAR goes above 4 GiB only behind a 64-bit
/// prefetchable window; without one, or with a 32-bit one, below.
#[test]
fn what_each_prefetchable_window_can_forward() {
    let bridge = PciAddress::new(0, 0, 1, 0);
    let device = PciAddress::new(0, 1, 0, 0);
    for allocators in MODES {
        for capability in [
            PrefetchWindow::None,
            PrefetchWindow::Bits32,
            PrefetchWindow::Bits64,
        ] {
            for low in [2 * MIB, 4 * MIB] {
                let mut port = bridge_at(bridge, 1, 1);
                port.prefetch = capability;
                let devices = vec![port, prefetchable(device, &[4 * MIB])];
                let pools = ([(LOW, LOW + low)], [(HIGH, HIGH + 4 * MIB)]);
                let (mut pci, _space, free) = root(&pools.0, &pools.1, 0, devices, allocators);
                let result = allocate(&mut pci, &free);
                if low == 4 * MIB {
                    result.unwrap();
                    assert!(base(&pci, device, 0) < HIGH);
                } else if capability == PrefetchWindow::Bits64 {
                    result.unwrap();
                    assert_eq!(base(&pci, device, 0), HIGH);
                } else {
                    assert_eq!(result, Err(PciEcamError::ResourceExhausted));
                }
                if result.is_ok() && capability == PrefetchWindow::None {
                    assert!(memory_window(&pci, bridge).contains(&base(&pci, device, 0)));
                }
            }
        }
    }
}

/// A non-prefetchable 64-bit BAR on the root bus may take the memory above
/// 4 GiB; behind a bridge it goes through the 32-bit memory window, so not.
#[test]
fn a_nonprefetchable_64bit_bar_goes_high_only_on_the_root_bus() {
    for allocators in MODES.into_iter().chain([Allocators::FastPaths]) {
        let direct = PciAddress::new(0, 0, 2, 0);
        let devices = vec![endpoint(direct, &[(0, BarType::Memory64, 4 * MIB)])];
        let (mut pci, _space, free) = root(&[], &[(HIGH, HIGH + 4 * MIB)], 0, devices, allocators);
        allocate(&mut pci, &free).unwrap();
        assert_eq!(base(&pci, direct, 0), HIGH);

        let behind = PciAddress::new(0, 1, 0, 0);
        let devices = vec![
            bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1),
            endpoint(behind, &[(0, BarType::Memory64, 4 * MIB)]),
        ];
        let (mut pci, _space, free) = root(&[], &[(HIGH, HIGH + 4 * MIB)], 0, devices, allocators);
        assert_eq!(
            allocate(&mut pci, &free),
            Err(PciEcamError::ResourceExhausted)
        );
    }
}

/// The search, like the subtree allocator, puts the root bus's 64-bit BARs
/// below 4 GiB where they fit (some devices do not decode above), and above
/// only what does not fit below.
#[test]
fn the_search_puts_root_bus_64bit_bars_low_first() {
    let (low, high) = ([(LOW, LOW + 2 * MIB)], [(HIGH, HIGH + 4 * MIB)]);
    let functions = [1, 2].map(|slot| PciAddress::new(0, 0, slot, 0));
    for (count, below) in [(1, 1), (2, 1)] {
        let devices = functions[..count]
            .iter()
            .map(|&address| endpoint(address, &[(0, BarType::Memory64, 2 * MIB)]))
            .collect();
        let search = Allocators::Search(Budget::FIRMWARE);
        let (mut pci, _space, free) = root(&low, &high, 0, devices, search);
        allocate(&mut pci, &free).unwrap();
        let low_bars = functions[..count]
            .iter()
            .filter(|&&address| base(&pci, address, 0) < HIGH)
            .count();
        assert_eq!(low_bars, below);
    }
}

/// A firmware reservation of one 1 MiB granule splits the pool below
/// 4 GiB: no window spans it. A 2 MiB and a 1 MiB BAR behind one bridge
/// do not fit around it; behind two, they do.
#[test]
fn no_window_spans_a_reserved_granule() {
    let low = [(LOW, LOW + 2 * MIB), (LOW + 3 * MIB, LOW + 4 * MIB)];
    for allocators in MODES.into_iter().chain([Allocators::FastPaths]) {
        let one = vec![
            bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1),
            endpoint(
                PciAddress::new(0, 1, 0, 0),
                &[(0, BarType::Memory32, 2 * MIB), (1, BarType::Memory32, MIB)],
            ),
        ];
        let (mut pci, _space, free) = root(&low, &[], 0, one, allocators);
        assert_eq!(
            allocate(&mut pci, &free),
            Err(PciEcamError::ResourceExhausted)
        );
        let mut two = Vec::new();
        for (slot, size) in [(1, MIB), (2, 2 * MIB)] {
            two.push(bridge_at(PciAddress::new(0, 0, slot, 0), slot, slot));
            two.push(endpoint(
                PciAddress::new(0, slot, 0, 0),
                &[(0, BarType::Memory32, size)],
            ));
        }
        let (mut pci, _space, free) = root(&low, &[], 0, two, allocators);
        allocate(&mut pci, &free).unwrap();
    }
}

/// Sixteen free ranges, each with a 128 KiB slot in its middle and 64 KiB
/// either side: placing the 128 KiB BARs leaves 32 free fragments, more
/// than a pool holds. The subtree allocator, which keeps its free space in
/// pools, finds no room; the search keeps no fragments.
#[test]
fn more_free_fragments_than_a_pool_holds() {
    let low: Vec<_> = (0..MAX_RESOURCE_RANGES as u64)
        .map(|n| LOW + n * MIB + 0x1_0000)
        .map(|base| (base, base + 0x4_0000))
        .collect();
    let devices = || {
        (1..=8)
            .map(|slot| {
                let mut definitions = vec![
                    (0, BarType::Memory32, 0x2_0000),
                    (1, BarType::Memory32, 0x2_0000),
                ];
                definitions.extend((2..6).map(|index| (index, BarType::Memory32, 0x1_0000)));
                endpoint(PciAddress::new(0, 0, slot, 0), &definitions)
            })
            .collect::<Vec<_>>()
    };
    let (mut pci, _space, free) = root(&low, &[], 0, devices(), Allocators::FastPaths);
    assert_eq!(
        allocate(&mut pci, &free),
        Err(PciEcamError::ResourceExhausted)
    );
    for allocators in MODES {
        let (mut pci, _space, free) = root(&low, &[], 0, devices(), allocators);
        allocate(&mut pci, &free).unwrap();
    }
}

/// Small I/O BARs of two functions share one 4 KiB I/O window.
#[test]
fn small_io_bars_share_one_window() {
    let bridge = PciAddress::new(0, 0, 1, 0);
    let functions = [0, 1].map(|function| PciAddress::new(0, 1, 0, function));
    for allocators in MODES {
        let mut devices = vec![bridge_at(bridge, 1, 1)];
        devices.extend(functions.map(|address| endpoint(address, &[(0, BarType::Io, 0x20)])));
        let (mut pci, _space, free) = root(&[], &[], 0x2000, devices, allocators);
        allocate(&mut pci, &free).unwrap();
        assert_eq!(pci.read32(bridge, PCI_IO_BASE) & 0xffff, 0x1010);
        let ports = functions.map(|address| io_bar_base(&pci, address, 0));
        assert_eq!(ports[0].abs_diff(ports[1]), 0x20);
    }
}

/// The Ethanol-X's root bridge 0.0 with 4 KiB of ports: two bridges want
/// an I/O window, one fits. Whichever allocator runs, enumeration fails
/// and nothing is written; with 8 KiB both fit.
#[test]
fn ports_that_do_not_fit_fail_whichever_allocator_runs() {
    for allocators in MODES {
        let (mut pci, _space) = ethanol_x(0x1000);
        pci.allocators = allocators;
        let [pcie1, _, ast2500] = ethanol_x_functions();
        pci.write32(ast2500, PCI_BAR0 + 8, 0x5a5a_5a59);
        pci.write32(pcie1, PCI_IO_BASE, 0x1234);
        assert_eq!(
            pci.allocate_resources(),
            Err(PciEcamError::ResourceExhausted)
        );
        assert_eq!(pci.read32(ast2500, PCI_BAR0 + 8), 0x5a5a_5a59);
        assert_eq!(pci.read32(pcie1, PCI_IO_BASE), 0x1234);
        for device in &pci.devices {
            assert!(!device.programmed);
            assert!(device.bars.iter().all(|bar| bar.fixed || !bar.allocated));
            assert_eq!(pci.read16(device.addr, PCI_COMMAND), 0);
        }
        assert_eq!(pci.roots[0].io_pool.free_bytes(), 0x1000);
        assert_eq!(pci.roots[0].mmio32.free_bytes(), 0x0600_0000);

        let (mut pci, _space) = ethanol_x(0x2000);
        pci.allocators = allocators;
        pci.allocate_resources().unwrap();
        let ranges = [(0xf000_0000, 0xf5ff_ffff), (0x4000, 0x5fff)];
        valid(&pci, &ranges).unwrap();
    }
}

/// Decode follows what was placed, whichever allocator placed it: I/O on
/// for a function with I/O BARs and a bridge with an I/O window or an I/O
/// BAR of its own, off for a bridge without either; closed windows read
/// closed.
#[test]
fn decode_and_empty_windows_whichever_allocator_runs() {
    for allocators in MODES {
        let own_ports = PciAddress::new(0, 0, 2, 0);
        let without = PciAddress::new(0, 0, 3, 0);
        let mixed = PciAddress::new(0, 1, 0, 0);
        let mut own = bridge_at(own_ports, 2, 2);
        own.bars[1] = endpoint(own_ports, &[(1, BarType::Io, 0x10)]).bars[1];
        let mut closed = bridge_at(without, 3, 3);
        closed.original_command = PCI_CMD_IO;
        let devices = vec![
            bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1),
            own,
            closed,
            endpoint(
                mixed,
                &[(0, BarType::Io, 0x20), (1, BarType::Memory32, 0x1000)],
            ),
            endpoint(
                PciAddress::new(0, 2, 0, 0),
                &[(0, BarType::Memory32, 0x1000)],
            ),
        ];
        let (mut pci, _space, free) =
            root(&[(LOW, LOW + 16 * MIB)], &[], 0xf000, devices, allocators);
        pci.write16(without, PCI_COMMAND, PCI_CMD_IO);
        allocate(&mut pci, &free).unwrap();
        let command = |address| pci.read16(address, PCI_COMMAND) & (PCI_CMD_IO | PCI_CMD_MEMORY);
        assert_eq!(command(mixed), PCI_CMD_IO | PCI_CMD_MEMORY);
        assert_eq!(pci.read32(own_ports, PCI_IO_BASE) & 0xffff, 0x00ff);
        assert_eq!(command(own_ports), PCI_CMD_IO | PCI_CMD_MEMORY);
        assert_eq!(command(without), PCI_CMD_MEMORY);
        assert_eq!(pci.read32(without, PCI_MEMORY_BASE), 0x0000_ffff);
        assert_eq!(pci.read32(without, PCI_PREF_MEMORY_BASE), 0x0000_ffff);
        assert_eq!(pci.read32(without, PCI_IO_BASE), 0x00ff);
    }
}

/// A chipset's fixed BAR, in the pool or above it, stays where it is and
/// out of every window; behind a bridge it is refused.
#[test]
fn fixed_bars_whichever_allocator_runs() {
    for allocators in MODES {
        for fixed_base in [0xfed0_0000, LOW + MIB] {
            let chipset = PciAddress::new(0, 0, 0x1f, 0);
            let mut fixed = endpoint(chipset, &[(0, BarType::Memory32, MIB)]);
            fixed.bars[0].allocated = true;
            fixed.bars[0].fixed = true;
            fixed.bars[0].base = fixed_base;
            let devices = vec![
                fixed,
                bridge_at(PciAddress::new(0, 0, 2, 0), 1, 1),
                endpoint(
                    PciAddress::new(0, 1, 0, 0),
                    &[(0, BarType::Memory32, MIB), (1, BarType::Memory32, MIB)],
                ),
            ];
            let (mut pci, _space, _) = root(&[(LOW, LOW + 4 * MIB)], &[], 0, devices, allocators);
            pci.roots[0].mmio32.reserve_range(fixed_base, MIB).unwrap();
            pci.write32(chipset, PCI_BAR0, fixed_base as u32);
            pci.allocate_resources().unwrap();
            assert_eq!(pci.read32(chipset, PCI_BAR0), fixed_base as u32);
            let window = memory_window(&pci, PciAddress::new(0, 0, 2, 0));
            assert!(!window.contains(&fixed_base));
            let free: Vec<_> =
                [(LOW, LOW + 4 * MIB - 1), (fixed_base, fixed_base + MIB - 1)].into();
            valid(&pci, &free).unwrap();
        }
        let (mut pci, _space, _) =
            root(&[(LOW, LOW + 4 * MIB)], &[], 0x1000, Vec::new(), allocators);
        let mut fixed = endpoint(PciAddress::new(0, 1, 0, 0), &[(0, BarType::Io, 0x20)]);
        fixed.bars[0].allocated = true;
        fixed.bars[0].fixed = true;
        for device in [bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1), fixed] {
            assert!(pci.devices.push(device).is_ok());
        }
        assert_eq!(pci.allocate_resources(), Err(PciEcamError::FixedBarInvalid));
    }
}

/// A search out of budget says so, and writes nothing.
#[test]
fn a_search_out_of_budget_says_so() {
    let leaf = PciAddress::new(0, 2, 0, 0);
    let devices = vec![
        bridge_at(PciAddress::new(0, 0, 1, 0), 1, 2),
        bridge_at(PciAddress::new(0, 1, 0, 0), 2, 2),
        endpoint(leaf, &[(0, BarType::Memory32, MIB)]),
    ];
    let (mut pci, _space, _) = root(
        &[(LOW, LOW + 4 * MIB)],
        &[],
        0,
        devices,
        Allocators::Search(Budget::visits(1)),
    );
    pci.write32(leaf, PCI_BAR0, 0x1234_5670);
    assert_eq!(pci.allocate_resources(), Err(PciEcamError::SearchLimit));
    assert_eq!(pci.spent.0, 1);
    assert_eq!(pci.read32(leaf, PCI_BAR0), 0x1234_5670);
    assert!(pci.devices.iter().all(|device| !device.programmed));
    assert_eq!(pci.roots[0].mmio32.free_bytes(), 4 * MIB);
}

/// A root bus 2 MiB BAR whose only aligned slot below 4 GiB is where the
/// bridge's 2 MiB memory window must go, and 748 small 64-bit prefetchable
/// BARs behind the bridge: no layout, but only the root bus says so, after
/// every prefetchable window above 4 GiB packed the 750 BARs. The search
/// stops at its work budget, not hours later, and writes nothing.
fn dense() -> Vec<PciDev> {
    let root_bar = PciAddress::new(0, 0, 2, 0);
    let mut devices = vec![
        endpoint(root_bar, &[(0, BarType::Memory32, 2 * MIB)]),
        bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1),
    ];
    for n in 0..250u8 {
        let address = PciAddress::new(0, 1, n / 8, n % 8);
        let mut device = prefetchable(address, &[0x1000; 3]);
        if n == 0 {
            device.bars[2] = endpoint(address, &[(2, BarType::Memory32, MIB)]).bars[2];
            device.bars[4] = endpoint(address, &[(4, BarType::Memory32, 0x1000)]).bars[4];
        }
        devices.push(device);
    }
    devices
}

#[test]
fn a_dense_search_stops_at_its_work_budget() {
    let low = [(LOW + MIB, LOW + 5 * MIB)];
    let high = [(HIGH, HIGH + 64 * MIB)];
    for allocators in MODES {
        let (mut pci, _space, _) = root(&low, &high, 0, dense(), allocators);
        let root_bar = PciAddress::new(0, 0, 2, 0);
        pci.write32(root_bar, PCI_BAR0, 0x1234_5670);
        assert_eq!(pci.allocate_resources(), Err(PciEcamError::SearchLimit));
        let (visits, work) = pci.spent;
        let budget = Budget::FIRMWARE;
        assert!(visits <= budget.visits, "{visits}");
        assert!(work <= budget.work + solver::MAX_CHARGE, "{work}");
        assert_eq!(pci.read32(root_bar, PCI_BAR0), 0x1234_5670);
        assert!(pci.devices.iter().all(|device| !device.programmed));
    }
}

/// Packing a bus is linear in what is on it for each BAR size there, but
/// for sorting: three times the BARs of one size on the root bus, behind
/// no bridge, cost little more than three times the work, not nine.
#[test]
fn packing_a_dense_bus_is_linear() {
    let work = |functions: u8| {
        let devices: Vec<_> = (0..functions)
            .map(|n| prefetchable(PciAddress::new(0, 0, n / 8, n % 8), &[0x1000; 3]))
            .collect();
        let low = [(LOW, LOW + 4 * MIB)];
        let search = Allocators::Search(Budget::FIRMWARE);
        let (mut pci, _space, free) = root(&low, &[], 0, devices, search);
        allocate(&mut pci, &free).unwrap();
        pci.spent.1
    };
    let (few, many) = (work(80), work(240));
    assert!(many < 4 * few && many < 64 * 720, "{few} {many}");
}

// -----------------------------------------------------------------------
// Against a brute-force oracle
// -----------------------------------------------------------------------

/// A tiny topology: the pools, the bridges (parent, prefetchable window)
/// in parents-first order, and memory BARs (bus: `None` the root bus,
/// 64-bit, prefetchable, size).
#[derive(Debug)]
struct Tiny {
    low: Vec<(u64, u64)>,
    high: Vec<(u64, u64)>,
    bridges: Vec<(Option<usize>, PrefetchWindow)>,
    bars: Vec<(Option<usize>, bool, bool, u64)>,
}

impl Tiny {
    fn generate(rng: &mut Rng) -> Self {
        let start = LOW + (rng.below(4) << 20);
        let size = (1 + rng.below(6)) << 20;
        let low = match rng.below(4) {
            // A reserved granule splits it.
            0 if size > 2 * MIB => {
                let hole = start + ((1 + rng.below(size / MIB - 2)) << 20);
                vec![(start, hole), (hole + MIB, start + size)]
            }
            _ => vec![(start, start + size)],
        };
        let high = match rng.below(3) {
            0 => vec![],
            _ => {
                let start = HIGH + (rng.below(2) << 20);
                vec![(start, start + ((1 + rng.below(4)) << 20))]
            }
        };
        let mut bridges: Vec<(Option<usize>, PrefetchWindow)> = Vec::new();
        for _ in 0..1 + rng.below(4) {
            let parent = match rng.below(3) {
                0 if !bridges.is_empty() => Some(rng.below(bridges.len() as u64) as usize),
                _ => None,
            };
            let parent = parent.filter(|&parent| {
                let mut depth = 0;
                let mut at = Some(parent);
                while let Some(bridge) = at {
                    depth += 1;
                    at = bridges[bridge].0;
                }
                depth < 2
            });
            let prefetch = match rng.below(5) {
                0 => PrefetchWindow::None,
                1 => PrefetchWindow::Bits32,
                _ => PrefetchWindow::Bits64,
            };
            bridges.push((parent, prefetch));
        }
        let mut bars = Vec::new();
        let mut small = false;
        for _ in 0..1 + rng.below(5) {
            let bus = match rng.below(bridges.len() as u64 + 1) {
                0 => None,
                n => Some(n as usize - 1),
            };
            let size = match rng.below(4) {
                0 if !small => {
                    small = true;
                    0x8_0000
                }
                n => MIB << (n % 3),
            };
            bars.push((bus, rng.below(2) == 0, rng.below(3) != 0, size));
        }
        bars.sort_by_key(|bar| core::cmp::Reverse(bar.3));
        Self {
            low,
            high,
            bridges,
            bars,
        }
    }

    /// Bus numbers in parents-first order of the bridges, and each one's
    /// last bus.
    fn buses(&self) -> Vec<(u8, u8)> {
        let mut buses = vec![(0u8, 0u8); self.bridges.len()];
        let mut next = 1u8;
        fn visit(tiny: &Tiny, bridge: usize, next: &mut u8, buses: &mut Vec<(u8, u8)>) {
            buses[bridge].0 = *next;
            *next += 1;
            for child in
                (0..tiny.bridges.len()).filter(|&child| tiny.bridges[child].0 == Some(bridge))
            {
                visit(tiny, child, next, buses);
            }
            buses[bridge].1 = *next - 1;
        }
        for bridge in (0..self.bridges.len()).filter(|&bridge| self.bridges[bridge].0.is_none()) {
            visit(self, bridge, &mut next, &mut buses);
        }
        buses
    }

    fn devices(&self) -> Vec<PciDev> {
        let buses = self.buses();
        let bus = |at: Option<usize>| at.map_or(0, |bridge| buses[bridge].0);
        let mut slots = [0u8; 8];
        let mut slot = |bus: u8| {
            slots[usize::from(bus)] += 1;
            slots[usize::from(bus)]
        };
        let mut devices = Vec::new();
        for (bridge, &(parent, prefetch)) in self.bridges.iter().enumerate() {
            let address = PciAddress::new(0, bus(parent), slot(bus(parent)), 0);
            let mut device = bridge_at(address, buses[bridge].0, buses[bridge].1);
            device.prefetch = prefetch;
            devices.push(device);
        }
        for &(at, wide, prefetchable, size) in &self.bars {
            let kind = if wide {
                BarType::Memory64
            } else {
                BarType::Memory32
            };
            let address = PciAddress::new(0, bus(at), slot(bus(at)), 0);
            let mut device = endpoint(address, &[(0, kind, size)]);
            device.bars[0].prefetchable = prefetchable;
            devices.push(device);
        }
        devices
    }

    /// Whether any layout exists: every BAR at every aligned address and,
    /// behind a bridge, through either window it may take; each window the
    /// 1 MiB hull of what goes through it, through the window of its parent
    /// it chose; then what each bus holds apart, and what the root bus
    /// holds in a range of a pool it may use.
    fn feasible(&self) -> bool {
        let mut at = vec![0u64; self.bars.len()];
        self.place(0, &mut at)
    }

    fn place(&self, n: usize, at: &mut Vec<u64>) -> bool {
        if n == self.bars.len() {
            return self.routes(at);
        }
        let (_, wide, _, size) = self.bars[n];
        let ranges = self
            .low
            .iter()
            .chain(if wide { &self.high[..] } else { &[] });
        for &(start, end) in ranges {
            let mut base = start.next_multiple_of(size);
            while base + size <= end {
                let apart = (0..n).all(|other| {
                    at[other] + self.bars[other].3 <= base || base + size <= at[other]
                });
                if apart {
                    at[n] = base;
                    if self.place(n + 1, at) {
                        return true;
                    }
                }
                base += size;
            }
        }
        false
    }

    /// Whether some forwarding of BARs at `at` is a layout. Choices: a
    /// prefetchable BAR behind a bridge with a prefetchable window goes
    /// through either; a prefetchable window, behind a bridge with one,
    /// into either of its parent's.
    fn routes(&self, at: &[u64]) -> bool {
        let bar_choices: Vec<usize> = (0..self.bars.len())
            .filter(|&n| {
                let (bus, _, prefetchable, _) = self.bars[n];
                prefetchable
                    && bus.is_some_and(|bridge| self.bridges[bridge].1 != PrefetchWindow::None)
            })
            .collect();
        let window_choices: Vec<usize> = (0..self.bridges.len())
            .filter(|&bridge| {
                let (parent, prefetch) = self.bridges[bridge];
                prefetch != PrefetchWindow::None
                    && parent.is_some_and(|parent| self.bridges[parent].1 != PrefetchWindow::None)
            })
            .collect();
        let choices = bar_choices.len() + window_choices.len();
        (0..1u32 << choices).any(|mask| {
            let mut through = vec![false; self.bars.len()];
            let mut into = vec![false; self.bridges.len()];
            for (bit, &n) in bar_choices.iter().enumerate() {
                through[n] = mask >> bit & 1 != 0;
            }
            for (bit, &bridge) in window_choices.iter().enumerate() {
                into[bridge] = mask >> (bar_choices.len() + bit) & 1 != 0;
            }
            self.check(at, &through, &into)
        })
    }

    /// Whether BARs at `at`, those `through` a prefetchable window, and
    /// prefetchable windows `into` their parent's prefetchable window, are
    /// a layout.
    fn check(&self, at: &[u64], through: &[bool], into: &[bool]) -> bool {
        type Span = Option<(u64, u64)>;
        let hull = |a: Span, b: (u64, u64)| Some(a.map_or(b, |a| (a.0.min(b.0), a.1.max(b.1))));
        // Windows [memory, prefetchable] by bridge, deepest first.
        let mut windows: Vec<[Span; 2]> = vec![[None; 2]; self.bridges.len()];
        // What each bus holds: (bus, span).
        let mut held: Vec<(Option<usize>, (u64, u64))> = Vec::new();
        for (n, &(bus, wide, _, size)) in self.bars.iter().enumerate() {
            let span = (at[n], at[n] + size);
            if !wide && span.1 > HIGH {
                return false;
            }
            if let Some(bridge) = bus {
                let kind = usize::from(through[n]);
                windows[bridge][kind] = hull(windows[bridge][kind], span);
            }
            held.push((bus, span));
        }
        for bridge in (0..self.bridges.len()).rev() {
            let (parent, prefetch) = self.bridges[bridge];
            for kind in 0..2 {
                let Some((base, end)) = windows[bridge][kind] else {
                    continue;
                };
                let span = (base & !(MIB - 1), end.next_multiple_of(MIB));
                windows[bridge][kind] = Some(span);
                let limit = match (kind, prefetch) {
                    (0, _) | (_, PrefetchWindow::Bits32) => HIGH,
                    _ => u64::MAX,
                };
                if span.1 > limit {
                    return false;
                }
                held.push((parent, span));
                match parent {
                    Some(parent) => {
                        let kind = usize::from(kind == 1 && into[bridge]);
                        windows[parent][kind] = hull(windows[parent][kind], span);
                    }
                    None => {
                        let high = kind == 1 && prefetch == PrefetchWindow::Bits64;
                        let ranges = self
                            .low
                            .iter()
                            .chain(if high { &self.high[..] } else { &[] });
                        if !ranges
                            .into_iter()
                            .any(|&(start, end)| start <= span.0 && span.1 <= end)
                        {
                            return false;
                        }
                    }
                }
            }
        }
        // Root-bus BARs are in ranges by construction; what a bus holds is
        // apart.
        held.iter().enumerate().all(|(n, &(bus, a))| {
            held[n + 1..]
                .iter()
                .all(|&(other, b)| other != bus || a.1 <= b.0 || b.1 <= a.0)
        })
    }
}

/// On tiny topologies the search without a budget finds a layout exactly
/// when the oracle does, and what it writes decodes as meant; with a small
/// budget it finds one, proves there is none, or says it ran out; firmware's
/// allocators never claim there is none when there is one.
#[test]
fn the_search_finds_a_layout_exactly_when_one_exists() {
    let (mut feasible, mut rescued, mut most) = (0, 0, 0);
    for seed in 0..3000u64 {
        let mut rng = Rng(seed);
        let tiny = Tiny::generate(&mut rng);
        let exists = tiny.feasible();
        feasible += usize::from(exists);
        let run = |allocators| {
            let (mut pci, space, free) = root(&tiny.low, &tiny.high, 0, tiny.devices(), allocators);
            let result = pci.allocate_resources();
            if result.is_ok()
                && let Err(error) = valid(&pci, &free).and_then(|()| typed(&pci, &pools_of(&pci)))
            {
                panic!(
                    "seed {seed} {allocators:?}: {error}\n{tiny:x?}\n{}",
                    describe(&pci, &free)
                );
            }
            drop(space);
            (result, pci.spent.0)
        };
        let (exact, visits) = run(Allocators::Search(Budget::UNLIMITED));
        most = most.max(visits);
        assert_eq!(exact.is_ok(), exists, "seed {seed}: {exact:?}\n{tiny:x?}");
        if !exists {
            assert_eq!(exact, Err(PciEcamError::ResourceExhausted), "seed {seed}");
        }
        for allocators in [
            Allocators::Search(Budget::visits(4)),
            Allocators::Search(Budget::visits(32)),
            Allocators::Both(Budget::FIRMWARE),
        ] {
            match run(allocators).0 {
                Ok(()) => assert!(exists),
                Err(PciEcamError::ResourceExhausted) => {
                    assert!(!exists, "seed {seed} {allocators:?}")
                }
                Err(PciEcamError::SearchLimit) => {}
                Err(error) => panic!("seed {seed} {allocators:?}: {error:?}"),
            }
        }
        let subtrees = run(Allocators::FastPaths).0;
        assert!(subtrees.is_err() || exists);
        rescued += usize::from(exists && subtrees.is_err());
    }
    // Both outcomes, layouts the subtree allocator misses, and the budget
    // well above what any of these needs.
    assert!(
        feasible > 500 && feasible < 2500 && rescued > 0 && most < Budget::FIRMWARE.visits,
        "{feasible} {rescued} {most}"
    );
}

/// A tiny I/O topology: free ports, bridges (parent) in parents-first
/// order, and I/O BARs (bus: `None` the root bus, size).
#[derive(Debug)]
struct TinyIo {
    ports: Vec<(u64, u64)>,
    bridges: Vec<Option<usize>>,
    bars: Vec<(Option<usize>, u64)>,
}

impl TinyIo {
    fn generate(rng: &mut Rng) -> Self {
        let start = 0x1000 + (rng.below(4) << 10);
        let ports = vec![(start, start + ((1 + rng.below(4)) << 12))];
        let mut bridges: Vec<Option<usize>> = Vec::new();
        for _ in 0..1 + rng.below(3) {
            let parent = match rng.below(3) {
                0 if !bridges.is_empty() => Some(rng.below(bridges.len() as u64) as usize),
                _ => None,
            };
            bridges.push(parent.filter(|&parent| bridges[parent].is_none()));
        }
        let mut bars = Vec::new();
        let mut small = false;
        for _ in 0..1 + rng.below(3) {
            let bus = match rng.below(bridges.len() as u64 + 1) {
                0 => None,
                n => Some(n as usize - 1),
            };
            let size = match rng.below(3) {
                0 if !small => {
                    small = true;
                    0x100
                }
                n => 0x400 << (n % 2),
            };
            bars.push((bus, size));
        }
        bars.sort_by_key(|bar| core::cmp::Reverse(bar.1));
        Self {
            ports,
            bridges,
            bars,
        }
    }

    /// The same bridges as a [`Tiny`]: their bus numbers and devices.
    fn topology(&self) -> Tiny {
        Tiny {
            low: Vec::new(),
            high: Vec::new(),
            bridges: self
                .bridges
                .iter()
                .map(|&parent| (parent, PrefetchWindow::Bits64))
                .collect(),
            bars: Vec::new(),
        }
    }

    fn devices(&self) -> Vec<PciDev> {
        let topology = self.topology();
        let buses = topology.buses();
        let mut devices = topology.devices();
        for (slot, &(at, size)) in self.bars.iter().enumerate() {
            let bus = at.map_or(0, |bridge| buses[bridge].0);
            let address = PciAddress::new(0, bus, 16 + slot as u8, 0);
            devices.push(endpoint(address, &[(0, BarType::Io, size)]));
        }
        devices
    }

    /// Whether any layout exists: every BAR at every aligned port; each
    /// window the 4 KiB hull of what is behind it; what each bus holds
    /// apart, and what the root bus holds in a range of the ports.
    fn feasible(&self) -> bool {
        let mut at = vec![0u64; self.bars.len()];
        self.place(0, &mut at)
    }

    fn place(&self, n: usize, at: &mut Vec<u64>) -> bool {
        if n == self.bars.len() {
            return self.check(at);
        }
        let size = self.bars[n].1;
        for &(start, end) in &self.ports {
            let mut base = start.next_multiple_of(size);
            while base + size <= end {
                let apart = (0..n).all(|other| {
                    at[other] + self.bars[other].1 <= base || base + size <= at[other]
                });
                if apart {
                    at[n] = base;
                    if self.place(n + 1, at) {
                        return true;
                    }
                }
                base += size;
            }
        }
        false
    }

    fn check(&self, at: &[u64]) -> bool {
        let mut windows: Vec<Option<(u64, u64)>> = vec![None; self.bridges.len()];
        let mut held: Vec<(Option<usize>, (u64, u64))> = Vec::new();
        let hull = |a: Option<(u64, u64)>, b: (u64, u64)| {
            Some(a.map_or(b, |a| (a.0.min(b.0), a.1.max(b.1))))
        };
        for (n, &(bus, size)) in self.bars.iter().enumerate() {
            let span = (at[n], at[n] + size);
            if let Some(bridge) = bus {
                windows[bridge] = hull(windows[bridge], span);
            }
            held.push((bus, span));
        }
        for bridge in (0..self.bridges.len()).rev() {
            let Some((base, end)) = windows[bridge] else {
                continue;
            };
            let span = (base & !0xfff, end.next_multiple_of(0x1000));
            held.push((self.bridges[bridge], span));
            match self.bridges[bridge] {
                Some(parent) => windows[parent] = hull(windows[parent], span),
                None => {
                    let pooled = |&(start, end): &(u64, u64)| start <= span.0 && span.1 <= end;
                    if !self.ports.iter().any(pooled) {
                        return false;
                    }
                }
            }
        }
        held.iter().enumerate().all(|(n, &(bus, a))| {
            held[n + 1..]
                .iter()
                .all(|&(other, b)| other != bus || a.1 <= b.0 || b.1 <= a.0)
        })
    }
}

/// The same for port I/O: one set, 4 KiB windows.
#[test]
fn the_search_finds_ports_exactly_when_they_exist() {
    let mut feasible = 0;
    for seed in 0..1500u64 {
        let mut rng = Rng(seed);
        let tiny = TinyIo::generate(&mut rng);
        let exists = tiny.feasible();
        feasible += usize::from(exists);
        let run = |allocators| {
            let (mut pci, space, _) = root(&[], &[], 0, tiny.devices(), allocators);
            let mut ports = ResourcePool::empty();
            for &(base, end) in &tiny.ports {
                ports.push_range(base, end).unwrap();
            }
            pci.roots[0].io_pool = ports;
            let free: Vec<_> = tiny
                .ports
                .iter()
                .map(|&(base, end)| (base, end - 1))
                .collect();
            let result = pci.allocate_resources();
            if result.is_ok()
                && let Err(error) = valid(&pci, &free).and_then(|()| typed(&pci, &pools_of(&pci)))
            {
                panic!(
                    "seed {seed} {allocators:?}: {error}\n{tiny:x?}\n{}",
                    describe(&pci, &free)
                );
            }
            drop(space);
            result
        };
        let exact = run(Allocators::Search(Budget::UNLIMITED));
        let proof = if exists {
            Ok(())
        } else {
            Err(PciEcamError::ResourceExhausted)
        };
        assert_eq!(exact, proof, "seed {seed}\n{tiny:x?}");
        match run(Allocators::Both(Budget::FIRMWARE)) {
            Ok(()) => assert!(exists),
            Err(error) => assert!(
                !exists && error == PciEcamError::ResourceExhausted,
                "seed {seed}"
            ),
        }
        assert!(run(Allocators::FastPaths).is_err() || exists);
    }
    assert!(feasible > 300 && feasible < 1300, "{feasible}");
}

/// A root window across 4 GiB, one range as the root describes it, is two
/// pools: below for 32-bit BARs and windows, above for 64-bit ones. A
/// 32-bit BAR on the root bus goes below whether or not there is more above.
#[test]
fn a_root_window_across_4_gib_is_two_pools() {
    use crate::PciRole::Window;
    let endpoint_at = PciAddress::new(0, 0, 1, 0);
    for windows in [
        vec![Window.mmio(0xfff0_0000, MIB)],
        vec![Window.mmio(0xfff0_0000, MIB), Window.mmio(HIGH, MIB)],
        vec![Window.mmio(0xfff0_0000, 2 * MIB)],
    ] {
        for allocators in MODES.into_iter().chain([Allocators::FastPaths]) {
            let (mut pci, _space) = described(&windows);
            pci.allocators = allocators;
            let device = endpoint(endpoint_at, &[(0, BarType::Memory32, MIB)]);
            assert!(pci.devices.push(device).is_ok());
            let free = pools_of(&pci);
            assert_eq!(free[0], [(0xfff0_0000, HIGH - 1)]);
            pci.allocate_resources().unwrap();
            assert_eq!(pci.read32(endpoint_at, PCI_BAR0), 0xfff0_0000);
            typed(&pci, &free).unwrap();
            // The tables still forward it as one range.
            let forwarded: Vec<_> = pci.roots()[0].mmio().collect();
            assert_eq!(forwarded.len(), 1);
        }
    }
}

/// Behind a bridge, below an aperture across 4 GiB: a 32-bit prefetchable
/// BAR below 4 GiB, through the memory window or a prefetchable window
/// below; a 64-bit one above, through the prefetchable window. No window
/// crosses 4 GiB.
#[test]
fn a_32bit_prefetchable_bar_stays_below_an_aperture_across_4_gib() {
    use crate::PciRole::Window;
    let bridge = PciAddress::new(0, 0, 1, 0);
    let device = PciAddress::new(0, 1, 0, 0);
    for allocators in MODES {
        let (mut pci, _space) = described(&[Window.mmio(0xfff0_0000, 3 * MIB)]);
        pci.allocators = allocators;
        let mut behind = endpoint(
            device,
            &[(0, BarType::Memory32, MIB), (2, BarType::Memory64, 2 * MIB)],
        );
        behind.bars[0].prefetchable = true;
        behind.bars[2].prefetchable = true;
        for device in [bridge_at(bridge, 1, 1), behind] {
            assert!(pci.devices.push(device).is_ok());
        }
        let free = pools_of(&pci);
        pci.allocate_resources().unwrap();
        assert_eq!(base(&pci, device, 0), 0xfff0_0000);
        assert_eq!(base(&pci, device, 2), HIGH);
        let all: Vec<_> = free.iter().flatten().copied().collect();
        valid(&pci, &all).unwrap();
        typed(&pci, &free).unwrap();
    }
}

/// The validator holds the same line: a prefetchable window across 4 GiB
/// is no window, even with only a 64-bit BAR, which may be anywhere, in its
/// lower part and a pool that covers it; the BAR through the memory window
/// below is.
#[test]
fn the_validator_refuses_a_window_across_4_gib() {
    let (mut pci, _space) = test_pci_with_buses(3);
    let bridge = PciAddress::new(0, 0, 1, 0);
    let mut device = endpoint(PciAddress::new(0, 1, 0, 0), &[(0, BarType::Memory64, MIB)]);
    device.bars[0].prefetchable = true;
    device.bars[0].base = 0xfff0_0000;
    device.bars[0].allocated = true;
    device.parent = Some(0);
    for device in [bridge_at(bridge, 1, 1), device] {
        assert!(pci.devices.push(device).is_ok());
    }
    // A pool across 4 GiB, as the root's pools were before they were split.
    let mut across = ResourcePool::empty();
    across.push_range(0xfff0_0000, HIGH + MIB).unwrap();
    let pools = (ResourcePool::empty(), across, ResourcePool::empty());
    let open = |pci: &mut PciEcam, kind: WindowKind, base: u64, size: u64| {
        let window = &mut pci.devices[0].windows[kind as usize];
        (window.base, window.size, window.placed) = (base, size, true);
    };
    open(&mut pci, WindowKind::Prefetchable, 0xfff0_0000, 2 * MIB);
    assert!(!pci.validate_plan(0, &pools));
    pci.devices[0].windows = Default::default();
    open(&mut pci, WindowKind::Memory, 0xfff0_0000, MIB);
    let low = ResourcePool::new(0xfff0_0000, MIB).unwrap();
    assert!(pci.validate_plan(0, &(low, ResourcePool::empty(), ResourcePool::empty())));
}

/// [`typed`] reads what it checks: a cleared prefetchable bit, a BAR in
/// the wrong window, decode turned off are each caught.
#[test]
fn typed_layouts_are_read_back() {
    let bridge = PciAddress::new(0, 0, 1, 0);
    let device = PciAddress::new(0, 1, 0, 0);
    let (low, high) = ([(LOW, LOW + 8 * MIB)], [(HIGH, HIGH + 4 * MIB)]);
    let devices = || {
        vec![
            bridge_at(bridge, 1, 1),
            prefetchable(device, &[8 * MIB, 4 * MIB]),
        ]
    };
    let (mut pci, _space, free) = root(&low, &high, 0, devices(), MODES[1]);
    allocate(&mut pci, &free).unwrap();
    let pools = pools_of(&pci);
    for (register, value) in [
        (PCI_BAR0, pci.read32(device, PCI_BAR0) & !0x8),
        (PCI_MEMORY_BASE, 0x0000_ffff),
        (PCI_COMMAND, 0),
    ] {
        let before = pci.read32(device, register);
        let at = if register == PCI_MEMORY_BASE {
            bridge
        } else {
            device
        };
        let saved = pci.read32(at, register);
        pci.write32(at, register, value);
        assert!(typed(&pci, &pools).is_err(), "{register:#x}");
        pci.write32(at, register, saved);
        assert_eq!(pci.read32(device, register), before);
    }
    typed(&pci, &pools).unwrap();
}

/// A root of six buses of RAM config space as `windows` describe it.
fn described(windows: &[crate::PciResource]) -> (PciEcam, Vec<u32>) {
    let mut space = vec![0u32; 6 << 18];
    let info = crate::PciRootInfo {
        segment: 0,
        ecam_base: space.as_mut_ptr() as u64,
        bus_start: 0,
        bus_end: 5,
    };
    let root = PciRoot::new(info, windows.iter().copied()).unwrap();
    (PciEcam::new([root]).unwrap(), space)
}

/// Enumeration that fails to allocate writes no BAR, window or command
/// register, but leaves the buses it numbered: fail-stop.
#[test]
fn a_failed_enumeration_leaves_routes_and_nothing_else() {
    use crate::PciRole::Window;
    let (mut pci, mut space) = described(&[Window.io(0x1000, 0x1000)]);
    let config =
        |bus: usize, device: usize, offset: usize| (bus << 18) + (device << 13) + offset / 4;
    for bus in [0, 1] {
        (0..32).for_each(|device| space[config(bus, device, 0)] = u32::MAX);
    }
    for (bus, header, class) in [(0, 1, 0x0604), (1, 0, 0x0200)] {
        space[config(bus, 0, 0)] = 0x1234_1022;
        space[config(bus, 0, 8)] = class << 16;
        space[config(bus, 0, 0xc)] = header << 16;
    }
    let before = space.clone();
    // Every BAR in RAM sizes as 16 bytes of memory; the root has none.
    assert_eq!(
        pci.enumerate_and_allocate(),
        Err(PciEcamError::ResourceExhausted)
    );
    let bridge = PciAddress::new(0, 0, 0, 0);
    assert_eq!(pci.read32(bridge, PCI_PRIMARY_BUS) & 0xff_ffff, 0x01_0100);
    for (index, (now, was)) in space.iter().zip(&before).enumerate() {
        if index != config(0, 0, usize::from(PCI_PRIMARY_BUS)) {
            assert_eq!(now, was, "config dword {index:#x}");
        }
    }
}
