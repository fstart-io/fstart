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
use fstart_pci::{PciEcamError, PciRoot, PciRootInfo, PciWindow};

/// Legacy ISA and chipset I/O (PIC, PIT, KBC, EC, ACPI PM block).
const LEGACY_IO: PciWindow = PciWindow::io(0, 0x1000);
/// Option ROMs and the BIOS shadow, behind the PAM registers.
const OPTION_ROMS: PciWindow = PciWindow::mmio(0xc_0000, 0x4_0000);
/// TPM TIS registers, decoded by LPC.
const TPM: PciWindow = PciWindow::mmio(0xfed4_0000, 0x5000);
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

impl HostBridge {
    /// The root bridge `info` of this host bridge: what it forwards to PCI
    /// and what of that the firmware keeps.
    pub fn root(&self, info: PciRootInfo) -> Result<PciRoot, PciEcamError> {
        let mmio = |(base, end): (u64, u64)| PciWindow::mmio(base, end.saturating_sub(base));
        let (ecam_base, ecam_end) = self.ecam;
        // Below and above the ECAM window.
        let low = self.low_dram_end.into_iter().flat_map(|base| {
            [
                mmio((base, ecam_base.min(MMIO32_END))),
                mmio((base.max(ecam_end), MMIO32_END)),
            ]
        });
        PciRoot::new(
            info,
            [PciWindow::io(0, 0x1_0000), OPTION_ROMS]
                .into_iter()
                .chain(low)
                .chain([TPM])
                .chain(self.above_4g.map(mmio)),
            [LEGACY_IO, OPTION_ROMS, TPM],
        )
    }

    /// `Device(PERC)`, for `\_SB`: the ECAM window as a motherboard resource.
    #[cfg(feature = "acpi")]
    pub fn ecam_reservation(&self) -> alloc::vec::Vec<u8> {
        let (base, end) = self.ecam;
        fstart_acpi::pci_root::ecam_reservation(base, end - base)
            .expect("the ECAM window is not empty")
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

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_pci::PciWindowKind::{Io, Mmio};

    const INFO: PciRootInfo = PciRootInfo {
        segment: 0,
        ecam_base: 0xf000_0000,
        bus_start: 0,
        bus_end: 0x3f,
    };

    fn spans(windows: &[PciWindow]) -> alloc::vec::Vec<(fstart_pci::PciWindowKind, u64, u64)> {
        windows.iter().map(|w| (w.kind, w.base, w.end())).collect()
    }

    #[test]
    fn forwards_what_is_not_dram_and_keeps_the_fixed_parts() {
        let root = HostBridge {
            ecam: (0xf000_0000, 0xf400_0000),
            low_dram_end: Some(0x8000_0000),
            above_4g: Some((0x1_4000_0000, 0x10_0000_0000)),
        }
        .root(INFO)
        .unwrap();
        assert_eq!(
            spans(root.windows()),
            [
                (Io, 0, 0x1_0000),
                (Mmio, 0xc_0000, 0x10_0000),
                // TOLUD to the I/O APIC, without the ECAM window.
                (Mmio, 0x8000_0000, 0xf000_0000),
                (Mmio, 0xf400_0000, 0xfec0_0000),
                (Mmio, 0xfed4_0000, 0xfed4_5000),
                (Mmio, 0x1_4000_0000, 0x10_0000_0000),
            ]
        );
        assert_eq!(
            spans(root.reserved()),
            [
                (Io, 0, 0x1000),
                (Mmio, 0xc_0000, 0x10_0000),
                (Mmio, 0xfed4_0000, 0xfed4_5000),
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
            let root = HostBridge {
                ecam,
                low_dram_end: Some(0xc000_0000),
                above_4g: None,
            }
            .root(INFO)
            .unwrap();
            let windows = root.windows();
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
        let root = HostBridge {
            ecam: (0xe000_0000, 0xf000_0000),
            low_dram_end: Some(0xc000_0000),
            above_4g: Some((1 << 32, 1 << 36)),
        }
        .root(INFO)
        .unwrap();
        let pci = fstart_pci::PciEcam::new([&root]).unwrap();
        assert_eq!(pci.roots().len(), 1);
    }

    #[test]
    fn nothing_below_4g_before_memory_is_known() {
        let root = HostBridge {
            ecam: (0xe000_0000, 0xf000_0000),
            low_dram_end: None,
            above_4g: None,
        }
        .root(INFO)
        .unwrap();
        assert!(
            root.windows()
                .iter()
                .all(|w| w.base < 0x10_0000 || w.base == 0xfed4_0000)
        );
    }
}
