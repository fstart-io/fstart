//! The PCI root bridge of an Intel host bridge.
//!
//! These host bridges send to PCI everything that is not DRAM and that they
//! do not decode themselves (coreboot `pci_domain_read_resources`): all of
//! I/O, the option ROM area, from the top of low DRAM to the I/O APIC
//! without the ECAM window, the TPM's registers on LPC and, where the
//! address lines reach, from the top of remapped DRAM upwards.
//!
//! The ECAM window is the host bridge's own, not a window to PCI, so it is
//! not in the root's windows and not in `_CRS`; it is a motherboard
//! resource under `\_SB` (PCI Firmware 3.2, 4.1.2; coreboot's Broadwell
//! `hostbridge.asl` has the same two windows around it). The firmware keeps
//! the legacy I/O, the option ROM area and the TPM for itself, which the
//! bridge forwards all the same; the PCI allocator has the rest.

#[cfg(any(feature = "gm45", feature = "gm965", feature = "i945"))]
use fstart_core::services::memory_detect::{E820Kind, E820State};
use fstart_pci::{PciRootError, PciRootWindows, PciWindow, PciWindowKind};

/// Legacy ISA and chipset I/O (PIC, PIT, KBC, EC, ACPI PM block).
const LEGACY_IO: (u64, u64) = (0, 0x1000);
/// Option ROMs and the BIOS shadow, behind the PAM registers.
const OPTION_ROMS: (u64, u64) = (0xc_0000, 0x10_0000);
/// TPM TIS registers, decoded by LPC.
const TPM: (u64, u64) = (0xfed4_0000, 0xfed4_5000);
/// The I/O APIC: 32-bit PCI MMIO ends below the fixed chipset blocks
/// (coreboot `DOMAIN_RESOURCE_32BIT_LIMIT`).
const MMIO32_END: u64 = 0xfec0_0000;

/// What one host bridge sends to PCI.
pub(crate) struct HostBridge {
    /// The ECAM window (PCIEXBAR), as `base..end`.
    pub ecam: (u64, u64),
    /// Where DRAM, stolen graphics memory and TSEG end below 4 GiB; `None`
    /// before memory is detected.
    pub low_dram_end: Option<u64>,
    /// PCI MMIO above 4 GiB as `base..end`, for a host bridge that decodes
    /// it.
    pub above_4g: Option<(u64, u64)>,
}

fn collect(
    ranges: impl IntoIterator<Item = (PciWindowKind, (u64, u64))>,
) -> Result<PciRootWindows, PciRootError> {
    let mut windows = PciRootWindows::new();
    for (kind, (base, end)) in ranges {
        if base < end {
            windows
                .push(PciWindow {
                    kind,
                    base,
                    size: end - base,
                    prefetchable: base >= 1 << 32,
                })
                .map_err(|_| PciRootError::TooManyWindows)?;
        }
    }
    Ok(windows)
}

impl HostBridge {
    /// What the host bridge decodes: `PciRootProvider::resource_windows`.
    pub fn windows(&self) -> Result<PciRootWindows, PciRootError> {
        use PciWindowKind::{Io, Mmio};
        collect(
            [(Io, (0, 0x1_0000)), (Mmio, OPTION_ROMS)]
                .into_iter()
                .chain(self.low_dram_end.into_iter().flat_map(|base| {
                    // Below and above the ECAM window.
                    let (ecam_base, ecam_end) = self.ecam;
                    [
                        (Mmio, (base, ecam_base.min(MMIO32_END))),
                        (Mmio, (base.max(ecam_end), MMIO32_END)),
                    ]
                }))
                .chain([(Mmio, TPM)])
                .chain(self.above_4g.map(|window| (Mmio, window))),
        )
    }

