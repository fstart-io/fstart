//! The ACPI view of a PCI root bridge, from the [`PciRoot`] the allocator
//! was built from and from nothing else.
//!
//! [`crs`] is the host bridge's `_CRS`: what the bridge forwards. An OS
//! takes every address descriptor in it for a window it may give to PCI
//! devices (the consumer/producer bit is ignored in Word, DWord and QWord
//! descriptors), so what the chipset decodes itself is not in it: the
//! configuration ports are a fixed I/O descriptor, and the ECAM window and
//! the chipset's own registers are in [`motherboard`], a `PNP0C02` device
//! next to the bridge.
//!
//! A platform whose tables come from elsewhere (QEMU's own, a device tree)
//! describes its root bridge for the allocator only and uses neither.

use alloc::vec::Vec;
use core::ops::RangeInclusive;

use fstart_pci::{PciRoot, PciWindowKind};

use crate::aml::{
    AddressSpace, AddressSpaceCacheable, EISAName, IO, Memory32Fixed, Name, Path, ResourceTemplate,
};
use crate::aml_linker::device_vec;
use crate::{Aml, AmlError, RawAml};

/// The configuration mechanism #1 ports: the root bridge they are routed
/// to consumes them, and no window may produce them.
const CONFIG_PORTS: RangeInclusive<u16> = 0xcf8..=0xcff;

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

