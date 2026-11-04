//! The PCI root bridge of an Intel host bridge, as resources.
//!
//! These host bridges send to PCI everything that is not DRAM and that the
//! chipset does not decode itself (coreboot `pci_domain_read_resources`).
//! [`forwarded`] is the part every one of them has; a northbridge adds what
//! its chipset owns (the ECAM window, MCHBAR and the other chipset BARs),
//! and the three readers of a [`fstart_pci::PciRoot`] do the rest: the
//! allocator, PCI0's `_CRS` and the motherboard device `PDRC`.

#[cfg(any(feature = "gm45", feature = "gm965", feature = "i945"))]
use fstart_core::services::memory_detect::{E820Kind, E820State};
use fstart_pci::PciResource;
use fstart_pci::PciRole::{Forwarded, Kept, Window};

/// The I/O APIC: 32-bit PCI MMIO ends below the fixed chipset blocks
/// (coreboot `DOMAIN_RESOURCE_32BIT_LIMIT`).
#[cfg(any(
    feature = "gm45",
    feature = "gm965",
    feature = "i945",
    feature = "pineview",
    feature = "ironlake"
))]
pub(crate) const MMIO32_END: u64 = 0xfec0_0000;

/// What every host bridge forwards.
///
/// `low_dram_end` is where DRAM, stolen graphics memory and TSEG end below
/// 4 GiB, `None` before memory is detected; `above_4g` is PCI MMIO above
/// 4 GiB as `base..end`, for a host bridge whose address lines reach it.
#[cfg(any(
    feature = "gm45",
    feature = "gm965",
    feature = "i945",
    feature = "pineview",
    feature = "ironlake"
))]
pub(crate) fn forwarded(
    low_dram_end: Option<u64>,
    above_4g: Option<(u64, u64)>,
) -> [PciResource; 7] {
    forwarded_below(low_dram_end, above_4g, MMIO32_END)
}

/// [`forwarded`] for a host bridge whose 32-bit MMIO ends before the I/O
/// APIC: a bridge that does not forward an address must not describe it.
pub(crate) fn forwarded_below(
    low_dram_end: Option<u64>,
    above_4g: Option<(u64, u64)>,
    mmio32_end: u64,
) -> [PciResource; 7] {
    let low = low_dram_end.unwrap_or(mmio32_end);
    let (high, high_end) = above_4g.unwrap_or((0, 0));
    [
        // All of I/O: the legacy ISA and chipset ports (PIC, PIT, KBC, EC,
        // the ACPI PM block), and PCI devices above them.
        Forwarded.io(0, 0x1_0000),
        Window.io(0x1000, 0xf000),
        // The legacy VGA memory, then the option ROMs and the BIOS shadow
        // behind the PAM registers.
        Forwarded.mmio(0xa_0000, 0x2_0000),
        Forwarded.mmio(0xc_0000, 0x4_0000),
        Window.mmio(low, mmio32_end.saturating_sub(low)),
        // The TPM's TIS registers, decoded by LPC whether a TPM is there
        // or not.
        Kept.mmio(0xfed4_0000, 0x5000),
        Window.mmio(high, high_end.saturating_sub(high)),
    ]
}

