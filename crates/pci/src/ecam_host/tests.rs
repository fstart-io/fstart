use super::*;
use crate::PciRole::{Forwarded, Kept, Own, Window};
use crate::PciRootInfo;
use alloc::vec;
use alloc::vec::Vec;

pub(super) fn test_pci() -> (PciEcam, Vec<u32>) {
    test_pci_with_buses(3)
}

pub(super) fn test_pci_with_buses(buses: usize) -> (PciEcam, Vec<u32>) {
    let mut config_space = vec![0u32; buses << 18];
    let ecam_base = config_space.as_mut_ptr() as usize;
    let root = Root {
        segment: 0,
        ecam_base,
        ecam_size: buses << 20,
        bus_start: 0,
        bus_end: buses as u8 - 1,
        mmio32: ResourcePool::new(0x8000_0000, 0x1000_0000).unwrap(),
        mmio64: ResourcePool::empty(),
        io_pool: ResourcePool::new(0x1000, 0xf000).unwrap(),
        reserved_through: 0,
        next_bus: 1,
    };
    (PciEcam::with_pools(HVec::from_iter([root])), config_space)
}

fn config_topology_fixture() -> (PciEcam, Vec<u32>) {
    let (pci, mut ram) = test_pci();
    ram.fill(u32::MAX);
    for (addr, id, header, class) in [
        // Unselected silicon host functions must not be scanned.
        (PciAddress::new(0, 0, 0, 0), 0x14a41022, 0x80, 0x06000000),
        (PciAddress::new(0, 0, 0, 1), 0xdeadc0de, 1, 0x06040000),
        (PciAddress::new(0, 0, 1, 0), 0x14781002, 1, 0x06040000),
        (PciAddress::new(0, 1, 0, 0), 0x14791002, 1, 0x06040000),
        (PciAddress::new(0, 2, 0, 0), 0x744c1002, 0x80, 0x03000000),
        (PciAddress::new(0, 2, 0, 1), 0x01001d82, 0, 0x01080200),
    ] {
        let dev = pci.device(addr).unwrap();
        dev.write32(PCI_VENDOR_ID, id);
        dev.write8(PCI_HEADER_TYPE, header);
        dev.write32(PCI_COMMAND, 0xa5a50003);
        dev.write32(crate::PCI_CLASS_REVISION, class);
    }
    pci.write32(PciAddress::new(0, 0, 1, 0), PCI_PRIMARY_BUS, 0x7a000000);
    pci.write32(PciAddress::new(0, 1, 0, 0), PCI_PRIMARY_BUS, 0x6b000000);
    (pci, ram)
}

#[test]
fn a_32_gib_bar_is_one_64_bit_bar_not_a_32_bit_upper_half() {
    // AMD 1002:7460 BAR0: 32 GiB, 64-bit prefetchable.
    assert!(!bar_unimplemented(0x0000_000c, 0));
    assert_eq!(
        size_from_moving_bits(0xffff_fff8_0000_0000, 0xFFFF_FFFF_FFFF_FFF0),
        Ok(0x8_0000_0000)
    );
    // Unimplemented and fixed 32-bit registers stay absent.
    assert!(bar_unimplemented(0, 0));
    assert!(bar_unimplemented(0x0000_0008, 0));
    assert!(!bar_unimplemented(0xffff_c000, 0xffff_c000));
}

#[test]
fn selected_resource_subtree_preserves_owned_bus_prefix_and_defers_dma() {
    let (mut pci, mut ram) = test_pci();
    ram.fill(u32::MAX);
    let retained = PciAddress::new(0, 0, 2, 0);
    let selected = PciAddress::new(0, 0, 1, 1); // no function zero
    let child = PciAddress::new(0, 2, 0, 0);
    for (address, header, class) in [
        (retained, 1, 0x06040000),
        (selected, 1, 0x06040000),
        (child, 0, 0x01080200),
    ] {
        let dev = pci.device(address).unwrap();
        dev.write32(PCI_VENDOR_ID, 0x12341022);
        dev.write8(PCI_HEADER_TYPE, header);
        dev.write32(crate::PCI_CLASS_REVISION, class);
        dev.write32(PCI_COMMAND, 0);
        for index in 0..if header == 1 { 2 } else { 6 } {
            dev.write32(PCI_BAR0 + index * 4, 0);
        }
    }
    pci.write32(retained, PCI_PRIMARY_BUS, 0x00010100);
    pci.write32(selected, PCI_PRIMARY_BUS, 0x7a000000);
    pci.write16(child, PCI_COMMAND, PCI_CMD_BUS_MASTER);
    let before = ram.clone();
    assert_eq!(
        pci.reserve_bus_prefix(1, 0, 1),
        Err(PciEcamError::ConfigError)
    );
    assert_eq!(
        pci.reserve_bus_prefix(0, 0, 3),
        Err(PciEcamError::ConfigError)
    );
    pci.reserve_bus_prefix(0, 0, 1).unwrap();
    pci.disable_auto_bus_master().unwrap();
    // SAFETY: modeled ECAM and read-only callback; journal must retain
    // the allocator's reservation after temporary discovery.
    assert_eq!(
        unsafe { pci.with_config_topology(&[selected], |p| p.devices().count()) }.unwrap(),
        2
    );
    assert_eq!(ram, before);
    assert_eq!(pci.roots[0].next_bus, 2);
    assert_eq!(
        pci.enumerate_root_ports_and_allocate(&[child]),
        Err(PciEcamError::ConfigError)
    );
    assert_eq!(ram, before);
    pci.enumerate_root_ports_and_allocate(&[selected]).unwrap();
    assert_eq!(pci.devices().count(), 2);
    assert_eq!(pci.read32(selected, PCI_PRIMARY_BUS), 0x7a020200);
    assert_eq!(pci.read32(retained, PCI_PRIMARY_BUS), 0x00010100);
    assert_eq!(&ram[1 << 18..2 << 18], &before[1 << 18..2 << 18]);
    for address in [selected, child] {
        assert_eq!(pci.read16(address, PCI_COMMAND) & PCI_CMD_BUS_MASTER, 0);
    }
    assert_eq!(
        pci.reserve_bus_prefix(0, 0, 2),
        Err(PciEcamError::ConfigError)
    );
    assert_eq!(
        pci.disable_auto_bus_master(),
        Err(PciEcamError::ConfigError)
    );
}

#[test]
fn exhausted_retained_prefix_survives_failed_config_scope_without_wraparound() {
    let (mut pci, ram) = config_topology_fixture();
    let before = ram.clone();
    pci.reserve_bus_prefix(0, 0, 2).unwrap();
    pci.reserve_bus_prefix(0, 0, 1).unwrap(); // Cannot shrink ownership.
    // SAFETY: modeled ECAM; no downstream bus remains to assign.
    assert_eq!(
        unsafe { pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |_| ()) },
        Err(PciEcamError::ResourceExhausted)
    );
    assert_eq!(ram, before);
    assert_eq!(pci.roots[0].next_bus, 3);
    assert_eq!(pci.roots[0].reserved_through, 2);
}

#[test]
fn config_only_walks_nested_multifunction_tree_and_restores_every_byte() {
    let (mut pci, ram) = config_topology_fixture();
    let before = ram.clone();
    let found = unsafe {
        pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |view| {
            assert_eq!(
                view.read32(PciAddress::new(0, 0, 1, 0), PCI_PRIMARY_BUS),
                0x7a020100
            );
            assert_eq!(
                view.read32(PciAddress::new(0, 1, 0, 0), PCI_PRIMARY_BUS),
                0x6b020201
            );
            assert!(
                view.devices
                    .iter()
                    .all(|d| d.bars.iter().all(|b| b.bar_type == BarType::None))
            );
            view.devices().collect::<Vec<_>>()
        })
    }
    .unwrap();
    assert_eq!(found.len(), 4);
    assert!(found.contains(&PciAddress::new(0, 2, 0, 1)));
    assert_eq!(ram, before);
    assert!(pci.devices.is_empty());
    assert_eq!(pci.roots[0].next_bus, 1);
    assert_eq!(
        unsafe {
            pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |p| p.devices().count())
        }
        .unwrap(),
        4
    );
    assert_eq!(ram, before);
}

#[test]
fn config_only_selected_root_port_does_not_require_function_zero() {
    let (mut pci, ram) = config_topology_fixture();
    let original = PciAddress::new(0, 0, 1, 0);
    let selected = PciAddress::new(0, 0, 1, 1);
    for reg in (0..256u16).step_by(4) {
        pci.write32(selected, reg, pci.read32(original, reg));
    }
    pci.write32(original, PCI_VENDOR_ID, PCI_VENDOR_INVALID);
    let before = ram.clone();
    assert_eq!(
        unsafe { pci.with_config_topology(&[selected], |p| p.devices().count()) }.unwrap(),
        4
    );
    assert_eq!(ram, before);
    assert_eq!(
        unsafe { pci.with_config_topology(&[], |_| ()) },
        Err(PciEcamError::ConfigError)
    );
}

#[test]
fn pcie_downstream_bus_never_probes_nonzero_device_numbers() {
    let (mut pci, ram) = config_topology_fixture();
    let downstream = pci.device(PciAddress::new(0, 1, 0, 0)).unwrap();
    downstream.write16(crate::PCI_STATUS, 0x10);
    downstream.write8(crate::PCI_CAPABILITIES_PTR, 0x40);
    downstream.write16(0x40, 0x10);
    downstream.write16(0x42, 0x60);
    let impossible = PciAddress::new(0, 2, 1, 0);
    pci.write32(impossible, PCI_VENDOR_ID, 0xdeadbeef);
    pci.device(impossible).unwrap().write8(PCI_HEADER_TYPE, 1);
    let before = ram.clone();
    assert_eq!(
        unsafe {
            pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |p| p.devices().count())
        }
        .unwrap(),
        4
    );
    assert_eq!(ram, before);
}

fn exact_function_fixture() -> (PciEcam, Vec<u32>, [PciAddress; 2]) {
    let (pci, ram) = config_topology_fixture();
    let bridge = PciAddress::new(0, 0, 1, 0);
    let endpoint = PciAddress::new(0, 1, 0, 4);
    let dev = pci.device(endpoint).unwrap();
    dev.write32(PCI_VENDOR_ID, 0x14c91022);
    dev.write8(PCI_HEADER_TYPE, 0);
    dev.write32(crate::PCI_CLASS_REVISION, 0x0c033000);
    dev.write16(PCI_COMMAND, 0);
    for reg in (PCI_BAR0..=PCI_BAR0 + 20).step_by(4) {
        dev.write32(reg, 0);
    }
    for reg in [PCI_BAR0, PCI_BAR0 + 4] {
        pci.write32(bridge, reg, 0);
    }
    (pci, ram, [bridge, endpoint])
}

#[test]
fn exact_config_scope_skips_hidden_function_zero_and_restores_all_bytes() {
    let (mut pci, ram, targets) = exact_function_fixture();
    let before = ram.clone();
    let found = unsafe { pci.with_config_functions(&targets, |p| p.devices().collect::<Vec<_>>()) }
        .unwrap();
    assert_eq!(found.len(), 2);
    assert!(found.contains(&targets[1]));
    assert_eq!(ram, before);
    let absent = [targets[0], PciAddress::new(0, 1, 0, 7)];
    assert_eq!(
        unsafe { pci.with_config_functions(&absent, |_| ()) },
        Err(PciEcamError::ConfigError)
    );
    assert_eq!(ram, before);
}
#[test]
fn exact_resource_scope_allocates_selected_endpoint_without_touching_siblings() {
    let (mut pci, _ram, targets) = exact_function_fixture();
    let hidden = PciAddress::new(0, 1, 0, 0);
    let original = pci.read32(hidden, PCI_PRIMARY_BUS);
    pci.enumerate_functions_and_allocate(&targets).unwrap();
    assert_eq!(pci.devices().count(), 2);
    assert_ne!(pci.read32(targets[1], PCI_BAR0), 0);
    assert_ne!(pci.read16(targets[1], PCI_COMMAND) & PCI_CMD_BUS_MASTER, 0);
    assert_eq!(pci.read32(hidden, PCI_PRIMARY_BUS), original);
}

