// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `_ROM` of a display function below a root bridge of the DSDT.

extern crate alloc;

use alloc::{format, vec::Vec};

use fstart_acpi::AmlError;
use fstart_acpi::aml_linker::{device_vec, scope_vec};
use fstart_acpi_macros::acpi_dsl;

use super::dsdt::bridge_name;

/// Attach a ROM to the actual display function under the root bridge
/// `PCI{root}`. The bridges of a root bus already have their `RPdf` nodes
/// (and `_PRT`); do not duplicate them. Further bridge/function nodes use
/// their real slot/function `_ADR`, never a hard-coded GPU bus number. The
/// ROM method itself is platform-independent.
pub fn video_rom_aml(
    root: u8,
    path: &[fstart_pci::PciAddress],
    rom: &[u8],
) -> Result<Vec<u8>, AmlError> {
    if path.is_empty() {
        return Err(AmlError::InvalidPath);
    }
    let mut body = fstart_acpi::video::rom_aml(rom)?;
    for (index, address) in path.iter().enumerate().rev() {
        if index == 0 && path.len() > 1 {
            let bridge = bridge_name(address.device(), address.function());
            let bridge = core::str::from_utf8(&bridge).map_err(|_| AmlError::InvalidPath)?;
            return scope_vec(&format!(r"\_SB.PCI{root}.{bridge}"), &body);
        }
        let devfn = fstart_pci::devfn(address.device(), address.function());
        let adr = u32::from(address.device()) << 16 | u32::from(address.function());
        let mut content = Vec::new();
        acpi_dsl! { Name("_ADR", #{dword adr}); }.emit(&mut content)?;
        content.extend(body);
        body = device_vec(&format!("D{devfn:03X}"), &content)?;
    }
    scope_vec(&format!(r"\_SB.PCI{root}"), &body)
}
