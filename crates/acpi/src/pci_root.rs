//! The ACPI view of a PCI root bridge, from the same [`PciRoot`] the
//! allocator was built from.
//!
//! `_CRS` holds what the bridge forwards to PCI. An OS takes every address
//! descriptor in a host bridge's `_CRS` for such a window (the
//! consumer/producer bit is ignored in Word, DWord and QWord descriptors),
//! so what the bridge consumes itself is not in it: the configuration ports
//! are a fixed I/O descriptor, and the ECAM window is a motherboard
//! resource next to the bridge ([`ecam_reservation`]). The parts of the
//! windows the firmware reserved are forwarded all the same; the platform
//! describes those under the devices that own them.

use alloc::vec::Vec;
use core::ops::RangeInclusive;

use fstart_pci::PciRoot;

use crate::aml::{
    AddressSpace, AddressSpaceCacheable, EISAName, IO, Memory32Fixed, Name, Path, ResourceTemplate,
};
use crate::aml_linker::device_vec;
use crate::{Aml, AmlError, RawAml};

/// The configuration mechanism #1 ports: the primary root bridge consumes
/// them and no window may produce them.
const CONFIG_PORTS: RangeInclusive<u16> = 0xcf8..=0xcff;
/// The legacy VGA memory, routed to the primary root bridge.
const VGA: RangeInclusive<u32> = 0xa_0000..=0xb_ffff;

/// The parts of `window` outside the configuration ports.
fn around_config_ports(window: RangeInclusive<u16>) -> impl Iterator<Item = RangeInclusive<u16>> {
    let below = *window.start()..=(*window.end()).min(CONFIG_PORTS.start() - 1);
    let above = (*window.start()).max(CONFIG_PORTS.end() + 1)..=*window.end();
    [below, above].into_iter().filter(|range| !range.is_empty())
}

/// A DWord memory window, or a QWord one if it reaches above 4 GiB.
fn memory(range: &RangeInclusive<u64>, out: &mut Vec<u8>) {
    let cacheable = AddressSpaceCacheable::NotCacheable;
    match u32::try_from(*range.end()) {
        Ok(end) => {
            AddressSpace::<u32>::new_memory(cacheable, true, *range.start() as u32, end, None)
                .to_aml_bytes(out)
        }
        Err(_) => {
            AddressSpace::<u64>::new_memory(cacheable, true, *range.start(), *range.end(), None)
                .to_aml_bytes(out)
        }
    }
}

/// `Name(_CRS, ...)` of the host bridge of `root`: its buses, then its I/O
/// and MMIO windows. The `primary` bridge also claims the configuration
/// ports and the legacy VGA memory.
pub fn crs(root: &PciRoot, primary: bool) -> Vec<u8> {
    let mut resources = Vec::new();
    let buses = root.buses();
    AddressSpace::<u16>::new_bus_number((*buses.start()).into(), (*buses.end()).into())
        .to_aml_bytes(&mut resources);
    if primary {
        IO::new(
            *CONFIG_PORTS.start(),
            *CONFIG_PORTS.start(),
            1,
            CONFIG_PORTS.len() as u8,
        )
        .to_aml_bytes(&mut resources);
    }
    for window in root.io().flat_map(around_config_ports) {
        AddressSpace::<u16>::new_io(*window.start(), *window.end(), None)
            .to_aml_bytes(&mut resources);
    }
    if primary {
        memory(
            &(u64::from(*VGA.start())..=u64::from(*VGA.end())),
            &mut resources,
        );
    }
    for window in root.mmio() {
        memory(&window, &mut resources);
    }
    let mut out = Vec::new();
    Name::new(
        Path::new("_CRS"),
        &ResourceTemplate::new(alloc::vec![&RawAml(&resources)]),
    )
    .to_aml_bytes(&mut out);
    out
}

/// `Device(PERC)`, for `\_SB`: the ECAM window `base..base + size` as a
/// motherboard resource.
///
/// PCI Firmware 3.2, 4.1.2: the range in MCFG "must be reserved by declaring
/// a motherboard resource. For most systems, the motherboard resource would
/// appear at the root of the ACPI namespace (under `\_SB`) in a node with a
/// _HID of EISAID (PNP0C02), and the resources in this case should not be
/// claimed in the root PCI bus's _CRS." Linux does not use ECAM without it
/// unless the memory map reserves the range.
pub fn ecam_reservation(base: u64, size: u64) -> Result<Vec<u8>, AmlError> {
    let end = base.checked_add(size).ok_or(AmlError::InvalidOperand)?;
    if size == 0 {
        return Err(AmlError::InvalidOperand);
    }
    let mut resources = Vec::new();
    match (u32::try_from(base), u32::try_from(size)) {
        (Ok(base), Ok(size)) => Memory32Fixed::new(true, base, size).to_aml_bytes(&mut resources),
        _ => memory(&(base..=end - 1), &mut resources),
    }
    let mut body = Vec::new();
    Name::new(Path::new("_HID"), &EISAName::new("PNP0C02")).to_aml_bytes(&mut body);
    Name::new(
        Path::new("_CRS"),
        &ResourceTemplate::new(alloc::vec![&RawAml(&resources)]),
    )
    .to_aml_bytes(&mut body);
    device_vec("PERC", &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_pci::{PciRootInfo, PciWindow, PciWindowKind};

    fn root(windows: &[(PciWindowKind, u64, u64)]) -> PciRoot {
        PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: 0xe000_0000,
                bus_start: 0x40,
                bus_end: 0x7f,
            },
            windows.iter().map(|&(kind, base, size)| PciWindow {
                kind,
                base,
                size,
                prefetchable: false,
            }),
        )
        .unwrap()
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn config_ports_are_cut_out_of_the_io_windows() {
        let parts = |window| around_config_ports(window).collect::<Vec<_>>();
        assert_eq!(parts(0..=0xfff), [0..=0xcf7, 0xd00..=0xfff]);
        assert_eq!(parts(0x1000..=0x1fff), [0x1000..=0x1fff]);
        assert_eq!(parts(0xcf8..=0xcff), []);
    }

    #[test]
    fn the_primary_bridge_claims_the_config_ports_and_vga() {
        let root = root(&[
            (PciWindowKind::Io, 0x1000, 0x3000),
            (PciWindowKind::Mmio, 0xa000_0000, 0x1000_0000),
            (PciWindowKind::Mmio, 0x100_0000_0000, 0x40_0000_0000),
        ]);
        let (primary, other) = (crs(&root, true), crs(&root, false));
        let vga = 0xa_0000u32.to_le_bytes();
        let config = [0x47, 0x01, 0xf8, 0x0c, 0xf8, 0x0c, 0x01, 0x08];
        assert!(contains(&primary, &vga) && contains(&primary, &config));
        assert!(!contains(&other, &vga) && !contains(&other, &config));
        // The window above 4 GiB is a QWord descriptor with its base.
        assert!(contains(&other, &0x100_0000_0000u64.to_le_bytes()));
        assert!(primary.len() > other.len());
    }
}