/// Where PCI MMIO can start below 4 GiB: above low DRAM and the reserved
/// memory on top of it (stolen graphics memory, TSEG), rounded to 1 MiB.
#[cfg(any(feature = "gm45", feature = "gm965", feature = "i945"))]
pub(crate) fn low_dram_end(memory: &E820State) -> Option<u64> {
    const FOUR_GIB: u64 = 1 << 32;
    if memory.count() == 0 {
        return None;
    }
    let below_4g = || memory.entries().iter().filter(|e| e.addr < FOUR_GIB);
    let end = |e: &fstart_core::services::memory_detect::E820Entry| {
        e.addr.saturating_add(e.size).min(FOUR_GIB)
    };
    let ram = E820Kind::Ram as u32;
    let ram_end = below_4g()
        .filter(|e| e.kind == ram)
        .map(end)
        .max()
        .unwrap_or(0)
        .max(0x10_0000);
    let reserved_end = below_4g()
        .filter(|e| e.kind != ram && e.addr >= ram_end)
        .map(end)
        .max()
        .unwrap_or(0);
    Some((reserved_end.max(ram_end) + 0xf_ffff) & !0xf_ffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_pci::PciRole::Own;
    use fstart_pci::PciWindowKind::{Io, Mmio};
    use fstart_pci::{PciRoot, PciRootInfo, PciWindow};

    fn root(ecam: (u64, u64), low: Option<u64>, high: Option<(u64, u64)>) -> PciRoot {
        let info = PciRootInfo {
            segment: 0,
            ecam_base: ecam.0,
            bus_start: 0,
            bus_end: 0x3f,
        };
        let chipset = [
            Own.mmio(ecam.0, ecam.1 - ecam.0),
            Own.mmio(0xfed1_4000, 0x4000),
        ];
        PciRoot::new(info, forwarded(low, high).into_iter().chain(chipset)).unwrap()
    }

    fn spans(ranges: &[PciWindow]) -> alloc::vec::Vec<(fstart_pci::PciWindowKind, u64, u64)> {
        ranges.iter().map(|r| (r.kind, r.base, r.end())).collect()
    }

    #[test]
    fn forwards_what_is_not_dram_without_what_the_chipset_owns() {
        let root = root(
            (0xf000_0000, 0xf400_0000),
            Some(0x8000_0000),
            Some((0x1_4000_0000, 0x10_0000_0000)),
        );
        // `_CRS`.
        assert_eq!(
            spans(root.forwarded()),
            [
                (Io, 0, 0x1_0000),
                // Legacy VGA, option ROMs and BIOS shadow: one range.
                (Mmio, 0xa_0000, 0x10_0000),
                // TOLUD to the I/O APIC, without the ECAM window.
                (Mmio, 0x8000_0000, 0xf000_0000),
                (Mmio, 0xf400_0000, 0xfec0_0000),
                (Mmio, 0xfed4_0000, 0xfed4_5000),
                (Mmio, 0x1_4000_0000, 0x10_0000_0000),
            ]
        );
        // The allocator: the same without the legacy ports, the option
        // ROMs and the TPM.
        assert_eq!(
            spans(root.allocatable()),
            [
                (Io, 0x1000, 0x1_0000),
                (Mmio, 0x8000_0000, 0xf000_0000),
                (Mmio, 0xf400_0000, 0xfec0_0000),
                (Mmio, 0x1_4000_0000, 0x10_0000_0000),
            ]
        );
        // `PDRC`.
        assert_eq!(
            spans(root.motherboard()),
            [
                (Mmio, 0xfed4_0000, 0xfed4_5000),
                (Mmio, 0xf000_0000, 0xf400_0000),
                (Mmio, 0xfed1_4000, 0xfed1_8000),
            ]
        );
    }

    #[test]
    fn no_window_holds_the_ecam_window_wherever_it_is() {
        // Above the top of low DRAM, at it, and up against the I/O APIC.
        for ecam in [
            (0xe000_0000, 0xf000_0000),
            (0xf000_0000, 0xf400_0000),
            (0xc000_0000, 0xd000_0000),
            (0xf800_0000, 0xfc00_0000),
        ] {
            let root = root(ecam, Some(0xc000_0000), None);
            let low = |ranges: &[PciWindow]| -> u64 {
                ranges
                    .iter()
                    .inspect(|r| assert!(r.end() <= ecam.0 || r.base >= ecam.1, "{ecam:x?}"))
                    .filter(|r| (0xc000_0000..MMIO32_END).contains(&r.base))
                    .map(|r| r.size)
                    .sum()
            };
            let rest = MMIO32_END - 0xc000_0000 - (ecam.1 - ecam.0);
            assert_eq!(low(root.forwarded()), rest);
            assert_eq!(low(root.allocatable()), rest);
        }
    }

    #[test]
    fn nothing_for_pci_below_4g_before_memory_is_known() {
        let root = root((0xe000_0000, 0xf000_0000), None, None);
        assert_eq!(spans(root.allocatable()), [(Io, 0x1000, 0x1_0000)]);
    }
}
