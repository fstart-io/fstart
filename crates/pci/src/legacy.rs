//! PCI configuration mechanism #1 (I/O ports CF8/CFC) as a
//! [`ConfigRegionAccess`], for the chipset writes that enable ECAM.
//!
//! Only the first 256 bytes of each function are reachable. Use
//! [`crate::ecam::GlobalEcam`] once ECAM is decoded.
//!
//! A CF8/CFC access is two port operations with nothing serializing them
//! against other CF8/CFC users. This accessor is meant for the early,
//! single-CPU and interrupt-free code that enables ECAM; callers of its
//! `unsafe` methods guarantee that no other CPU, interrupt handler or
//! direct `fstart_core::pio` CF8/CFC user can run in between.

use crate::{ConfigRegionAccess, PciAddress};

/// Segment 0 configuration space through CF8/CFC.
#[derive(Debug, Clone, Copy, Default)]
pub struct LegacyConfig;

impl ConfigRegionAccess for LegacyConfig {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        debug_assert!(offset < 0x100 && offset & 3 == 0);
        // SAFETY: the caller owns the function and, per the module contract,
        // the CF8/CFC pair for the whole transaction.
        unsafe {
            fstart_core::pio::pci_cfg_read32(
                address.bus(),
                address.device(),
                address.function(),
                offset as u8,
            )
        }
    }

    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        debug_assert!(offset < 0x100 && offset & 3 == 0);
        // SAFETY: as for `read`.
        unsafe {
            fstart_core::pio::pci_cfg_write32(
                address.bus(),
                address.device(),
                address.function(),
                offset as u8,
                value,
            )
        }
    }
}