#[test]
fn config_only_exhaustion_restores_routes_and_resets_metadata() {
    let (mut pci, ram) = config_topology_fixture();
    let before = ram.clone();
    pci.roots[0].bus_end = 1;
    for _ in 0..2 {
        let result = unsafe {
            pci.with_config_topology::<()>(&[PciAddress::new(0, 0, 1, 0)], |_| {
                panic!("incomplete walk must not be inspected")
            })
        };
        assert_eq!(result, Err(PciEcamError::ResourceExhausted));
        assert_eq!(ram, before);
        assert!(pci.devices.is_empty());
        assert_eq!(pci.roots[0].next_bus, 1);
    }
}

#[test]
fn config_only_unwinding_restores_the_hierarchy() {
    extern crate std;
    let (mut pci, ram) = config_topology_fixture();
    let before = ram.clone();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: Result<(), _> = unsafe {
            pci.with_config_topology(&[PciAddress::new(0, 0, 1, 0)], |_| {
                panic!("observer failed")
            })
        };
    }));
    assert!(panic.is_err());
    assert_eq!(ram, before);
}

#[test]
fn route_journal_restores_children_before_parents() {
    let mut ram = [0u32; 16];
    let parent =
        crate::EcamDevice::at_config_base(PciAddress::new(0, 0, 1, 0), ram.as_mut_ptr() as usize);
    // Aliased test registers make restoration order directly observable.
    let child =
        crate::EcamDevice::at_config_base(PciAddress::new(0, 1, 0, 0), ram.as_mut_ptr() as usize);
    let mut routes = RouteJournal(HVec::from_iter([(parent, 0xaabbccdd), (child, 0x11223344)]));
    routes.restore().unwrap();
    assert_eq!(parent.read32(PCI_PRIMARY_BUS), 0xaabbccdd);
}

