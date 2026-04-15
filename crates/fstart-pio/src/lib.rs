//! x86 port I/O primitives.
//!
//! Provides inline-asm wrappers for the x86 `in` and `out` instructions,
//! which access the separate 64 KiB I/O port address space (ports 0x0000
//! through 0xFFFF).
//!
//! Legacy x86 devices (UART at 0x3F8, PCI config at 0xCF8/0xCFC, PIT at
//! 0x40-0x43, fw_cfg at 0x510/0x511) use port I/O rather than
//! memory-mapped I/O.
//!
//! # Safety
//!
//! All functions are `unsafe` because accessing arbitrary I/O ports can
//! trigger side effects in hardware. Callers must ensure the port address
//! corresponds to an actual device register.

#![no_std]

/// Read a byte from an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    // SAFETY: caller guarantees port validity.
    unsafe {
        core::arch::asm!(
            "in al, dx",
            out("al") val,
            in("dx") port,
            options(nostack, nomem, preserves_flags),
        );
    }
    val
}

/// Write a byte to an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn outb(port: u16, val: u8) {
    // SAFETY: caller guarantees port validity.
    unsafe {
        core::arch::asm!(
            "out dx, al",
            in("dx") port,
            in("al") val,
            options(nostack, nomem, preserves_flags),
        );
    }
}

/// Read a 16-bit word from an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn inw(port: u16) -> u16 {
    let val: u16;
    // SAFETY: caller guarantees port validity.
    unsafe {
        core::arch::asm!(
            "in ax, dx",
            out("ax") val,
            in("dx") port,
            options(nostack, nomem, preserves_flags),
        );
    }
    val
}

/// Write a 16-bit word to an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn outw(port: u16, val: u16) {
    // SAFETY: caller guarantees port validity.
    unsafe {
        core::arch::asm!(
            "out dx, ax",
            in("dx") port,
            in("ax") val,
            options(nostack, nomem, preserves_flags),
        );
    }
}

/// Read a 32-bit doubleword from an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn inl(port: u16) -> u32 {
    let val: u32;
    // SAFETY: caller guarantees port validity.
    unsafe {
        core::arch::asm!(
            "in eax, dx",
            out("eax") val,
            in("dx") port,
            options(nostack, nomem, preserves_flags),
        );
    }
    val
}

/// Write a 32-bit doubleword to an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn outl(port: u16, val: u32) {
    // SAFETY: caller guarantees port validity.
    unsafe {
        core::arch::asm!(
            "out dx, eax",
            in("dx") port,
            in("eax") val,
            options(nostack, nomem, preserves_flags),
        );
    }
}

// -----------------------------------------------------------------------
// Legacy PCI configuration access via CF8/CFC I/O ports
// -----------------------------------------------------------------------

/// CF8 address port for legacy PCI config access.
const PCI_CF8: u16 = 0xCF8;
/// CFC data port for legacy PCI config access.
const PCI_CFC: u16 = 0xCFC;

/// Build a CF8 address for legacy PCI config access.
///
/// Format: `(1 << 31) | (bus << 16) | (dev << 11) | (func << 8) | (reg & 0xFC)`
#[inline]
const fn cf8_addr(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | ((reg as u32) & 0xFC)
}

/// Read a 32-bit PCI config register via legacy CF8/CFC I/O ports.
///
/// # Safety
/// Must only be called on x86 systems with legacy PCI config access.
#[inline]
pub unsafe fn pci_cfg_read32(bus: u8, dev: u8, func: u8, reg: u8) -> u32 {
    unsafe {
        outl(PCI_CF8, cf8_addr(bus, dev, func, reg));
        inl(PCI_CFC)
    }
}

/// Write a 32-bit PCI config register via legacy CF8/CFC I/O ports.
///
/// # Safety
/// Must only be called on x86 systems with legacy PCI config access.
#[inline]
pub unsafe fn pci_cfg_write32(bus: u8, dev: u8, func: u8, reg: u8, val: u32) {
    unsafe {
        outl(PCI_CF8, cf8_addr(bus, dev, func, reg));
        outl(PCI_CFC, val);
    }
}

/// Tiny I/O delay via a dummy write to port 0x80 (POST code port).
///
/// This is the standard Linux/coreboot technique for I/O delay on x86.
/// Port 0x80 is the BIOS POST code display port; writing to it has no
/// harmful side effects but introduces enough bus delay to satisfy
/// legacy device timing requirements.
///
/// # Safety
/// Only valid on x86 systems where port 0x80 is the POST code port.
#[inline(always)]
pub unsafe fn io_delay() {
    // SAFETY: port 0x80 write is a standard x86 I/O delay mechanism.
    unsafe { outb(0x80, 0) };
}
