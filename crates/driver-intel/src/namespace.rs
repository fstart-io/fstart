//! Paths of the chipset devices, from the names their drivers give them
//! (coreboot's `acpi_device_path()`): the PCI root under `\_SB`, the IGD
//! and the LPC bridge on that root.

use crate::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
use alloc::vec::Vec;
use fstart_acpi::AcpiPath;

/// The northbridge's PCI root, `\_SB_.PCI0`.
#[must_use]
pub const fn pci_root<NB: IntelNorthbridgeDriver>() -> AcpiPath {
    AcpiPath::SB.child(NB::ACPI_PCI_ROOT_NAME)
}

/// What the tables say about the northbridge's PCI root, all of it from
/// `root`: `_CRS` in the root's scope with what it forwards, and next to it
/// the motherboard device `PDRC` with what the chipset decodes itself. The
/// platform passes the root bridge its allocator was built from, so the OS
/// is told about the windows the firmware allocated in.
pub fn pci_root_resources<NB: IntelNorthbridgeDriver>(
    root: &fstart_pci::PciRoot,
) -> Result<Vec<u8>, fstart_acpi::AmlError> {
    let mut aml = fstart_acpi::aml_linker::scope_vec(
        const { pci_root::<NB>() }.as_str(),
        &fstart_acpi::pci_root::crs(root),
    )?;
    aml.extend(fstart_acpi::pci_root::motherboard("PDRC", root)?);
    Ok(aml)
}

/// The integrated graphics device, `\_SB_.PCI0.GFX0`.
#[must_use]
pub const fn igd<NB: IntelNorthbridgeDriver>() -> AcpiPath {
    pci_root::<NB>().child(NB::ACPI_IGD_NAME)
}

/// The southbridge's LPC bridge on the northbridge's root, `\_SB_.PCI0.LPCB`.
#[must_use]
pub const fn lpc<NB: IntelNorthbridgeDriver, SB: IntelSouthbridgeDriver>() -> AcpiPath {
    pci_root::<NB>().child(SB::ACPI_LPC_NAME)
}

/// A southbridge's ACPI tables. Its PCI devices sit on the northbridge's
/// root, which the platform passes in: coreboot's `dsdt.asl` includes the
/// southbridge ASL inside the northbridge's `Device (PCI0)`.
pub trait SouthbridgeAcpi: IntelSouthbridgeDriver {
    /// DSDT content: the southbridge's devices inside `pci_root` and its
    /// root-scope objects.
    fn dsdt_aml(&self, config: &Self::Config, pci_root: &AcpiPath) -> Vec<u8>;

    /// Standalone tables, if the southbridge has any.
    fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
        Vec::new()
    }
}