#[test]
fn subwidth_accesses_preserve_neighboring_config_bytes() {
    let (pci, _memory) = test_pci();
    let device = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    for lane in 0..4 {
        device.write32(0x40, 0xaabb_ccdd);
        device.write8(0x40 + lane, 0x12);
        let shift = lane * 8;
        assert_eq!(device.read8(0x40 + lane), 0x12);
        assert_eq!(
            device.read32(0x40),
            (0xaabb_ccdd & !(0xff << shift)) | (0x12 << shift)
        );
    }
    device.write32(0x40, 0xaabb_ccdd);
    device.write16(0x42, 0x1234);
    assert_eq!(device.read16(0x42), 0x1234);
    assert_eq!(device.read32(0x40), 0x1234_ccdd);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[test]
fn unaligned_loads_and_stores_use_exact_byte_spans() {
    let (pci, _memory) = test_pci();
    let device = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    for offset in 1..4u16 {
        device.write32(0x40, 0xa5a5_a5a5);
        device.write32(0x44, 0xa5a5_a5a5);
        // SAFETY: the entire access is backed by ordinary test RAM.
        unsafe {
            assert_eq!(
                device.try_write32_unaligned(0x40 + offset, 0x1234_5678),
                Some(())
            );
            assert_eq!(
                device.try_read32_unaligned(0x40 + offset),
                Some(0x1234_5678)
            );
        }
        let mut expected = [0xa5; 8];
        let start = usize::from(offset);
        expected[start..start + 4].copy_from_slice(&0x1234_5678u32.to_le_bytes());
        let observed: [u8; 8] = core::array::from_fn(|i| device.read8(0x40 + i as u16));
        assert_eq!(observed, expected);
        assert_eq!(device.try_read32(0x40 + offset), None);
        assert_eq!(device.try_write32(0x40 + offset, 0), None);

        device.write32(0x40, 0xa5a5_a5a5);
        device.write32(0x44, 0xa5a5_a5a5);
        // SAFETY: the entire access is backed by ordinary test RAM.
        unsafe {
            assert_eq!(
                device.try_write16_unaligned(0x40 + offset, 0xcdef),
                Some(())
            );
            assert_eq!(device.try_read16_unaligned(0x40 + offset), Some(0xcdef));
        }
        let mut expected = [0xa5; 8];
        expected[start..start + 2].copy_from_slice(&0xcdefu16.to_le_bytes());
        let observed: [u8; 8] = core::array::from_fn(|i| device.read8(0x40 + i as u16));
        assert_eq!(observed, expected);
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[test]
fn unaligned_config_spans_cannot_cross_function_boundary() {
    let (pci, _memory) = test_pci();
    let device = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    device.write32(0xff8, 0xa5a5_a5a5);
    device.write32(0xffc, 0xa5a5_a5a5);
    // SAFETY: the valid span is test RAM; invalid spans must be rejected
    // before any memory access, including the unmapped segment below.
    unsafe {
        assert_eq!(device.try_write32_unaligned(0xffb, 0x1234_5678), Some(()));
        assert_eq!(device.try_read32_unaligned(0xffb), Some(0x1234_5678));
        assert_eq!(device.try_read16_unaligned(0xfff), None);
        assert_eq!(device.try_write16_unaligned(0xfff, 0), None);
        assert_eq!(device.try_read32_unaligned(0xffd), None);
        assert_eq!(device.try_write32_unaligned(0xffd, 0), None);
        assert_eq!(device.try_read32_unaligned(u16::MAX), None);
        let unmapped = crate::EcamDevice::new_address(PciAddress::new(1, 0, 0, 0));
        assert_eq!(unmapped.try_read16_unaligned(1), None);
        assert_eq!(unmapped.try_write16_unaligned(1, 0), None);
        assert_eq!(unmapped.try_read32_unaligned(1), None);
        assert_eq!(unmapped.try_write32_unaligned(1, 0), None);
    }
    assert_eq!(device.read32(0xff8), 0x78a5_a5a5);
    assert_eq!(device.read32(0xffc), 0xa512_3456);
}

#[test]
fn invalid_accesses_are_not_absent_device_responses() {
    let (mut pci, _memory) = test_pci();
    assert!(pci.device(PciAddress::new(1, 0, 0, 0)).is_none());
    assert!(pci.device(PciAddress::new(0, 3, 0, 0)).is_none());
    let device = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    device.write32(0xffc, u32::MAX);
    assert_eq!(device.try_read8(0xfff), Some(u8::MAX));
    assert_eq!(device.try_read8(0x1000), None);
    assert_eq!(device.try_read16(0xfff), None);
    assert_eq!(device.try_write32(0xffe, 0), None);
    assert_eq!(device.try_write8(0x1000, 0), None);
    assert_eq!(device.read32(0xffc), u32::MAX);
    pci.roots[0].ecam_size = 0xfff; // A truncated function is not a mapped device.
    assert!(pci.device(PciAddress::new(0, 0, 0, 0)).is_none());
}

#[test]
fn bridge_children_distinguish_empty_slots_from_present_ports() {
    let (mut pci, _memory) = test_pci();
    let bridge = PciAddress::new(0, 0, 28, 3);
    assert!(pci.bridge_devices(bridge).is_none());
    pci.devices
        .push(PciDev {
            header_type: PCI_HEADER_TYPE_BRIDGE,
            secondary_bus: 1,
            subordinate_bus: 2,
            ..endpoint(bridge, &[])
        })
        .ok()
        .unwrap();
    assert_eq!(pci.bridge_devices(bridge).unwrap().count(), 0);
    // A sibling's bus or another segment must not occupy this slot.
    pci.devices
        .push(endpoint(PciAddress::new(0, 2, 0, 0), &[]))
        .ok()
        .unwrap();
    pci.devices
        .push(endpoint(PciAddress::new(1, 1, 0, 0), &[]))
        .ok()
        .unwrap();
    assert_eq!(pci.bridge_devices(bridge).unwrap().count(), 0);
    let card = PciAddress::new(0, 1, 0, 0);
    pci.devices.push(endpoint(card, &[])).ok().unwrap();
    assert_eq!(
        pci.bridge_devices(bridge).unwrap().collect::<Vec<_>>(),
        [card]
    );
    assert!(pci.bridge_devices(card).is_none());
    pci.devices[0].secondary_bus = 0;
    assert!(pci.bridge_devices(bridge).is_none());
}

#[test]
fn device_paths_follow_secondary_buses_and_reject_invalid_topology() {
    let (mut pci, _memory) = test_pci();
    let root_bridge = PciAddress::new(0, 0, 8, 1);
    let child_bridge = PciAddress::new(0, 1, 3, 2);
    let gpu = PciAddress::new(0, 2, 0, 1);
    for (address, secondary) in [(root_bridge, 1), (child_bridge, 2)] {
        assert!(
            pci.devices
                .push(PciDev {
                    header_type: PCI_HEADER_TYPE_BRIDGE,
                    secondary_bus: secondary,
                    subordinate_bus: 2,
                    ..endpoint(address, &[])
                })
                .is_ok()
        );
    }
    assert!(pci.devices.push(endpoint(gpu, &[])).is_ok());
    assert_eq!(
        pci.device_path(gpu).unwrap().as_slice(),
        &[root_bridge, child_bridge, gpu]
    );
    assert_eq!(
        pci.device_path(root_bridge).unwrap().as_slice(),
        &[root_bridge]
    );
    assert!(pci.device_path(PciAddress::new(0, 2, 1, 0)).is_none());
    pci.devices[0].secondary_bus = 2; // Ambiguous parent of the GPU.
    assert!(pci.device_path(gpu).is_none());
    pci.devices[0].secondary_bus = 1;
    pci.devices[1].secondary_bus = 0; // Missing parent.
    assert!(pci.device_path(gpu).is_none());
    pci.devices[1].secondary_bus = 2;
    pci.devices[0].addr = PciAddress::new(0, 2, 8, 1); // Cycle: 2 -> 1 -> 2.
    assert!(pci.device_path(gpu).is_none());
}

/// A root port with a bridge to a VGA-compatible display behind it, an
/// accelerator (display, not VGA-compatible), a second VGA display on
/// the root bus and a second root port that already forwards the legacy
/// VGA ranges (a previous owner's choice).
#[test]
fn legacy_vga_goes_to_the_display_along_its_path() {
    let (mut pci, mut ram) = test_pci();
    ram.fill(0);
    let port = PciAddress::new(0, 0, 1, 1);
    let bridge = PciAddress::new(0, 1, 0, 0);
    let bmc = PciAddress::new(0, 2, 0, 0);
    let accelerator = PciAddress::new(0, 0, 2, 0);
    let gpu = PciAddress::new(0, 0, 3, 0);
    let other = PciAddress::new(0, 0, 4, 1);
    for (address, header, class, secondary, control) in [
        (accelerator, 0, 0x0380_0000, 0, 0),
        (port, 1, 0x0604_0000, 1, 0x0400),
        (bridge, 1, 0x0604_0000, 2, 0x0400),
        (bmc, 0, 0x0300_0041, 0, 0),
        (gpu, 0, 0x0300_00c1, 0, 0),
        // VGA Enable and VGA 16-bit decode on, Discard Timer Status
        // (W1C) pending.
        (other, 1, 0x0604_0000, 3, 0x0418),
    ] {
        let dev = pci.device(address).unwrap();
        dev.write8(PCI_HEADER_TYPE, header);
        dev.write32(crate::PCI_CLASS_REVISION, class);
        dev.write16(0x3e, control);
        assert!(
            pci.devices
                .push(PciDev {
                    header_type: header,
                    secondary_bus: secondary,
                    subordinate_bus: secondary.max(2),
                    programmed: true,
                    ..endpoint(address, &[])
                })
                .is_ok()
        );
    }
    // The first VGA-compatible one in scan order, or the one behind
    // the port the board names.
    assert_eq!(pci.legacy_vga_display(None), Some(bmc));
    assert_eq!(pci.legacy_vga_display(Some(port)), Some(bmc));
    assert_eq!(pci.legacy_vga_display(Some(gpu)), Some(gpu));
    assert_eq!(pci.legacy_vga_display(Some(accelerator)), None);
    assert!(!pci.forwards_legacy_vga(bmc));
    assert_eq!(pci.forward_legacy_vga(bmc), Some(0));
    assert!(pci.forwards_legacy_vga(bmc));
    assert!(!pci.forwards_legacy_vga(gpu));
    for address in [port, bridge, bmc] {
        assert_eq!(pci.read16(address, PCI_COMMAND) & 3, 3);
    }
    // VGA Enable on the bridges; their discard timer status (W1C)
    // written as 0.
    for address in [port, bridge] {
        assert_eq!(pci.read16(address, 0x3e), 1 << 3);
    }
    // Off the path: VGA Enable cleared, the 16-bit decode kept, the
    // W1C status written as 0.
    assert_eq!(pci.read16(other, 0x3e), 1 << 4);
    for address in [accelerator, gpu] {
        assert_eq!(pci.read16(address, PCI_COMMAND), 0);
    }
    // Moving the legacy VGA to the root-bus display closes the old path.
    assert_eq!(pci.forward_legacy_vga(gpu), Some(0));
    assert!(pci.forwards_legacy_vga(gpu));
    for address in [port, bridge, other] {
        assert_eq!(pci.read16(address, 0x3e) & (1 << 3), 0);
    }
    assert_eq!(pci.forward_legacy_vga(PciAddress::new(0, 1, 5, 0)), None);
    assert!(pci.forwards_legacy_vga(gpu));
    pci.close_legacy_vga();
    assert!(!pci.forwards_legacy_vga(bmc));
    pci.write16(other, 0x3e, 0x0408);
    pci.close_legacy_vga();
    assert_eq!(pci.read16(other, 0x3e), 0);
}

/// The legacy VGA ports go only through functions whose decode the
/// allocator set up: none before allocation, none while a function on
/// the path has an I/O BAR it did not place; then nothing changes.
#[test]
fn legacy_vga_needs_a_path_the_allocator_programmed() {
    let (mut pci, _config_space) = test_pci();
    let port = PciAddress::new(0, 0, 1, 0);
    let display = PciAddress::new(0, 1, 0, 0);
    pci.write32(display, crate::PCI_CLASS_REVISION, 0x0300_0000);
    for device in [
        bridge_at(port, 1, 1),
        endpoint(
            display,
            &[(0, BarType::Memory32, 0x100_0000), (2, BarType::Io, 0x80)],
        ),
    ] {
        assert!(pci.devices.push(device).is_ok());
    }
    let decode = |pci: &PciEcam, address| pci.read16(address, PCI_COMMAND) & 3;

    assert_eq!(pci.forward_legacy_vga(display), None);
    assert_eq!((decode(&pci, port), decode(&pci, display)), (0, 0));
    assert_eq!(pci.read16(port, 0x3e), 0);

    pci.allocate_resources().unwrap();
    pci.write16(display, PCI_COMMAND, 0);
    pci.devices[1].bars[2].allocated = false;
    assert_eq!(pci.forward_legacy_vga(display), None);
    assert_eq!(decode(&pci, display), 0);
    assert_eq!(pci.read16(port, 0x3e), 0);

    pci.devices[1].bars[2].allocated = true;
    assert_eq!(pci.forward_legacy_vga(display), Some(0));
    assert_eq!(decode(&pci, display), 3);
    assert_eq!(decode(&pci, port), 3);
    assert!(pci.forwards_legacy_vga(display));
}

/// The root the platform routes the legacy VGA memory to reports it.
#[test]
fn the_legacy_vga_root_forwards_the_vga_memory() {
    let info = |bus_start, bus_end| PciRootInfo {
        segment: 0,
        ecam_base: 0xe000_0000,
        bus_start,
        bus_end,
    };
    let root = |bus_start, bus_end, base| {
        PciRoot::new(
            info(bus_start, bus_end),
            [
                Window.mmio(base, 0x1000_0000),
                Forwarded.mmio(0xc_0000, 0x4_0000),
            ],
        )
        .unwrap()
    };
    let mut pci =
        PciEcam::new([root(0, 0x3f, 0xc000_0000), root(0x40, 0x7f, 0xd000_0000)]).unwrap();
    pci.forward_legacy_vga_memory(1).unwrap();
    assert!(
        pci.roots()[0]
            .mmio()
            .all(|range| *range.start() != 0xa_0000)
    );
    // Joined with the option ROM range it touches.
    assert!(
        pci.roots()[1]
            .mmio()
            .any(|range| range == (0xa_0000..=0xf_ffff))
    );
    assert!(pci.forward_legacy_vga_memory(2).is_err());
}

#[test]
fn sibling_bridge_windows_do_not_interleave() {
    let (mut pci, _config_space) = test_pci();
    pci.configure_windows(0xf800_0000, 0x0700_0000, 0, 0, 0x1000, 0xf000)
        .unwrap();
    let bridge = |device, bus| PciDev {
        header_type: PCI_HEADER_TYPE_BRIDGE,
        secondary_bus: bus,
        subordinate_bus: bus,
        ..endpoint(PciAddress::new(0, 0, device, 1), &[])
    };
    // Zeppelin: xHCI's 64-bit BAR behind 7.1, SATA's 32-bit BAR behind 8.1.
    let xhci = PciAddress::new(0, 1, 0, 3);
    let sata = PciAddress::new(0, 2, 0, 2);
    for device in [
        bridge(7, 1),
        bridge(8, 2),
        endpoint(xhci, &[(0, BarType::Memory64, 0x10_0000)]),
        endpoint(sata, &[(5, BarType::Memory32, 0x1000)]),
    ] {
        assert!(pci.devices.push(device).is_ok());
    }

    pci.allocate_resources().unwrap();

    let window = |device| {
        let value = pci.read32(PciAddress::new(0, 0, device, 1), PCI_MEMORY_BASE);
        (value & 0xfff0) << 16..=(value | 0xf_ffff)
    };
    let (xhci_window, sata_window) = (window(7), window(8));
    assert!(xhci_window.end() < sata_window.start());
    assert!(xhci_window.contains(&(pci.read32(xhci, PCI_BAR0) & !0xf)));
    assert!(sata_window.contains(&(pci.read32(sata, PCI_BAR0 + 20) & !0xf)));
}

/// The non-prefetchable memory window of `bridge`, inclusive.
pub(super) fn memory_window(pci: &PciEcam, bridge: PciAddress) -> core::ops::RangeInclusive<u64> {
    let value = pci.read32(bridge, PCI_MEMORY_BASE);
    u64::from((value & 0xfff0) << 16)..=u64::from(value | 0xf_ffff)
}

/// Every bridge's memory window holds its subtree's memory BARs and no
/// other BAR, fixed or movable.
fn assert_windows_hold_only_their_subtrees(pci: &PciEcam) {
    for bridge in pci
        .devices
        .iter()
        .filter(|d| d.header_type == PCI_HEADER_TYPE_BRIDGE)
    {
        let window = memory_window(pci, bridge.addr);
        let subtree = bridge.secondary_bus..=bridge.subordinate_bus;
        for (address, bar) in pci.assigned_memory_bars() {
            let start = bar.base;
            let end = bar.base + bar.size - 1;
            if subtree.contains(&address.bus()) {
                assert!(window.contains(&start) && window.contains(&end));
            } else {
                assert!(
                    end < *window.start() || start > *window.end(),
                    "{address:?}'s BAR at {start:#x} is in {:?}'s window {window:x?}",
                    bridge.addr
                );
            }
        }
    }
}

pub(super) fn bridge_at(address: PciAddress, secondary: u8, subordinate: u8) -> PciDev {
    PciDev {
        header_type: PCI_HEADER_TYPE_BRIDGE,
        secondary_bus: secondary,
        subordinate_bus: subordinate,
        ..endpoint(address, &[])
    }
}

/// The S8040's first IOS: a root port with the AST2600 behind it (a
/// 16 MiB framebuffer, aligned up past the pool's front, and a 256 KiB
/// register BAR), then the internal bridge with the xHCI. Placed BAR by
/// BAR, the xHCI went into the gap below the framebuffer, inside the
/// AST's root port window. Now the AST's window is placed whole, laid
/// out down (the register BAR below the framebuffer, the window ending
/// at a 16 MiB boundary), and the xHCI's window takes the gap below.
#[test]
fn a_later_bus_does_not_fill_an_earlier_bridges_alignment_gap() {
    let (mut pci, _config_space) = test_pci();
    pci.configure_windows(0xc880_0000, 0x0700_0000, 0, 0, 0x1000, 0xf000)
        .unwrap();
    let ast = PciAddress::new(0, 1, 0, 0);
    let xhci = PciAddress::new(0, 2, 0, 4);
    for device in [
        bridge_at(PciAddress::new(0, 0, 5, 3), 1, 1),
        endpoint(
            ast,
            &[
                (0, BarType::Memory32, 0x100_0000),
                (1, BarType::Memory32, 0x4_0000),
            ],
        ),
        bridge_at(PciAddress::new(0, 0, 7, 1), 2, 2),
        endpoint(xhci, &[(0, BarType::Memory64, 0x10_0000)]),
    ] {
        assert!(pci.devices.push(device).is_ok());
    }

    pci.allocate_resources().unwrap();

    assert_eq!(pci.read32(ast, PCI_BAR0) & !0xf, 0xc900_0000);
    assert_eq!(pci.read32(ast, crate::PCI_BAR1) & !0xf, 0xc8fc_0000);
    assert_eq!(pci.read32(xhci, PCI_BAR0) & !0xf, 0xc880_0000);
    assert_eq!(
        memory_window(&pci, PciAddress::new(0, 0, 5, 3)),
        0xc8f0_0000..=0xc9ff_ffff
    );
    assert_windows_hold_only_their_subtrees(&pci);
}

/// A root-bus BAR aligned to the end of the pool leaves the gap below
/// it, where a bridge's window still fits. Every BAR above all the
/// others' did not, and enumeration failed.
#[test]
fn a_bridge_window_fits_below_a_root_bus_bar() {
    let (mut pci, _config_space) = test_pci();
    pci.configure_windows(0xc080_0000, 0x0180_0000, 0, 0, 0x1000, 0xf000)
        .unwrap();
    let vga = PciAddress::new(0, 0, 1, 0);
    let nic = PciAddress::new(0, 1, 0, 0);
    for device in [
        endpoint(vga, &[(0, BarType::Memory32, 0x100_0000)]),
        bridge_at(PciAddress::new(0, 0, 2, 0), 1, 1),
        endpoint(nic, &[(0, BarType::Memory32, 0x10_0000)]),
    ] {
        assert!(pci.devices.push(device).is_ok());
    }

    pci.allocate_resources().unwrap();

    assert_eq!(pci.read32(vga, PCI_BAR0) & !0xf, 0xc100_0000);
    assert_eq!(pci.read32(nic, PCI_BAR0) & !0xf, 0xc080_0000);
    assert_windows_hold_only_their_subtrees(&pci);
}

/// A chipset's fixed BAR, above the pool or inside it, is neither a
/// floor for the BARs behind a bridge nor inside a bridge's window.
#[test]
fn a_fixed_bar_is_not_a_floor_nor_in_a_window() {
    for fixed_base in [0xfed0_0000, 0x8010_0000] {
        let (mut pci, _config_space) = test_pci();
        pci.configure_windows(0x8000_0000, 0x40_0000, 0, 0, 0x1000, 0xf000)
            .unwrap();
        let chipset = PciAddress::new(0, 0, 0x1f, 0);
        pci.add_fixed_bars(&[PciFixedBar {
            address: chipset,
            register: PCI_BAR0,
            kind: PciFixedBarType::Memory32,
            base: fixed_base,
            size: 0x10_0000,
            prefetchable: false,
        }])
        .unwrap();
        let mut fixed = endpoint(chipset, &[(0, BarType::Memory32, 0x10_0000)]);
        fixed.bars[0].allocated = true;
        fixed.bars[0].fixed = true;
        pci.write32(chipset, PCI_BAR0, fixed_base as u32);
        let disk = PciAddress::new(0, 1, 0, 0);
        for device in [
            fixed,
            bridge_at(PciAddress::new(0, 0, 2, 0), 1, 1),
            endpoint(
                disk,
                &[
                    (0, BarType::Memory32, 0x10_0000),
                    (1, BarType::Memory32, 0x10_0000),
                ],
            ),
        ] {
            assert!(pci.devices.push(device).is_ok());
        }

        pci.allocate_resources().unwrap();

        assert_eq!(pci.read32(chipset, PCI_BAR0), fixed_base as u32);
        assert_windows_hold_only_their_subtrees(&pci);
    }
}

/// No window is placed around a fixed BAR behind a bridge.
#[test]
fn a_fixed_bar_behind_a_bridge_is_refused() {
    let (mut pci, _config_space) = test_pci();
    let mut fixed = endpoint(PciAddress::new(0, 1, 0, 0), &[(0, BarType::Io, 0x20)]);
    fixed.bars[0].allocated = true;
    fixed.bars[0].fixed = true;
    assert!(
        pci.devices
            .push(bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1))
            .is_ok()
    );
    assert!(pci.devices.push(fixed).is_ok());
    assert_eq!(pci.allocate_resources(), Err(PciEcamError::FixedBarInvalid));
}

/// Behind nested bridges, each window is its subtree, packed: the
/// largest alignment first, the window rounded to 1 MiB.
#[test]
fn nested_windows_are_sized_from_their_subtrees() {
    let (mut pci, _config_space) = test_pci();
    pci.configure_windows(0x8000_0000, 0x1000_0000, 0, 0, 0x1000, 0xf000)
        .unwrap();
    let upstream = PciAddress::new(0, 0, 1, 0);
    let downstream = PciAddress::new(0, 1, 0, 0);
    let gpu = PciAddress::new(0, 1, 1, 0);
    let disk = PciAddress::new(0, 2, 0, 0);
    let host = PciAddress::new(0, 0, 2, 0);
    for device in [
        bridge_at(upstream, 1, 2),
        bridge_at(downstream, 2, 2),
        endpoint(
            gpu,
            &[
                (0, BarType::Memory32, 0x200_0000),
                (1, BarType::Memory32, 0x1000),
            ],
        ),
        endpoint(disk, &[(0, BarType::Memory32, 0x4000)]),
        endpoint(host, &[(0, BarType::Memory32, 0x1000)]),
    ] {
        assert!(pci.devices.push(device).is_ok());
    }

    pci.allocate_resources().unwrap();

    // Upstream: the 32 MiB BAR, the 1 MiB window of the disk's port,
    // then the 4 KiB BAR, rounded up to 34 MiB.
    assert_eq!(memory_window(&pci, upstream), 0x8000_0000..=0x821f_ffff);
    assert_eq!(pci.read32(gpu, PCI_BAR0), 0x8000_0000);
    assert_eq!(memory_window(&pci, downstream), 0x8200_0000..=0x820f_ffff);
    assert_eq!(pci.read32(disk, PCI_BAR0), 0x8200_0000);
    assert_eq!(pci.read32(gpu, crate::PCI_BAR1), 0x8210_0000);
    assert_eq!(pci.read32(host, PCI_BAR0), 0x8220_0000);
    assert_windows_hold_only_their_subtrees(&pci);
}

/// The Ethanol-X's root bridge 0.0 (2P Milan) as its DF plan routes
/// it: PCI memory 0xF000_0000 + 96 MiB and `ports` of I/O from 0x4000.
/// PCIE1 (60:03.1) with a dual-port NIC, an I/O BAR on each function,
/// comes before the BMC's link: 60:05.2, the AST1150 and the AST2500,
/// its framebuffer, registers and ports.
pub(super) fn ethanol_x(ports: u64) -> (PciEcam, Vec<u32>) {
    const BUSES: usize = 6;
    let mut config_space = vec![0u32; BUSES << 18];
    let root = Root {
        segment: 0,
        ecam_base: config_space.as_mut_ptr() as usize - (0x60 << 20),
        ecam_size: BUSES << 20,
        bus_start: 0x60,
        bus_end: 0x60 + BUSES as u8 - 1,
        mmio32: ResourcePool::new(0xf000_0000, 0x0600_0000).unwrap(),
        mmio64: ResourcePool::new(0x100_2020_0000, 0xdf_5fe0_0000).unwrap(),
        io_pool: ResourcePool::new(0x4000, ports).unwrap(),
        reserved_through: 0,
        next_bus: 0x61,
    };
    let mut pci = PciEcam::with_pools(HVec::from_iter([root]));
    let [pcie1, bmc, ast2500] = ethanol_x_functions();
    let nic = [0, 1].map(|function| PciAddress::new(0, 0x61, 0, function));
    let port = |function| {
        endpoint(
            nic[function],
            &[
                (0, BarType::Memory32, 0x10_0000),
                (2, BarType::Io, 0x20),
                (3, BarType::Memory32, 0x4000),
            ],
        )
    };
    for device in [
        bridge_at(pcie1, 0x61, 0x61),
        port(0),
        port(1),
        bridge_at(bmc, 0x62, 0x63),
        bridge_at(PciAddress::new(0, 0x62, 0, 0), 0x63, 0x63),
        endpoint(
            ast2500,
            &[
                (0, BarType::Memory32, 0x400_0000),
                (1, BarType::Memory32, 0x2_0000),
                (2, BarType::Io, 0x80),
            ],
        ),
        bridge_at(PciAddress::new(0, 0x60, 7, 1), 0x64, 0x64),
        endpoint(
            PciAddress::new(0, 0x64, 0, 2),
            &[
                (2, BarType::Memory32, 0x8_0000),
                (5, BarType::Memory32, 0x2000),
            ],
        ),
    ] {
        assert!(pci.devices.push(device).is_ok());
    }
    (pci, config_space)
}

/// PCIE1, the BMC's link and the AST2500 of [`ethanol_x`].
pub(super) fn ethanol_x_functions() -> [PciAddress; 3] {
    [
        PciAddress::new(0, 0x60, 3, 1),
        PciAddress::new(0, 0x60, 5, 2),
        PciAddress::new(0, 0x63, 0, 0),
    ]
}

/// With the 4 KiB of ports the DF's equal I/O split leaves each of
/// eight root bridges, two bridges want an I/O window and one fits:
/// enumeration fails, and says which; nothing is written, so nothing
/// decodes ports it was not given.
#[test]
fn ports_that_do_not_fit_fail_and_nothing_is_written() {
    let (mut pci, _config_space) = ethanol_x(0x1000);
    let [pcie1, _, ast2500] = ethanol_x_functions();
    pci.write16(ast2500, PCI_COMMAND, 0);
    pci.write32(ast2500, PCI_BAR0 + 8, 0x5a5a_5a59);
    pci.write32(pcie1, PCI_IO_BASE, 0x1234);

    assert_eq!(
        pci.allocate_resources(),
        Err(PciEcamError::ResourceExhausted)
    );

    assert_eq!(pci.read16(ast2500, PCI_COMMAND), 0);
    assert_eq!(pci.read32(ast2500, PCI_BAR0 + 8), 0x5a5a_5a59);
    assert_eq!(pci.read32(pcie1, PCI_IO_BASE), 0x1234);
    for device in &pci.devices {
        assert!(!device.programmed);
        assert!(device.bars.iter().all(|bar| bar.fixed || !bar.allocated));
        assert_eq!(pci.read16(device.addr, PCI_COMMAND), 0);
    }
    assert_eq!(pci.roots[0].io_pool.free_bytes(), 0x1000);
}

/// With 8 KiB, each bridge's I/O window is 4 KiB: the NIC's two 32-byte
/// I/O BARs share PCIE1's, at their own alignment, not 4 KiB each.
#[test]
fn io_bars_behind_a_bridge_share_its_window_at_their_own_alignment() {
    let (mut pci, _config_space) = ethanol_x(0x2000);
    let [pcie1, bmc, ast2500] = ethanol_x_functions();

    pci.allocate_resources().unwrap();

    let io_window = |bridge| {
        let value = pci.read32(bridge, PCI_IO_BASE);
        (u64::from(value & 0xf0) << 8)..=(u64::from(value & 0xf000) | 0xfff)
    };
    let nic = io_window(pcie1);
    assert_eq!(nic.end() - nic.start(), 0xfff);
    let ports: Vec<_> = [0, 1]
        .map(|function| io_bar_base(&pci, PciAddress::new(0, 0x61, 0, function), 2))
        .into();
    assert_eq!(ports[0].abs_diff(ports[1]), 0x20);
    assert!(ports.iter().all(|port| nic.contains(port)));
    let ast = io_window(bmc);
    assert!(ast.contains(&io_bar_base(&pci, ast2500, 2)));
    assert!(ast.end() < nic.start() || nic.end() < ast.start());
    for address in [pcie1, bmc, ast2500] {
        assert_ne!(pci.read16(address, PCI_COMMAND) & PCI_CMD_IO, 0);
    }
    assert_windows_hold_only_their_subtrees(&pci);
}

/// Small memory BARs of two functions behind a bridge share one 1 MiB
/// window, each at its own alignment.
#[test]
fn small_memory_bars_behind_a_bridge_share_one_mebibyte() {
    let (mut pci, _config_space) = test_pci();
    pci.configure_windows(0x8000_0000, 0x10_0000, 0, 0, 0x1000, 0xf000)
        .unwrap();
    let functions = [0, 1].map(|function| PciAddress::new(0, 1, 0, function));
    assert!(
        pci.devices
            .push(bridge_at(PciAddress::new(0, 0, 1, 0), 1, 1))
            .is_ok()
    );
    for address in functions {
        let device = endpoint(
            address,
            &[
                (0, BarType::Memory32, 0x4000),
                (1, BarType::Memory32, 0x1000),
            ],
        );
        assert!(pci.devices.push(device).is_ok());
    }

    pci.allocate_resources().unwrap();

    assert_eq!(
        memory_window(&pci, PciAddress::new(0, 0, 1, 0)),
        0x8000_0000..=0x800f_ffff
    );
    let mut bars: Vec<_> = pci
        .assigned_memory_bars()
        .map(|(_, bar)| bar.base)
        .collect();
    bars.sort_unstable();
    assert_eq!(bars, [0x8000_0000, 0x8000_4000, 0x8000_8000, 0x8000_9000]);
}

/// An I/O BAR on the root bus that does not fit fails the allocation;
/// the function's other I/O BAR and its memory are not written, and its
/// I/O decode stays off.
#[test]
fn a_root_bus_io_bar_without_ports_fails() {
    let (mut pci, _config_space) = test_pci();
    pci.configure_windows(0x8000_0000, 0x10_0000, 0, 0, 0x1000, 0x40)
        .unwrap();
    let addr = PciAddress::new(0, 0, 1, 0);
    let device = endpoint(
        addr,
        &[
            (0, BarType::Io, 0x40),
            (1, BarType::Io, 0x40),
            (2, BarType::Memory32, 0x1000),
        ],
    );
    assert!(pci.devices.push(device).is_ok());

    assert_eq!(
        pci.allocate_resources(),
        Err(PciEcamError::ResourceExhausted)
    );

    for register in [PCI_BAR0, crate::PCI_BAR1, PCI_BAR0 + 8] {
        assert_eq!(pci.read32(addr, register), 0);
    }
    assert_eq!(pci.read16(addr, PCI_COMMAND), 0);
}

/// I/O decode follows what was placed: on for a function with I/O and
/// memory BARs; on a bridge only with an I/O window open or an I/O BAR
/// of its own, off otherwise even if it was on before.
#[test]
fn io_decode_only_where_ports_were_placed() {
    let (mut pci, _config_space) = test_pci();
    pci.configure_windows(0x8000_0000, 0x100_0000, 0, 0, 0x1000, 0xf000)
        .unwrap();
    let with_ports = PciAddress::new(0, 0, 1, 0);
    let own_ports = PciAddress::new(0, 0, 2, 0);
    let without = PciAddress::new(0, 0, 3, 0);
    let mixed = PciAddress::new(0, 1, 0, 0);
    let memory_only = PciAddress::new(0, 2, 0, 0);
    let mut own = bridge_at(own_ports, 2, 2);
    own.bars[1] = endpoint(own_ports, &[(1, BarType::Io, 0x10)]).bars[1];
    let mut closed = bridge_at(without, 3, 3);
    closed.original_command = PCI_CMD_IO;
    for device in [
        bridge_at(with_ports, 1, 1),
        own,
        closed,
        endpoint(
            mixed,
            &[(0, BarType::Io, 0x20), (1, BarType::Memory32, 0x1000)],
        ),
        endpoint(memory_only, &[(0, BarType::Memory32, 0x1000)]),
    ] {
        assert!(pci.devices.push(device).is_ok());
    }
    pci.write16(without, PCI_COMMAND, PCI_CMD_IO);

    pci.allocate_resources().unwrap();

    let command = |address| pci.read16(address, PCI_COMMAND) & (PCI_CMD_IO | PCI_CMD_MEMORY);
    assert_eq!(command(mixed), PCI_CMD_IO | PCI_CMD_MEMORY);
    assert_eq!(command(memory_only), PCI_CMD_MEMORY);
    assert_eq!(command(with_ports), PCI_CMD_IO | PCI_CMD_MEMORY);
    // Its own I/O BAR; behind it only memory, its I/O window closed.
    assert_eq!(pci.read32(own_ports, PCI_IO_BASE) & 0xffff, 0x00ff);
    assert_eq!(command(own_ports), PCI_CMD_IO | PCI_CMD_MEMORY);
    assert_eq!(command(without), PCI_CMD_MEMORY);
    assert!(pci.devices.iter().all(|device| device.programmed));
}

/// The prefetchable window `bridge` decodes, inclusive, its upper
/// halves read as they are.
pub(super) fn prefetchable_window(
    pci: &PciEcam,
    bridge: PciAddress,
) -> core::ops::RangeInclusive<u64> {
    let value = pci.read32(bridge, PCI_PREF_MEMORY_BASE);
    let base = (u64::from(pci.read32(bridge, PCI_PREF_BASE_UPPER32)) << 32)
        | u64::from((value & 0xfff0) << 16);
    let end = (u64::from(pci.read32(bridge, PCI_PREF_LIMIT_UPPER32)) << 32)
        | u64::from(value & 0xfff0_0000)
        | 0xf_ffff;
    base..=end
}

/// Behind one bridge, a 1 MiB BAR and a 4 MiB 32-bit prefetchable one
/// in [0x8030_0000, 0x8080_0000): the memory window takes the first,
/// the prefetchable window, below 4 GiB, the other, as their spans did
/// when BARs were placed one by one. One window for both, aligned to
/// 4 MiB, does not fit.
#[test]
fn a_32bit_prefetchable_bar_takes_the_prefetchable_window_below_4_gib() {
    let (mut pci, _config_space) = test_pci();
    pci.configure_windows(0x8030_0000, 0x50_0000, 0x1_0000_0000, 0x1000_0000, 0, 0)
        .unwrap();
    let bridge = PciAddress::new(0, 0, 1, 0);
    let device = PciAddress::new(0, 1, 0, 0);
    let mut endpoint = endpoint(
        device,
        &[
            (0, BarType::Memory32, 0x10_0000),
            (1, BarType::Memory32, 0x40_0000),
        ],
    );
    endpoint.bars[1].prefetchable = true;
    for device in [bridge_at(bridge, 1, 1), endpoint] {
        assert!(pci.devices.push(device).is_ok());
    }

    pci.allocate_resources().unwrap();

    assert_eq!(pci.read32(device, PCI_BAR0), 0x8030_0000);
    assert_eq!(pci.read32(device, crate::PCI_BAR1), 0x8040_0008);
    assert_eq!(memory_window(&pci, bridge), 0x8030_0000..=0x803f_ffff);
    assert_eq!(prefetchable_window(&pci, bridge), 0x8040_0000..=0x807f_ffff);
}

/// Two 64-bit prefetchable BARs, 8 and 4 MiB, behind one bridge, with 8
/// MiB below 4 GiB and 4 MiB above: the 8 MiB one below through the
/// memory window, the 4 MiB one above through the prefetchable window.
#[test]
fn prefetchable_memory_splits_across_4_gib() {
    let (mut pci, _config_space) = test_pci();
    pci.configure_windows(0x8000_0000, 0x80_0000, 0x1_0000_0000, 0x40_0000, 0, 0)
        .unwrap();
    let bridge = PciAddress::new(0, 0, 1, 0);
    let device = PciAddress::new(0, 1, 0, 0);
    let mut endpoint = endpoint(
        device,
        &[
            (0, BarType::Memory64, 0x80_0000),
            (2, BarType::Memory64, 0x40_0000),
        ],
    );
    endpoint.bars[0].prefetchable = true;
    endpoint.bars[2].prefetchable = true;
    for device in [bridge_at(bridge, 1, 1), endpoint] {
        assert!(pci.devices.push(device).is_ok());
    }

    pci.allocate_resources().unwrap();

    assert_eq!(pci.read32(device, PCI_BAR0), 0x8000_000c);
    assert_eq!(pci.read32(device, crate::PCI_BAR1), 0);
    assert_eq!(pci.read32(device, PCI_BAR0 + 8), 0x0000_000c);
    assert_eq!(pci.read32(device, PCI_BAR0 + 12), 1);
    assert_eq!(memory_window(&pci, bridge), 0x8000_0000..=0x807f_ffff);
    assert_eq!(
        prefetchable_window(&pci, bridge),
        0x1_0000_0000..=0x1_003f_ffff
    );
}

/// A bridge whose prefetchable window is 32-bit keeps the 64-bit
/// prefetchable BARs behind it below 4 GiB and its upper registers
/// unwritten; its parent, 64-bit, forwards that window through its
/// memory window while a sibling's 64-bit BAR, which does not fit
/// below, goes above through its prefetchable window. A bridge without
/// one forwards prefetchable memory through its memory window.
#[test]
fn prefetchable_windows_as_wide_as_the_bridge_has() {
    let (mut pci, _config_space) = test_pci_with_buses(5);
    pci.configure_windows(0x8000_0000, 0x40_0000, 0x1_0000_0000, 0x100_0000, 0, 0)
        .unwrap();
    let parent = PciAddress::new(0, 0, 1, 0);
    let narrow = PciAddress::new(0, 1, 0, 0);
    let wide = PciAddress::new(0, 1, 1, 0);
    let none = PciAddress::new(0, 0, 2, 0);
    let behind_narrow = PciAddress::new(0, 2, 0, 0);
    let behind_wide = PciAddress::new(0, 3, 0, 0);
    let behind_none = PciAddress::new(0, 4, 0, 0);
    let prefetchable = |address, size| {
        let mut device = endpoint(address, &[(0, BarType::Memory64, size)]);
        device.bars[0].prefetchable = true;
        device
    };
    let mut narrow_bridge = bridge_at(narrow, 2, 2);
    narrow_bridge.prefetch = PrefetchWindow::Bits32;
    let mut none_bridge = bridge_at(none, 4, 4);
    none_bridge.prefetch = PrefetchWindow::None;
    for device in [
        bridge_at(parent, 1, 3),
        narrow_bridge,
        bridge_at(wide, 3, 3),
        prefetchable(behind_narrow, 0x10_0000),
        prefetchable(behind_wide, 0x80_0000),
        none_bridge,
        prefetchable(behind_none, 0x10_0000),
    ] {
        assert!(pci.devices.push(device).is_ok());
    }
    for bridge in [narrow, none] {
        pci.write32(bridge, PCI_PREF_BASE_UPPER32, 0xa5a5_a5a5);
        pci.write32(bridge, PCI_PREF_LIMIT_UPPER32, 0xa5a5_a5a5);
    }
    pci.write32(none, PCI_PREF_MEMORY_BASE, 0x5a5a_5a5a);

    pci.allocate_resources().unwrap();

    let base = |address| {
        u64::from(pci.read32(address, PCI_BAR0) & !0xf)
            | u64::from(pci.read32(address, crate::PCI_BAR1)) << 32
    };
    assert!(base(behind_narrow) < 1 << 32);
    assert!(base(behind_wide) >= 1 << 32);
    assert!(base(behind_none) < 1 << 32);
    for bridge in [narrow, none] {
        assert_eq!(pci.read32(bridge, PCI_PREF_BASE_UPPER32), 0xa5a5_a5a5);
        assert_eq!(pci.read32(bridge, PCI_PREF_LIMIT_UPPER32), 0xa5a5_a5a5);
    }
    assert_eq!(pci.read32(none, PCI_PREF_MEMORY_BASE), 0x5a5a_5a5a);
    assert!(memory_window(&pci, none).contains(&base(behind_none)));
    let narrow_window = prefetchable_window(&pci, narrow);
    assert_eq!(*narrow_window.end() >> 32, 0xa5a5_a5a5);
    let narrow_low = (narrow_window.start() & 0xffff_ffff)..=(narrow_window.end() & 0xffff_ffff);
    assert!(narrow_low.contains(&base(behind_narrow)));
    assert!(memory_window(&pci, parent).contains(narrow_low.start()));
    assert!(memory_window(&pci, parent).contains(narrow_low.end()));
    assert!(prefetchable_window(&pci, parent).contains(&base(behind_wide)));
    assert!(prefetchable_window(&pci, wide).contains(&base(behind_wide)));
}

/// A bridge's prefetchable window as enumeration reads it: 64-bit when
/// its base says so and its upper base holds bits, 32-bit when it says
/// 32-bit; a base that reads 0 is tried with ones and put back.
#[test]
fn the_prefetchable_window_is_read_as_linux_reads_it() {
    let (pci, _config_space) = test_pci();
    let bridge = PciAddress::new(0, 0, 1, 0);
    pci.write32(bridge, PCI_PREF_MEMORY_BASE, 0x0001_0001);
    pci.write32(bridge, PCI_PREF_BASE_UPPER32, 0x1234);
    assert_eq!(pci.prefetch_window(bridge), PrefetchWindow::Bits64);
    assert_eq!(pci.read32(bridge, PCI_PREF_BASE_UPPER32), 0x1234);
    pci.write32(bridge, PCI_PREF_MEMORY_BASE, 0x0000_fff0);
    assert_eq!(pci.prefetch_window(bridge), PrefetchWindow::Bits32);
    pci.write32(bridge, PCI_PREF_MEMORY_BASE, 0);
    // RAM holds the ones: a 32-bit window, its register put back.
    assert_eq!(pci.prefetch_window(bridge), PrefetchWindow::Bits32);
    assert_eq!(pci.read32(bridge, PCI_PREF_MEMORY_BASE), 0);
}

pub(super) fn endpoint(addr: PciAddress, definitions: &[(usize, BarType, u64)]) -> PciDev {
    let none = BarInfo {
        bar_type: BarType::None,
        size: 0,
        prefetchable: false,
        reg: 0,
        allocated: false,
        fixed: false,
        base: 0,
        side: Side::Low,
    };
    let mut bars = [none; 6];
    for &(index, bar_type, size) in definitions {
        bars[index] = BarInfo {
            bar_type,
            size,
            prefetchable: false,
            reg: PCI_BAR0 + index as u16 * 4,
            allocated: false,
            fixed: false,
            base: 0,
            side: Side::Low,
        };
    }
    PciDev {
        addr,
        root: 0,
        header_type: 0,
        original_command: 0,
        bars,
        secondary_bus: 0,
        subordinate_bus: 0,
        windows: Default::default(),
        // What a bridge built from it has: plain RAM cannot model the
        // read-only capability bits enumeration probes.
        prefetch: PrefetchWindow::Bits64,
        parent: None,
        programmed: false,
    }
}

pub(super) fn io_bar_base(pci: &PciEcam, addr: PciAddress, index: usize) -> u64 {
    u64::from(pci.read32(addr, PCI_BAR0 + index as u16 * 4) & 0x0000_fffc)
}

#[test]
fn a_root_becomes_its_pools() {
    let root = PciRoot::new(
        PciRootInfo {
            segment: 3,
            ecam_base: 0xe000_0000,
            bus_start: 0x40,
            bus_end: 0x7f,
        },
        [
            Window.mmio(0x8000_0000, 0x1000_0000),
            Window.io(0x1000, 0xf000),
        ],
    )
    .unwrap();
    let pci = PciEcam::new([root.clone()]).unwrap();

    assert_eq!(pci.roots[0].segment, 3);
    assert_eq!(pci.ecam_base(), 0xe000_0000);
    assert_eq!(pci.ecam_size(), 64 * 1024 * 1024);
    assert_eq!(pci.bus_start(), 0x40);
    assert_eq!(pci.bus_end(), 0x7f);
    assert_eq!(pci.roots[0].mmio32.free_bytes(), 0x1000_0000);
    assert_eq!(pci.roots[0].mmio64.free_bytes(), 0);
    assert_eq!(pci.roots[0].io_pool.free_bytes(), 0xf000);
}

#[test]
fn a_root_is_refused_rather_than_cut_short() {
    let info = PciRootInfo {
        segment: 0,
        ecam_base: 0xe000_0000,
        bus_start: 0,
        bus_end: 0xff,
    };
    // Windows that do not touch stay apart: one too many.
    let many = (0..=crate::MAX_PCI_ROOT_WINDOWS as u64)
        .map(|index| Window.mmio(0x8000_0000 + (index << 21), 0x10_0000));
    assert_eq!(
        PciRoot::new(info, many).unwrap_err(),
        PciEcamError::ResourceRangeLimit
    );
    for wrong in [
        Window.io(0xff00, 0x200),
        Window.mmio(u64::MAX - 0xfff, 0x2000),
    ] {
        assert_eq!(
            PciRoot::new(info, [wrong]).unwrap_err(),
            PciEcamError::ConfigError
        );
    }
    let backwards = PciRootInfo {
        bus_start: 1,
        bus_end: 0,
        ..info
    };
    assert!(PciRoot::new(backwards, []).is_err());
}

/// A root bridge of fixed buses with one MMIO window.
fn domain(info: PciRootInfo, mmio: u64) -> PciRoot {
    PciRoot::new(info, [Window.mmio(mmio, 0x1000_0000)]).unwrap()
}

#[test]
fn every_root_allocates_its_buses_and_windows() {
    // Buses 0..=0x41 of test RAM, in which every BAR sizes as 16 bytes
    // of 32-bit memory. Each root has a bridge with an endpoint behind it.
    let mut ecam = vec![0u32; 0x42 << 18];
    let base = ecam.as_mut_ptr() as usize;
    let config = |bus: u8, device: u8, offset: usize| {
        (usize::from(bus) << 18) + (usize::from(device) << 13) + offset / 4
    };
    for bus in [0, 1, 0x40, 0x41] {
        (0..32).for_each(|device| ecam[config(bus, device, 0)] = u32::MAX);
    }
    for (bus, header, class) in [
        (0, 1, 0x0604),
        (1, 0, 0x0200),
        (0x40, 1, 0x0604),
        (0x41, 0, 0x0200),
    ] {
        ecam[config(bus, 0, 0)] = 0x1234_1022;
        ecam[config(bus, 0, 8)] = class << 16;
        ecam[config(bus, 0, 0xc)] = header << 16;
    }
    let root = |bus_start, bus_end, window| {
        let info = PciRootInfo {
            segment: 0,
            ecam_base: base as u64,
            bus_start,
            bus_end,
        };
        domain(info, window)
    };
    let roots = [root(0, 0x3f, 0x8000_0000), root(0x40, 0x7f, 0xa000_0000)];
    assert!(matches!(
        PciEcam::new([root(0, 0x40, 0), root(0x40, 0x7f, 0)]),
        Err(PciEcamError::ConfigError)
    ));
    let mut pci = PciEcam::new(roots.clone()).unwrap();

    pci.enumerate_and_allocate().unwrap();

    let buses = |bus| pci.read32(PciAddress::new(0, bus, 0, 0), PCI_PRIMARY_BUS) & 0xff_ffff;
    assert_eq!((buses(0), buses(0x40)), (0x01_0100, 0x41_4140));
    for (endpoint, window) in [
        (1, 0x8000_0000..0x9000_0000),
        (0x41, 0xa000_0000..0xb000_0000),
    ] {
        let endpoint = PciAddress::new(0, endpoint, 0, 0);
        let bar = u64::from(pci.read32(endpoint, PCI_BAR0) & !0xf);
        assert!(window.contains(&bar), "{endpoint:?}: {bar:#x}");
    }
}

/// A host bridge in the shape of an Intel one: everything that is not
/// DRAM forwarded, the ECAM window and a chipset BAR its own.
fn described() -> PciRoot {
    PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base: 0xe000_0000,
            bus_start: 0,
            bus_end: 0xff,
        },
        [
            Forwarded.io(0, 0x1_0000),
            Window.io(0x1000, 0xf000),
            Forwarded.mmio(0xc_0000, 0x4_0000),
            Window.mmio(0x8000_0000, 0x7ec0_0000),
            Kept.mmio(0xfed4_0000, 0x5000),
            Window.mmio(0x1_0000_0000, 0x1_0000_0000),
            Own.mmio(0xe000_0000, 0x1000_0000),
            Own.mmio(0xfed1_0000, 0x4000),
            // Up against the range that is kept.
            Own.mmio(0xfed2_0000, 0x2_0000),
        ],
    )
    .unwrap()
}