/// `Name(_CRS, ...)` of the host bridge of `root`: its buses, then the I/O
/// and the memory it forwards, in the order of description. A bridge that
/// forwards the I/O around the configuration ports is the one they are
/// routed to, and consumes them.
pub fn crs(root: &PciRoot) -> Vec<u8> {
    let mut resources = Vec::new();
    let buses = root.buses();
    AddressSpace::<u16>::new_bus_number((*buses.start()).into(), (*buses.end()).into())
        .to_aml_bytes(&mut resources);
    let holds_config_ports = |window: RangeInclusive<u16>| {
        window.start() <= CONFIG_PORTS.end() && CONFIG_PORTS.start() <= window.end()
    };
    if root.io().any(holds_config_ports) {
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

/// `Device(name)`, for `\_SB`: the motherboard resources of `root`, what
/// the firmware keeps and the chipset decodes itself. Nothing if it has
/// none.
///
/// For the ECAM window this is what PCI Firmware 3.2, 4.1.2 asks for: the
/// range in MCFG "must be reserved by declaring a motherboard resource. For
/// most systems, the motherboard resource would appear at the root of the
/// ACPI namespace (under `\_SB`) in a node with a _HID of EISAID (PNP0C02),
/// and the resources in this case should not be claimed in the root PCI
/// bus's _CRS." Linux only uses ECAM for the buses one such resource (or
/// the memory map) covers, so the window is one range.
pub fn motherboard(name: &str, root: &PciRoot) -> Result<Vec<u8>, AmlError> {
    let mut resources = Vec::new();
    for range in root.motherboard() {
        let narrow = |value: u64| value.try_into().map_err(|_| AmlError::InvalidOperand);
        match range.kind {
            // The fixed I/O descriptor counts its ports in a byte; a longer
            // range is a Word one.
            PciWindowKind::Io => match u8::try_from(range.size) {
                Ok(ports) => {
                    let base = narrow(range.base)?;
                    IO::new(base, base, 1, ports).to_aml_bytes(&mut resources);
                }
                Err(_) => {
                    let (base, last) = (narrow(range.base)?, narrow(range.end() - 1)?);
                    AddressSpace::<u16>::new_io(base, last, None).to_aml_bytes(&mut resources);
                }
            },
            PciWindowKind::Mmio => match (u32::try_from(range.base), u32::try_from(range.size)) {
                (Ok(base), Ok(size)) if range.is_below_4g() => {
                    Memory32Fixed::new(true, base, size).to_aml_bytes(&mut resources);
                }
                _ => memory(&(range.base..=range.end() - 1), &mut resources),
            },
            _ => return Err(AmlError::InvalidOperand),
        }
    }
    if resources.is_empty() {
        return Ok(Vec::new());
    }
    let mut body = Vec::new();
    Name::new(Path::new("_HID"), &EISAName::new("PNP0C02")).to_aml_bytes(&mut body);
    Name::new(
        Path::new("_CRS"),
        &ResourceTemplate::new(alloc::vec![&RawAml(&resources)]),
    )
    .to_aml_bytes(&mut body);
    device_vec(name, &body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_pci::PciRole::{Forwarded, Kept, Own, Window};
    use fstart_pci::{PciResource, PciRootInfo};

    fn root(resources: &[PciResource]) -> PciRoot {
        PciRoot::new(
            PciRootInfo {
                segment: 0,
                ecam_base: 0xe000_0000,
                bus_start: 0x40,
                bus_end: 0x7f,
            },
            resources.iter().copied(),
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
    fn the_bridge_the_config_ports_are_routed_to_consumes_them() {
        let windows = [
            Window.io(0x1000, 0x3000),
            Window.mmio(0xa000_0000, 0x1000_0000),
            Window.mmio(0x100_0000_0000, 0x40_0000_0000),
        ];
        // The primary forwards the legacy ports too, and the legacy VGA
        // memory: both are in its description, not added here.
        let legacy = [Forwarded.io(0, 0x1000), Forwarded.mmio(0xa_0000, 0x2_0000)];
        let primary = crs(&root(&[&legacy[..], &windows[..]].concat()));
        let other = crs(&root(&windows));
        let vga = 0xa_0000u32.to_le_bytes();
        let config = [0x47, 0x01, 0xf8, 0x0c, 0xf8, 0x0c, 0x01, 0x08];
        assert!(contains(&primary, &vga) && contains(&primary, &config));
        assert!(!contains(&other, &vga) && !contains(&other, &config));
        // The window above 4 GiB is a QWord descriptor with its base.
        assert!(contains(&other, &0x100_0000_0000u64.to_le_bytes()));
        assert!(primary.len() > other.len());
    }

    #[test]
    fn a_motherboard_io_range_of_any_length_has_a_descriptor() {
        let root = root(&[
            Forwarded.io(0, 0x1_0000),
            Own.io(0x80, 0x10),
            Own.io(0x1000, 0x1000),
        ]);
        let board = motherboard("SYSR", &root).unwrap();
        // Up to 255 ports: the fixed I/O descriptor.
        assert!(contains(
            &board,
            &[0x47, 0x01, 0x80, 0x00, 0x80, 0x00, 0x01, 0x10]
        ));
        // More: a Word I/O range, 0x1000-0x1fff.
        let word = [0x00, 0x10, 0xff, 0x1f, 0x00, 0x00, 0x00, 0x10];
        assert!(contains(&board, &word), "{board:x?}");
    }

    /// A DWord memory descriptor from `base` to `limit`.
    fn dword(base: u32, limit: u32) -> Vec<u8> {
        [base.to_le_bytes(), limit.to_le_bytes()].concat()
    }

    /// A Memory32Fixed descriptor.
    fn fixed(base: u32, size: u32) -> Vec<u8> {
        [
            &[0x86, 9, 0, 1][..],
            &base.to_le_bytes(),
            &size.to_le_bytes(),
        ]
        .concat()
    }

    #[test]
    fn crs_and_the_motherboard_device_split_the_description() {
        let root = root(&[
            Window.mmio(0xc000_0000, 0x3ec0_0000),
            Forwarded.mmio(0xc_0000, 0x4_0000),
            Kept.mmio(0xfed4_0000, 0x5000),
            Own.mmio(0xe000_0000, 0x1000_0000),
            Own.mmio(0xfed1_0000, 0x4000),
        ]);
        let (crs, board) = (crs(&root), motherboard("PDRC", &root).unwrap());
        // `_CRS`: the window around the ECAM window and the rest that is
        // forwarded; not what the chipset decodes itself.
        for (base, limit) in [
            (0xc000_0000, 0xdfff_ffff),
            (0xf000_0000, 0xfebf_ffff),
            (0xc_0000, 0xf_ffff),
            (0xfed4_0000, 0xfed4_4fff),
        ] {
            assert!(contains(&crs, &dword(base, limit)), "{base:#x}");
        }
        assert!(!contains(&crs, &0xe000_0000u32.to_le_bytes()));
        assert!(!contains(&crs, &0xfed1_0000u32.to_le_bytes()));
        // The motherboard device: what is kept and what is owned.
        assert!(contains(&board, b"PDRC"));
        for (base, size) in [
            (0xfed4_0000, 0x5000),
            (0xe000_0000, 0x1000_0000),
            (0xfed1_0000, 0x4000),
        ] {
            assert!(contains(&board, &fixed(base, size)), "{base:#x}");
        }
        assert!(!contains(&board, &fixed(0xc_0000, 0x4_0000)));
        // No motherboard resources, no device.
        assert!(
            motherboard("PDRC", &self::root(&[Window.io(0, 0x100)]))
                .unwrap()
                .is_empty()
        );
    }
}
