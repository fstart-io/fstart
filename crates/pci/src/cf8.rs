//! PCI configuration mechanism #1: segment 0 through the x86 ports CF8/CFC.
//!
//! Each access writes a dword selector (enable, bus, device, function and
//! dword offset) to CF8, then reads or writes CFC at the requested width.
//! Byte and word accesses stay byte and word cycles on CFC..CFF: a dword
//! read/modify/write would replay neighbouring registers, some of which are
//! write-1-to-clear. Only the first 256 bytes of each function are reachable;
//! use [`crate::ecam`] once the chipset decodes ECAM.
//!
//! This is the only CF8/CFC implementation; everything that needs legacy
//! config access, including the chipset writes that enable ECAM, uses it.
//!
//! # Serialization
//!
//! An access is two port operations and the selector in CF8 is shared by
//! every CPU. Nothing here locks: the caller guarantees that no other CPU,
//! interrupt handler or SMI handler uses CF8/CFC between the two, for
//! example by running on the BSP with interrupts off before the APs start,
//! or in an SMI handler that owns the rendezvous. Code that hands the ports
//! to something else and resumes, like the VBIOS emulator, saves and
//! restores the selector at [`ADDRESS_PORT`].

use fstart_core::pio::{inb, inl, inw, outb, outl, outw};
use pci_types::{ConfigRegionAccess, PciAddress};

/// The selector port (CONFIG_ADDRESS).
pub const ADDRESS_PORT: u16 = 0x0cf8;
/// The data port (CONFIG_DATA); bytes and words use CFC + `offset & 3`.
pub const DATA_PORT: u16 = 0x0cfc;

/// CF8 selector for the dword holding `offset`.
#[inline]
pub const fn address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | (((device & 0x1f) as u32) << 11)
        | (((function & 7) as u32) << 8)
        | ((offset as u32) & 0xfc)
}

/// Read the dword at `offset` (low two bits ignored).
///
/// # Safety
/// Legacy config access must be decoded, the caller owns the register and
/// serializes CF8/CFC as described in the [module documentation](self).
#[inline]
pub unsafe fn read32(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    // SAFETY: the caller upholds this function's contract.
    unsafe {
        outl(ADDRESS_PORT, address(bus, device, function, offset));
        inl(DATA_PORT)
    }
}

/// Write the dword at `offset` (low two bits ignored).
///
/// # Safety
/// As for [`read32`].
#[inline]
pub unsafe fn write32(bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    // SAFETY: the caller upholds this function's contract.
    unsafe {
        outl(ADDRESS_PORT, address(bus, device, function, offset));
        outl(DATA_PORT, value);
    }
}

/// Read the aligned word at `offset`.
///
/// # Safety
/// As for [`read32`].
#[inline]
pub unsafe fn read16(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    assert!(offset & 1 == 0, "unaligned config word");
    // SAFETY: the caller upholds this function's contract.
    unsafe {
        outl(ADDRESS_PORT, address(bus, device, function, offset));
        inw(DATA_PORT + u16::from(offset & 2))
    }
}

/// Write the aligned word at `offset` with a word cycle.
///
/// # Safety
/// As for [`read32`].
#[inline]
pub unsafe fn write16(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    assert!(offset & 1 == 0, "unaligned config word");
    // SAFETY: the caller upholds this function's contract.
    unsafe {
        outl(ADDRESS_PORT, address(bus, device, function, offset));
        outw(DATA_PORT + u16::from(offset & 2), value);
    }
}

/// Read the byte at `offset`.
///
/// # Safety
/// As for [`read32`].
#[inline]
pub unsafe fn read8(bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    // SAFETY: the caller upholds this function's contract.
    unsafe {
        outl(ADDRESS_PORT, address(bus, device, function, offset));
        inb(DATA_PORT + u16::from(offset & 3))
    }
}

/// Write the byte at `offset` with a byte cycle.
///
/// # Safety
/// As for [`read32`].
#[inline]
pub unsafe fn write8(bus: u8, device: u8, function: u8, offset: u8, value: u8) {
    // SAFETY: the caller upholds this function's contract.
    unsafe {
        outl(ADDRESS_PORT, address(bus, device, function, offset));
        outb(DATA_PORT + u16::from(offset & 3), value);
    }
}

/// Segment 0 through CF8/CFC as a [`ConfigRegionAccess`], for `pci_types`
/// headers and capability walks before ECAM is decoded.
///
/// Dword accesses only, at offsets below 0x100. Callers of the `unsafe`
/// methods uphold the [serialization contract](self).
#[derive(Debug, Clone, Copy, Default)]
pub struct Cf8;

impl ConfigRegionAccess for Cf8 {
    unsafe fn read(&self, address: PciAddress, offset: u16) -> u32 {
        assert!(address.segment() == 0 && offset < 0x100 && offset & 3 == 0);
        // SAFETY: the caller owns the function and serializes CF8/CFC.
        unsafe {
            read32(
                address.bus(),
                address.device(),
                address.function(),
                offset as u8,
            )
        }
    }

    unsafe fn write(&self, address: PciAddress, offset: u16, value: u32) {
        assert!(address.segment() == 0 && offset < 0x100 && offset & 3 == 0);
        // SAFETY: as for `read`.
        unsafe {
            write32(
                address.bus(),
                address.device(),
                address.function(),
                offset as u8,
                value,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selector_keeps_every_bdf_bit_and_selects_the_dword() {
        assert_eq!(address(0, 0, 0, 0), 0x8000_0000);
        assert_eq!(address(255, 31, 7, 0xff), 0x80ff_fffc);
        for bus in [0, 1, 0x55, 0x80, 255] {
            for device in 0..32 {
                for function in 0..8 {
                    for offset in 0..=u8::MAX {
                        let value = address(bus, device, function, offset);
                        assert_eq!(value >> 31, 1);
                        assert_eq!((value >> 16) & 0xff, u32::from(bus));
                        assert_eq!((value >> 11) & 0x1f, u32::from(device));
                        assert_eq!((value >> 8) & 7, u32::from(function));
                        assert_eq!(value & 0xff, u32::from(offset & !3));
                    }
                }
            }
        }
    }

    // The checks must fire before an unprivileged host executes `in`/`out`.
    #[test]
    #[should_panic]
    fn unaligned_word_read_is_rejected_before_port_access() {
        unsafe { read16(0, 0, 0, 1) };
    }

    #[test]
    #[should_panic]
    fn unaligned_word_write_is_rejected_before_port_access() {
        unsafe { write16(0, 0, 0, 0xff, 0) };
    }

    #[test]
    #[should_panic]
    fn extended_config_is_rejected_before_port_access() {
        unsafe { Cf8.read(PciAddress::new(0, 0, 0, 0), 0x100) };
    }
}