fn spans(ranges: &[PciWindow]) -> Vec<(PciWindowKind, u64, u64)> {
    ranges.iter().map(|r| (r.kind, r.base, r.end())).collect()
}

#[test]
fn the_three_readers_of_a_root() {
    use PciWindowKind::{Io, Mmio};
    let root = described();
    // `_CRS`: all that is forwarded, without what the chipset decodes.
    assert_eq!(
        spans(root.forwarded()),
        [
            (Io, 0, 0x1_0000),
            (Mmio, 0xc_0000, 0x10_0000),
            (Mmio, 0x8000_0000, 0xe000_0000),
            (Mmio, 0xf000_0000, 0xfec0_0000),
            (Mmio, 0xfed4_0000, 0xfed4_5000),
            (Mmio, 0x1_0000_0000, 0x2_0000_0000),
        ]
    );
    // The allocator: the windows without anything kept or owned.
    assert_eq!(
        spans(root.allocatable()),
        [
            (Io, 0x1000, 0x1_0000),
            (Mmio, 0x8000_0000, 0xe000_0000),
            (Mmio, 0xf000_0000, 0xfec0_0000),
            (Mmio, 0x1_0000_0000, 0x2_0000_0000),
        ]
    );
    // The motherboard device: what is kept and what is owned, each as
    // described. The kept range is in a window of the bridge and its
    // neighbour is not: as one range an OS could reserve neither.
    assert_eq!(
        spans(root.motherboard()),
        [
            (Mmio, 0xfed4_0000, 0xfed4_5000),
            (Mmio, 0xe000_0000, 0xf000_0000),
            (Mmio, 0xfed1_0000, 0xfed1_4000),
            (Mmio, 0xfed2_0000, 0xfed4_0000),
        ]
    );
    let mut pci = PciEcam::new([root.clone()]).unwrap();
    let pools = &mut pci.roots[0];
    assert_eq!(pools.io_pool.allocate_aligned(0x100, 0x100), Some(0x1000));
    assert_eq!(pools.mmio32.free_bytes(), 0x6000_0000 + 0x0ec0_0000);
    assert_eq!(pools.mmio64.free_bytes(), 0x1_0000_0000);
}

