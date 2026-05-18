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

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub use x86::io;

/// Read a byte from an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn inb(port: u16) -> u8 {
    unsafe { x86::io::inb(port) }
}

/// Write a byte to an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn outb(port: u16, val: u8) {
    unsafe { x86::io::outb(port, val) }
}

/// Read a 16-bit word from an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn inw(port: u16) -> u16 {
    unsafe { x86::io::inw(port) }
}

/// Write a 16-bit word to an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn outw(port: u16, val: u16) {
    unsafe { x86::io::outw(port, val) }
}

/// Read a 32-bit doubleword from an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn inl(port: u16) -> u32 {
    unsafe { x86::io::inl(port) }
}

/// Write a 32-bit doubleword to an I/O port.
///
/// # Safety
/// `port` must be a valid I/O port address for the target device.
#[inline(always)]
pub unsafe fn outl(port: u16, val: u32) {
    unsafe { x86::io::outl(port, val) }
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

// -----------------------------------------------------------------------
// tock-registers-compatible port-I/O register types
// -----------------------------------------------------------------------

use core::marker::PhantomData;

use tock_registers::interfaces::{Readable, Writeable};
use tock_registers::{RegisterLongName, UIntLike};

/// Integer widths supported by x86 port I/O registers.
pub trait PioValue: UIntLike {
    /// Read a value from `port`.
    ///
    /// # Safety
    /// `port` must be valid for this register width.
    unsafe fn port_read(port: u16) -> Self;

    /// Write a value to `port`.
    ///
    /// # Safety
    /// `port` must be valid for this register width.
    unsafe fn port_write(port: u16, value: Self);
}

impl PioValue for u8 {
    #[inline(always)]
    unsafe fn port_read(port: u16) -> Self {
        // SAFETY: caller upholds port validity.
        unsafe { inb(port) }
    }

    #[inline(always)]
    unsafe fn port_write(port: u16, value: Self) {
        // SAFETY: caller upholds port validity.
        unsafe { outb(port, value) };
    }
}

impl PioValue for u16 {
    #[inline(always)]
    unsafe fn port_read(port: u16) -> Self {
        // SAFETY: caller upholds port validity.
        unsafe { inw(port) }
    }

    #[inline(always)]
    unsafe fn port_write(port: u16, value: Self) {
        // SAFETY: caller upholds port validity.
        unsafe { outw(port, value) };
    }
}

impl PioValue for u32 {
    #[inline(always)]
    unsafe fn port_read(port: u16) -> Self {
        // SAFETY: caller upholds port validity.
        unsafe { inl(port) }
    }

    #[inline(always)]
    unsafe fn port_write(port: u16, value: Self) {
        // SAFETY: caller upholds port validity.
        unsafe { outl(port, value) };
    }
}

/// A typed x86 port-I/O register compatible with `tock-registers`.
///
/// `PioRegister<u8, STATUS::Register>` implements [`Readable`] and
/// [`Writeable`], so tock's generated field APIs work (`read`, `write`,
/// `modify`, `matches_all`, etc.).
#[derive(Clone, Copy)]
pub struct PioRegister<T: PioValue, R: RegisterLongName = ()> {
    port: u16,
    _reg: PhantomData<(T, R)>,
}

impl<T: PioValue, R: RegisterLongName> PioRegister<T, R> {
    /// Create a typed register at an absolute I/O port.
    pub const fn new(port: u16) -> Self {
        Self {
            port,
            _reg: PhantomData,
        }
    }

    /// Return the absolute I/O port number.
    pub const fn port(&self) -> u16 {
        self.port
    }
}

impl<T: PioValue, R: RegisterLongName> Readable for PioRegister<T, R> {
    type T = T;
    type R = R;

    #[inline(always)]
    fn get(&self) -> T {
        // SAFETY: `PioRegister` is only constructed by drivers for valid ports.
        unsafe { T::port_read(self.port) }
    }
}

impl<T: PioValue, R: RegisterLongName> Writeable for PioRegister<T, R> {
    type T = T;
    type R = R;

    #[inline(always)]
    fn set(&self, value: T) {
        // SAFETY: `PioRegister` is only constructed by drivers for valid ports.
        unsafe { T::port_write(self.port, value) };
    }
}

/// Declare a small port-I/O register block with tock-compatible accessors.
///
/// Example:
///
/// ```ignore
/// use fstart_pio::pio_register_structs;
/// use fstart_pio::PioRegister;
/// use tock_registers::register_bitfields;
///
/// register_bitfields![u8, STATUS [ INTR OFFSET(1) NUMBITS(1) [] ]];
///
/// pio_register_structs! {
///     pub I801Regs {
///         (0x00 => pub status: PioRegister<u8, STATUS::Register>),
///         (0x02 => pub control: PioRegister<u8>),
///     }
/// }
///
/// let regs = I801Regs::new(0x400);
/// let intr = regs.status().is_set(STATUS::INTR);
/// ```
#[macro_export]
macro_rules! pio_register_structs {
    (
        $(#[$meta:meta])*
        $vis:vis $name:ident {
            $(($offset:expr => $field_vis:vis $field:ident : $reg_ty:ty)),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy)]
        $vis struct $name {
            base: u16,
        }

        impl $name {
            /// Create a port-I/O register block at `base`.
            pub const fn new(base: u16) -> Self {
                Self { base }
            }

            /// Return the block base I/O port.
            pub const fn base(&self) -> u16 {
                self.base
            }

            $(
                #[inline(always)]
                $field_vis fn $field(&self) -> $reg_ty {
                    <$reg_ty>::new(self.base + $offset)
                }
            )+
        }
    };
}