    /// What the allocator leaves alone: `PciRootProvider::reserved_windows`.
    pub fn reserved(&self) -> Result<PciRootWindows, PciRootError> {
        use PciWindowKind::{Io, Mmio};
        collect([(Io, LEGACY_IO), (Mmio, OPTION_ROMS), (Mmio, TPM)])
    }
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

#[cfg(feature = "acpi")]
impl HostBridge {
    /// `Device(PERC)`, for `\\_SB`: the ECAM window as a motherboard resource.
    pub fn ecam_reservation(&self) -> alloc::vec::Vec<u8> {
        let (base, end) = self.ecam;
        fstart_acpi::pci_root::ecam_reservation(base, end - base)
            .expect("the ECAM window is not empty")
    }
}

/// `Name(_CRS, ...)` of PCI0: what `bridge` forwards to PCI.
#[cfg(feature = "acpi")]
pub(crate) fn crs(bridge: &impl fstart_pci::PciRootProvider) -> alloc::vec::Vec<u8> {
    let root = fstart_pci::PciRoot::of(bridge).expect("the host bridge's windows fit one root");
    fstart_acpi::pci_root::crs(&root, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_what_is_not_dram_and_keeps_the_fixed_parts() {
        let bridge = HostBridge {
            ecam: (0xf000_0000, 0xf400_0000),
            low_dram_end: Some(0x8000_0000),
            above_4g: Some((0x1_4000_0000, 0x10_0000_0000)),
        };
        let spans = |windows: PciRootWindows| -> alloc::vec::Vec<_> {
            windows
                .iter()
                .map(|w| (w.kind, w.base, w.end(), w.prefetchable))
                .collect()
        };
        use PciWindowKind::{Io, Mmio};
        assert_eq!(
            spans(bridge.windows().unwrap()),
            [
                (Io, 0, 0x1_0000, false),
                (Mmio, 0xc_0000, 0x10_0000, false),
                // TOLUD to the I/O APIC, without the ECAM window.
                (Mmio, 0x8000_0000, 0xf000_0000, false),
                (Mmio, 0xf400_0000, 0xfec0_0000, false),
                (Mmio, 0xfed4_0000, 0xfed4_5000, false),
                (Mmio, 0x1_4000_0000, 0x10_0000_0000, true),
            ]
        );
        assert_eq!(
            spans(bridge.reserved().unwrap()),
            [
                (Io, 0, 0x1000, false),
                (Mmio, 0xc_0000, 0x10_0000, false),
                (Mmio, 0xfed4_0000, 0xfed4_5000, false),
            ]
        );
    }

    #[test]
    fn no_window_holds_the_ecam_window_wherever_it_is() {
        // Below the top of low DRAM never happens; above it, in it, at its
        // end and above the I/O APIC must all leave ECAM out.
        for ecam in [
            (0xe000_0000, 0xf000_0000),
            (0xf000_0000, 0xf400_0000),
            (0xc000_0000, 0xd000_0000),
            (0xf800_0000, 0xfc00_0000),
        ] {
            let bridge = HostBridge {
                ecam,
                low_dram_end: Some(0xc000_0000),
                above_4g: None,
            };
            let windows = bridge.windows().unwrap();
            assert!(
                windows
                    .iter()
                    .all(|w| w.end() <= ecam.0 || w.base >= ecam.1),
                "{ecam:x?}: {windows:x?}"
            );
            let low: u64 = windows
                .iter()
                .filter(|w| (0xc000_0000..MMIO32_END).contains(&w.base))
                .map(|w| w.size)
                .sum();
            assert_eq!(low, MMIO32_END - 0xc000_0000 - (ecam.1 - ecam.0));
        }
    }

    #[test]
    fn the_allocator_takes_the_root() {
        let bridge = HostBridge {
            ecam: (0xe000_0000, 0xf000_0000),
            low_dram_end: Some(0xc000_0000),
            above_4g: Some((1 << 32, 1 << 36)),
        };
        let root = fstart_pci::PciRoot::new(
            fstart_pci::PciRootInfo {
                segment: 0,
                ecam_base: 0xe000_0000,
                bus_start: 0,
                bus_end: 0xff,
            },
            bridge.windows().unwrap(),
        )
        .unwrap()
        .reserve(bridge.reserved().unwrap())
        .unwrap();
        assert!(fstart_pci::PciEcam::from_provider(&root).is_ok());
    }

    #[test]
    fn nothing_below_4g_before_memory_is_known() {
        let bridge = HostBridge {
            ecam: (0xe000_0000, 0xf000_0000),
            low_dram_end: None,
            above_4g: None,
        };
        assert!(
            bridge
                .windows()
                .unwrap()
                .iter()
                .all(|w| w.base < 0x10_0000 || w.base == 0xfed4_0000)
        );
    }
}