#[test]
fn ranges_that_touch_are_one_in_the_order_of_description() {
    let info = described().info();
    let root = PciRoot::new(
        info,
        [
            // A routed window, the PCI pools in it and the non-PCI one.
            Forwarded.mmio(0xf000_0000, 0x0ec0_0000),
            Window.mmio(0xf000_0000, 0x0d00_0000),
            Own.mmio(0xfd00_0000, 0x0100_0000),
            Window.io(0x1000, 0x4000),
            Forwarded.io(0, 0x1000),
            // Joins the two above it once it is there.
            Forwarded.mmio(0xb000_0000, 0x1000_0000),
            Forwarded.mmio(0xd000_0000, 0x1000_0000),
            Forwarded.mmio(0xc000_0000, 0x1000_0000),
        ],
    )
    .unwrap();
    use PciWindowKind::{Io, Mmio};
    assert_eq!(
        spans(root.forwarded()),
        [
            (Mmio, 0xf000_0000, 0xfd00_0000),
            (Mmio, 0xfe00_0000, 0xfec0_0000),
            (Io, 0, 0x5000),
            (Mmio, 0xb000_0000, 0xe000_0000),
        ]
    );
    assert_eq!(
        spans(root.allocatable()),
        [(Mmio, 0xf000_0000, 0xfd00_0000), (Io, 0x1000, 0x5000)]
    );
}

