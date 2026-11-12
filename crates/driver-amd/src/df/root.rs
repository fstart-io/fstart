// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PCI root complex of a die as a PCI root bridge: what the DF routes
//! to it and what that is for. The allocator and the ACPI host bridge of
//! the die both take it from here.
use super::map::RootComplex;
use fstart_pci::PciRole::{Forwarded, Own, Window};
use fstart_pci::{PciRoot, PciRootInfo};

/// Where the legacy LPC/FCH I/O ends: forwarded like the rest, not for PCI
/// devices.
const LEGACY_IO: u64 = 0x1000;
/// The IOAPIC, HPET and FCH fixed MMIO and everything above.
const FIXED_MMIO: u64 = 0xfec0_0000;
/// The top 12 GiB of the 48-bit address space are the fabric's own and
/// cannot hold MMIO (coreboot `DF_RESERVED_TOP_12GB_MMIO_SIZE`).
const FABRIC_RESERVED: u64 = (1 << 48) - (12 << 30);

/// A die's root complex as a PCI root bridge.
pub type Root = PciRoot;

/// The root bridge of the root complex `routes`, with its buses in the ECAM
/// `ecam`.
///
/// What the DF routes to the die is forwarded: its I/O, the legacy ports
/// to the FCH's fixed devices and the rest to PCI, and its MMIO, below the
/// fixed MMIO and below the fabric's reserved top; on the die with bus 0
/// the legacy VGA memory too. Two ranges of that are the die's own and not
/// forwarded: `non_pci`, the pool of the NBIO's own MMIO, and, on the die
/// with bus 0, the ECAM window.
///
/// `None` if the buses are outside the ECAM or the DF routes more than a
/// root bridge holds.
pub fn root(
    routes: &RootComplex,
    (ecam_base, ecam_size): (u64, u64),
    (non_pci_base, non_pci_size): (u64, u64),
) -> Option<Root> {
    let last_bus = u8::try_from((ecam_size >> 20).checked_sub(1)?).ok()?;
    let buses = *routes.buses.start()..=(*routes.buses.end()).min(last_bus);
    if buses.is_empty() {
        return None;
    }
    let io = routes.io.iter().flat_map(|&(base, size)| {
        let end = (base + size).min(0x1_0000);
        let pci = base.max(LEGACY_IO);
        [
            Forwarded.io(base, end.saturating_sub(base)),
            Window.io(pci, end.saturating_sub(pci)),
        ]
    });
    // The windows below 4 GiB first: the allocator fills them in this order.
    let below = |&&(base, size): &&(u64, u64)| base + size <= 1 << 32;
    let mmio = |limit: u64| {
        move |&(base, size): &(u64, u64)| {
            Window.mmio(base, (base + size).min(limit).saturating_sub(base))
        }
    };
    let low = routes.mmio.iter().filter(below).map(mmio(FIXED_MMIO));
    let high = routes
        .mmio
        .iter()
        .filter(|route| !below(route))
        .map(mmio(FABRIC_RESERVED));
    // The die with bus 0 has the legacy VGA memory routed to it and owns
    // the ECAM window.
    let primary = |size: u64| if *buses.start() == 0 { size } else { 0 };
    let vga = [Forwarded.mmio(0xa_0000, primary(0x2_0000))];
    let own = [
        Own.mmio(non_pci_base, non_pci_size),
        Own.mmio(ecam_base, primary(ecam_size)),
    ];
    PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base,
            bus_start: *buses.start(),
            bus_end: *buses.end(),
        },
        io.chain(vga).chain(low).chain(high).chain(own),
    )
    .ok()
}