/// The agreement of the tables with the allocator: whatever is
/// described, every address the allocator may give out is in a range
/// `_CRS` reports, and in no motherboard resource.
#[test]
fn what_the_allocator_may_use_is_forwarded_and_nobodys() {
    // A small deterministic generator: descriptions of every shape.
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = |bound: u64| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state % bound
    };
    let info = described().info();
    let mut described = 0;
    for _ in 0..20_000 {
        let resources: Vec<_> = (0..next(9))
            .map(|_| {
                let role = [Window, Window, Forwarded, Kept, Own][next(5) as usize];
                let (base, size) = (next(60) << 12, (1 + next(24)) << 12);
                match next(3) {
                    0 => role.io(base % 0xf000, size.min(0x1000)),
                    _ => role.mmio(0x8000_0000 + base, size),
                }
            })
            .collect();
        let Ok(root) = PciRoot::new(info, resources.iter().copied()) else {
            continue;
        };
        described += 1;
        let inside = |part: &PciWindow, whole: &PciWindow| {
            part.kind == whole.kind && whole.base <= part.base && part.end() <= whole.end()
        };
        let overlaps =
            |a: &PciWindow, b: &PciWindow| a.kind == b.kind && a.base < b.end() && b.base < a.end();
        for usable in root.allocatable() {
            assert!(
                root.forwarded().iter().any(|whole| inside(usable, whole)),
                "{usable:x?} not forwarded: {resources:x?}"
            );
            assert!(
                !root.motherboard().iter().any(|kept| overlaps(usable, kept)),
                "{usable:x?} is a motherboard resource: {resources:x?}"
            );
            // And it is what was described: in a window, and off what
            // is kept and owned.
            assert!(
                resources
                    .iter()
                    .any(|r| r.role == Window && overlaps(usable, &r.range))
            );
            for resource in &resources {
                let taken = matches!(resource.role, Kept | Own);
                assert!(!taken || !overlaps(usable, &resource.range));
            }
        }
        // Nothing the chipset decodes itself is forwarded.
        for resource in resources.iter().filter(|r| r.role == Own) {
            assert!(
                !root
                    .forwarded()
                    .iter()
                    .any(|whole| overlaps(whole, &resource.range)),
                "{resource:x?} forwarded: {resources:x?}"
            );
        }
        // The views are disjoint lists.
        for view in [root.forwarded(), root.allocatable()] {
            for (index, range) in view.iter().enumerate() {
                assert!(!view[index + 1..].iter().any(|other| overlaps(range, other)));
            }
        }
    }
    assert!(described > 10_000);
}

/// Two SP3 sockets: eight root bridges with 160 functions between them.
#[test]
fn eight_roots_hold_the_functions_of_two_sockets() {
    const ROOTS: u8 = 8;
    const FUNCTIONS: u8 = 20;
    // One bus of test RAM per root, 0x20 buses apart; every BAR sizes as
    // 16 bytes of 32-bit memory.
    let mut ecam = vec![0u32; (usize::from(ROOTS - 1) * 0x20 + 1) << 18];
    let base = ecam.as_mut_ptr() as usize;
    let config = |bus: u8, device: u8, offset: usize| {
        (usize::from(bus) << 18) + (usize::from(device) << 13) + offset / 4
    };
    for root in 0..ROOTS {
        let bus = root * 0x20;
        (0..32).for_each(|device| ecam[config(bus, device, 0)] = u32::MAX);
        for device in 0..FUNCTIONS {
            ecam[config(bus, device, 0)] = 0x1234_1022;
            ecam[config(bus, device, 8)] = 0x0200 << 16;
        }
    }
    let roots: Vec<PciRoot> = (0..ROOTS)
        .map(|root| {
            let info = PciRootInfo {
                segment: 0,
                ecam_base: base as u64,
                bus_start: root * 0x20,
                bus_end: root * 0x20 + 0x1f,
            };
            domain(info, 0x8000_0000 + u64::from(root) * 0x1000_0000)
        })
        .collect();
    let mut pci = PciEcam::new(roots.clone()).unwrap();

    pci.enumerate_and_allocate().unwrap();

    assert_eq!(
        pci.device_count(),
        usize::from(ROOTS) * usize::from(FUNCTIONS)
    );
    for root in 0..ROOTS {
        let window = 0x8000_0000 + u64::from(root) * 0x1000_0000;
        for device in [0, FUNCTIONS - 1] {
            let function = PciAddress::new(0, root * 0x20, device, 0);
            let bar = u64::from(pci.read32(function, PCI_BAR0) & !0xf);
            assert!(
                (window..window + 0x1000_0000).contains(&bar),
                "{function:?}: {bar:#x}"
            );
        }
    }
}

#[test]
fn a_root_does_not_take_bus_numbers_beyond_its_range() {
    let mut ecam = vec![0u32; 2 << 18];
    (0..32).for_each(|device| ecam[device << 13] = u32::MAX);
    ecam[0] = 0x1234_1022;
    ecam[2] = 0x0604 << 16;
    ecam[3] = 1 << 16;
    let info = PciRootInfo {
        segment: 0,
        ecam_base: ecam.as_mut_ptr() as u64,
        bus_start: 0,
        bus_end: 0,
    };
    let mut pci = PciEcam::new([domain(info, 0x8000_0000)]).unwrap();
    assert_eq!(
        pci.enumerate_and_allocate(),
        Err(PciEcamError::ResourceExhausted)
    );
}

#[test]
fn bar_size_uses_moving_bits_not_the_all_ones_value() {
    // Bit 4 reads as one for both probe values, while bit 5 is the first
    // address bit software can move. A one-mask would incorrectly report
    // 16 bytes; the PCI moving-bit algorithm reports 32.
    let ones = 0xffff_fff1u32;
    let zeroes = 0x0000_0011u32;
    assert_eq!(
        size_from_moving_bits(u64::from(ones ^ zeroes), 0x0000_fffc),
        Ok(0x20)
    );
}

#[test]
fn wc_selection_excludes_gpu_registers_and_non_display_prefetchable_bars() {
    let (mut pci, _config_space) = test_pci();
    let vga = PciAddress::new(0, 0, 2, 0);
    let storage = PciAddress::new(0, 0, 3, 0);
    for (address, class) in [(vga, 0x0300_0000), (storage, 0x0108_0000)] {
        let mut device = endpoint(
            address,
            &[
                (0, BarType::Memory32, 0x10_0000),
                (2, BarType::Memory32, 0x100_0000),
            ],
        );
        device.bars[2].prefetchable = true;
        assert!(pci.devices.push(device).is_ok());
        pci.write32(address, 8, class);
    }
    pci.allocate_resources().unwrap();
    let windows: HVec<_, 6> = pci
        .prefetchable_vga_windows()
        .map(|window| (window.base, window.size))
        .collect();
    assert_eq!(
        windows.as_slice(),
        &[(u64::from(pci.read32(vga, PCI_BAR0 + 8) & !0xf), 0x100_0000)]
    );
}

#[test]
fn assigned_bar_inspection_includes_io_without_changing_memory_consumers() {
    let (mut pci, _space) = test_pci();
    pci.configure_windows(0xb0000000, 0x1000000, 0, 0, 0x4000, 0x4000)
        .unwrap();
    let address = PciAddress::new(0, 0, 1, 0);
    assert!(
        pci.devices
            .push(endpoint(
                address,
                &[(0, BarType::Memory32, 0x100000), (1, BarType::Io, 0x80)]
            ))
            .is_ok()
    );
    pci.allocate_resources().unwrap();
    let all: HVec<_, 6> = pci.assigned_bars().collect();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].1.kind, PciWindowKind::Mmio);
    assert_eq!(all[1].1.kind, PciWindowKind::Io);
    assert_eq!((all[1].1.base, all[1].1.size), (0x4000, 0x80));
    assert_eq!(pci.assigned_memory_bars().count(), 1);
    assert_eq!(pci.assigned_memory_windows().count(), 1);
}

#[test]
fn graphics_mmio_bar_reuses_gap_before_aligned_framebuffer() {
    let (mut pci, _config_space) = test_pci();
    // X61's aperture runs from TOLUD (3 GiB) to ECAM (3.5 GiB).
    pci.configure_windows(0xc000_0000, 0x2000_0000, 0, 0, 0x1000, 0xf000)
        .unwrap();
    let igd = PciAddress::new(0, 0, 2, 0);
    let hda = PciAddress::new(0, 0, 0x1b, 0);
    assert!(
        pci.devices
            .push(endpoint(
                igd,
                &[
                    (0, BarType::Memory64, 0x10_0000),
                    (2, BarType::Memory64, 0x1000_0000),
                ],
            ))
            .is_ok()
    );
    assert!(
        pci.devices
            .push(endpoint(hda, &[(0, BarType::Memory32, 0x4000)]))
            .is_ok()
    );

    pci.allocate_resources().unwrap();

    assert_eq!(pci.read32(hda, PCI_BAR0) & !0xf, 0xc000_0000);
    assert_eq!(pci.read32(igd, PCI_BAR0 + 8) & !0xf, 0xd000_0000);
    assert_eq!(pci.read32(igd, PCI_BAR0) & !0xf, 0xc010_0000);
    let windows: HVec<_, 6> = pci
        .assigned_memory_windows()
        .map(|window| (window.base, window.size))
        .collect();
    assert_eq!(
        windows.as_slice(),
        &[
            (0xc010_0000, 0x10_0000),
            (0xd000_0000, 0x1000_0000),
            (0xc000_0000, 0x4000),
        ]
    );
}

#[test]
fn firmware_io_reservations_exclude_dynamic_bars() {
    let (mut pci, _memory) = test_pci();
    pci.configure_windows(0x8000_0000, 0x1000_0000, 0, 0, 0x15c0, 0x200)
        .unwrap();
    // EC/PMH7, battery interface and both dock-controller windows.
    let reserved = [
        (0x15e0u64, 16u64),
        (0x1600, 1),
        (0x1602, 1),
        (0x1604, 1),
        (0x1606, 1),
        (0x1610, 16),
        (0x1620, 16),
        (0x164c, 4),
        (0x1680, 16),
    ];
    for (base, size) in reserved {
        pci.reserve_io_range(base, size).unwrap();
    }
    assert!(pci.reserve_io_range(0xffff, 2).is_err());
    for index in 0..28 {
        let size = if index < 24 { 16 } else { 4 };
        assert!(
            pci.devices
                .push(endpoint(
                    PciAddress::new(0, 0, index + 1, 0),
                    &[(0, BarType::Io, size)]
                ))
                .is_ok()
        );
    }
    pci.allocate_resources().unwrap();
    for device in &pci.devices {
        let base = io_bar_base(&pci, device.addr, 0);
        let size = device.bars[0].size;
        assert!(base >= 0x15c0 && base + size <= 0x17c0);
        for (start, length) in reserved {
            assert!(
                base + size <= start || start + length <= base,
                "BAR overlaps firmware I/O"
            );
        }
    }
}

#[test]
fn exact_bar_query_requires_that_registers_allocation_not_an_aliasing_base() {
    let (mut pci, _memory) = test_pci();
    let addr = PciAddress::new(0, 0, 0x14, 2);
    let mut dev = endpoint(
        addr,
        &[
            (0, BarType::Memory64, 0x1000),
            (2, BarType::Memory64, 0x2000),
            (4, BarType::Io, 32),
        ],
    );
    dev.bars[0].allocated = true;
    dev.bars[4].allocated = true;
    pci.write32(addr, 0x10, 0x10000004);
    pci.write32(addr, 0x14, 1);
    pci.write32(addr, 0x18, 0x10000004); // identical full base, but unallocated
    pci.write32(addr, 0x1c, 1);
    assert!(pci.devices.push(dev).is_ok());
    let window = pci.assigned_memory_bar(addr, 0x10).unwrap();
    assert_eq!(window.base, 0x110000000);
    assert_eq!(window.size, 0x1000);
    for register in [0x14, 0x18, 0x1c, 0x20, 0x24] {
        assert_eq!(pci.assigned_memory_bar(addr, register), None);
    }
    assert_eq!(
        pci.assigned_memory_bars().collect::<Vec<_>>(),
        vec![(addr, window)]
    );
}

#[test]
fn fixed_bar_is_preserved_without_consuming_dynamic_space() {
    let (mut pci, _config_space) = test_pci();
    let fixed = PciFixedBar {
        address: PciAddress::new(0, 0, 0x1f, 3),
        register: PCI_BAR0 + 4 * 4,
        kind: PciFixedBarType::Io,
        base: 0x1040,
        size: 0x20,
        prefetchable: false,
    };
    pci.add_fixed_bars(&[fixed]).unwrap();

    let io_pool = &mut pci.roots[0].io_pool;
    assert_eq!(io_pool.allocate_aligned(0x20, 0x20), Some(0x1000));
    assert_eq!(io_pool.allocate_aligned(0x20, 0x20), Some(0x1020));
    assert_eq!(io_pool.allocate_aligned(0x20, 0x20), Some(0x1060));
}

#[test]
fn fixed_bar_probe_preserves_bar_and_restores_original_decode_policy() {
    let (mut pci, _config_space) = test_pci();
    let smbus = PciAddress::new(0, 0, 0x1f, 3);
    pci.write32(smbus, PCI_VENDOR_ID, 0x27da_8086);
    pci.device(smbus).unwrap().write8(PCI_HEADER_TYPE, 0);
    pci.write16(smbus, PCI_COMMAND, PCI_CMD_IO);
    pci.write32(smbus, PCI_BAR0 + 4 * 4, 0x401);
    pci.add_fixed_bars(&[PciFixedBar {
        address: smbus,
        register: PCI_BAR0 + 4 * 4,
        kind: PciFixedBarType::Io,
        base: 0x400,
        size: 0x20,
        prefetchable: false,
    }])
    .unwrap();

    let dev = pci.probe_device(smbus, true).unwrap().unwrap();
    assert_ne!(pci.read16(smbus, PCI_COMMAND) & PCI_CMD_IO, 0);
    assert!(dev.bars[4].fixed);
    assert_eq!(io_bar_base(&pci, smbus, 4), 0x400);
    assert!(pci.devices.push(dev).is_ok());

    pci.allocate_resources().unwrap();

    assert_ne!(pci.read16(smbus, PCI_COMMAND) & PCI_CMD_IO, 0);
    assert_eq!(io_bar_base(&pci, smbus, 4), 0x400);
}

#[test]
fn barless_device_retains_non_bar_decode_policy() {
    let (mut pci, _config_space) = test_pci();
    let lpc = PciAddress::new(0, 0, 0x1f, 0);
    let mut dev = endpoint(lpc, &[]);
    dev.original_command = PCI_CMD_IO | PCI_CMD_MEMORY;
    assert!(pci.devices.push(dev).is_ok());

    pci.allocate_resources().unwrap();

    let command = pci.read16(lpc, PCI_COMMAND);
    assert_eq!(
        command & (PCI_CMD_IO | PCI_CMD_MEMORY),
        PCI_CMD_IO | PCI_CMD_MEMORY
    );
}

#[test]
fn host_bridge_chipset_registers_are_not_probed_as_bars() {
    let (pci, _config_space) = test_pci();
    let host = PciAddress::new(0, 0, 0, 0);
    pci.write32(host, PCI_VENDOR_ID, 0xa000_8086);
    pci.write32(host, 0x08, 0x0600_0000);
    pci.device(host).unwrap().write8(PCI_HEADER_TYPE, 0);
    pci.write16(host, PCI_COMMAND, PCI_CMD_MEMORY);
    pci.write32(host, PCI_BAR0, 0xdead_beef);

    let dev = pci.probe_device(host, true).unwrap().unwrap();

    assert!(dev.bars.iter().all(|bar| bar.bar_type == BarType::None));
    assert_eq!(pci.read32(host, PCI_BAR0), 0xdead_beef);
    assert_eq!(pci.read16(host, PCI_COMMAND), PCI_CMD_MEMORY);
}

#[test]
fn fixed_bar_probe_rejects_live_base_mismatch() {
    let (mut pci, _config_space) = test_pci();
    let smbus = PciAddress::new(0, 0, 0x1f, 3);
    pci.write32(smbus, PCI_VENDOR_ID, 0x27da_8086);
    pci.device(smbus).unwrap().write8(PCI_HEADER_TYPE, 0);
    pci.write32(smbus, PCI_BAR0 + 4 * 4, 0x421);
    pci.add_fixed_bars(&[PciFixedBar {
        address: smbus,
        register: PCI_BAR0 + 4 * 4,
        kind: PciFixedBarType::Io,
        base: 0x400,
        size: 0x20,
        prefetchable: false,
    }])
    .unwrap();

    assert!(matches!(
        pci.probe_device(smbus, true),
        Err(PciEcamError::FixedBarInvalid)
    ));
}

#[test]
fn d41s_fixed_smbus_does_not_displace_sata_bar() {
    let (mut pci, _config_space) = test_pci();
    let igd = PciAddress::new(0, 0, 0x02, 0);
    let sata = PciAddress::new(0, 0, 0x1f, 2);
    let smbus = PciAddress::new(0, 0, 0x1f, 3);

    pci.add_fixed_bars(&[PciFixedBar {
        address: smbus,
        register: PCI_BAR0 + 4 * 4,
        kind: PciFixedBarType::Io,
        base: 0x400,
        size: 0x20,
        prefetchable: false,
    }])
    .unwrap();

    assert!(
        pci.devices
            .push(endpoint(igd, &[(1, BarType::Io, 0x8)]))
            .is_ok()
    );
    for function in 0..4 {
        assert!(
            pci.devices
                .push(endpoint(
                    PciAddress::new(0, 0, 0x1d, function),
                    &[(4, BarType::Io, 0x20)],
                ))
                .is_ok()
        );
    }
    assert!(
        pci.devices
            .push(endpoint(
                sata,
                &[
                    (0, BarType::Io, 0x8),
                    (1, BarType::Io, 0x4),
                    (2, BarType::Io, 0x8),
                    (3, BarType::Io, 0x4),
                    (4, BarType::Io, 0x20),
                ],
            ))
            .is_ok()
    );
    let mut smbus_dev = endpoint(smbus, &[]);
    smbus_dev.bars[4] = BarInfo {
        bar_type: BarType::Io,
        size: 0x20,
        prefetchable: false,
        reg: PCI_BAR0 + 4 * 4,
        allocated: true,
        fixed: true,
        base: 0,
        side: Side::Low,
    };
    assert!(pci.devices.push(smbus_dev).is_ok());
    pci.write32(smbus, PCI_BAR0 + 4 * 4, 0x401);

    pci.allocate_resources().unwrap();

    assert_eq!(io_bar_base(&pci, smbus, 4), 0x400);
    assert_eq!(io_bar_base(&pci, sata, 4), 0x1080);

    let mut intervals = Vec::new();
    for dev in &pci.devices {
        for (index, bar) in dev.bars.iter().enumerate() {
            if bar.bar_type != BarType::Io || !bar.allocated {
                continue;
            }
            let base = io_bar_base(&pci, dev.addr, index);
            intervals.push((base, base + bar.size));
        }
    }
    for left in 0..intervals.len() {
        for right in left + 1..intervals.len() {
            assert!(
                intervals[left].1 <= intervals[right].0 || intervals[right].1 <= intervals[left].0,
                "overlapping intervals: {:?} and {:?}",
                intervals[left],
                intervals[right]
            );
        }
    }
}

#[test]
fn conflicting_fixed_bars_are_rejected() {
    let (mut pci, _config_space) = test_pci();
    let first = PciFixedBar {
        address: PciAddress::new(0, 0, 1, 0),
        register: PCI_BAR0,
        kind: PciFixedBarType::Io,
        base: 0x1000,
        size: 0x20,
        prefetchable: false,
    };
    let second = PciFixedBar {
        address: PciAddress::new(0, 0, 2, 0),
        register: PCI_BAR0,
        kind: PciFixedBarType::Io,
        base: 0x1010,
        size: 0x10,
        prefetchable: false,
    };
    pci.add_fixed_bars(&[first]).unwrap();
    assert_eq!(
        pci.add_fixed_bars(&[second]),
        Err(PciEcamError::FixedBarConflict)
    );
}

#[test]
fn unmanaged_console_preserves_live_resources_without_losing_siblings() {
    let (mut pci, mut config) = test_pci();
    config.fill(u32::MAX);
    let console = PciAddress::new(0, 0, 3, 0);
    let sibling = PciAddress::new(0, 0, 3, 1);
    pci.write32(console, 0, 0xa127_8086);
    pci.device(console).unwrap().write8(PCI_HEADER_TYPE, 0);
    pci.write32(console, 8, 0x0700_0201);
    pci.write16(console, PCI_COMMAND, PCI_CMD_MEMORY);
    pci.write32(console, PCI_BAR0, 0x8000_0004);
    pci.write32(console, PCI_BAR0 + 4, 0);
    pci.write32(sibling, 0, 0x1234_8086);
    pci.device(sibling).unwrap().write8(PCI_HEADER_TYPE, 0);
    pci.write32(sibling, 8, 0x0600_0000); // barless host function
    pci.write16(sibling, PCI_COMMAND, 0);
    pci.add_fixed_bars(&[PciFixedBar {
        address: console,
        register: PCI_BAR0,
        kind: PciFixedBarType::Memory64,
        base: 0x8000_0000,
        size: 0x1000,
        prefetchable: false,
    }])
    .unwrap();
    pci.exclude_functions(&[console]).unwrap();
    let start = 3 << 13;
    let before = config[start..start + 1024].to_vec();
    pci.enumerate_and_allocate().unwrap();
    assert_eq!(pci.devices().collect::<Vec<_>>(), vec![sibling]);
    assert_eq!(&config[start..start + 1024], before.as_slice());
    assert_eq!(pci.assigned_memory_bar(console, PCI_BAR0), None);
    assert_eq!(
        pci.roots[0].mmio32.allocate_aligned(0x1000, 0x1000),
        Some(0x8000_1000)
    );
    assert_eq!(
        pci.exclude_functions(&[sibling]),
        Err(PciEcamError::ConfigError)
    );
    let (mut hidden, mut hidden_bytes) = test_pci();
    hidden_bytes.fill(u32::MAX);
    hidden.write32(sibling, 0, 0x1234_8086);
    hidden.device(sibling).unwrap().write8(PCI_HEADER_TYPE, 0);
    hidden.write32(sibling, 8, 0x0600_0000);
    hidden.write16(sibling, PCI_COMMAND, 0);
    hidden.exclude_functions(&[console]).unwrap();
    hidden.enumerate_and_allocate().unwrap();
    assert_eq!(hidden.devices().collect::<Vec<_>>(), vec![sibling]);
    // An empty scan also closes exclusion registration: device_count is
    // not a substitute for the software enumeration lifecycle.
    let (mut empty, mut bytes) = test_pci();
    bytes.fill(u32::MAX);
    empty.enumerate_and_allocate().unwrap();
    assert_eq!(
        empty.exclude_functions(&[console]),
        Err(PciEcamError::ConfigError)
    );
}
