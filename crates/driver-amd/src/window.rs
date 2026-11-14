// SPDX-License-Identifier: MIT OR Apache-2.0
//! Byte-addressed register spaces ("windows") and tock-registers on them:
//! host MMIO, x86 ports, the SMN of a die or socket, or a PCI function's
//! config space. Code that programs a block takes the window it lives in,
//! so the same code runs on any FCH of any socket or die, and on a host
//! model.
//!
//! A register is defined once as an [`At`]: its offset in the window, its
//! width and its bitfields. [`Op`] tables replay recovered fixed register
//! programs.

use core::marker::PhantomData;

use fstart_core::indexed::Indexed;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::RegisterLongName;
use tock_registers::UIntLike;
use tock_registers::interfaces::{Readable, Writeable};

use crate::smn::{Smn, SmnAddr};

/// A byte-addressed register space.
///
/// Only the 32-bit accessors are required; the narrow ones default to a
/// read-modify-write of the containing dword, which is right for spaces
/// without byte enables (SMN, config space through dword accesses). Spaces
/// with real narrow accesses (MMIO, ports) override them: the FCH IOMUX,
/// BIOS RAM and CMOS blocks only take byte accesses (PPR 55570 §9.1.10).
pub trait Window {
    fn read32(&self, off: u32) -> u32;
    fn write32(&self, off: u32, value: u32);

    fn read16(&self, off: u32) -> u16 {
        (self.read32(off & !3) >> ((off & 2) * 8)) as u16
    }
    fn write16(&self, off: u32, value: u16) {
        let shift = (off & 2) * 8;
        let old = self.read32(off & !3) & !(0xffff << shift);
        self.write32(off & !3, old | u32::from(value) << shift);
    }
    fn read8(&self, off: u32) -> u8 {
        (self.read32(off & !3) >> ((off & 3) * 8)) as u8
    }
    fn write8(&self, off: u32, value: u8) {
        let shift = (off & 3) * 8;
        let old = self.read32(off & !3) & !(0xff << shift);
        self.write32(off & !3, old | u32::from(value) << shift);
    }

    /// The register of width `T` at `off` with the bitfields `R`.
    fn reg<T: Width, R: RegisterLongName>(&self, off: u32) -> Reg<&Self, T, R>
    where
        Self: Sized,
    {
        Reg::new(self, off)
    }

    /// The register `at` defines.
    fn at<T: Width, R: RegisterLongName>(&self, at: At<T, R>) -> Reg<&Self, T, R>
    where
        Self: Sized,
    {
        Reg::new(self, at.offset)
    }
}

/// A register of width `T` with the bitfields `R` at `offset` in its
/// window, for [`Window::at`].
pub struct At<T, R = ()> {
    pub offset: u32,
    _type: PhantomData<fn() -> (T, R)>,
}
impl<T, R> Clone for At<T, R> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T, R> Copy for At<T, R> {}
impl<T, R> core::fmt::Debug for At<T, R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "At({:#x})", self.offset)
    }
}
impl<T, R> PartialEq for At<T, R> {
    fn eq(&self, other: &Self) -> bool {
        self.offset == other.offset
    }
}
impl<T, R> Eq for At<T, R> {}
impl<T: Width, R: RegisterLongName> At<T, R> {
    pub const fn new(offset: u32) -> Self {
        Self {
            offset,
            _type: PhantomData,
        }
    }
}

impl<W: Window + ?Sized> Window for &W {
    fn read32(&self, off: u32) -> u32 {
        (**self).read32(off)
    }
    fn write32(&self, off: u32, value: u32) {
        (**self).write32(off, value)
    }
    fn read16(&self, off: u32) -> u16 {
        (**self).read16(off)
    }
    fn write16(&self, off: u32, value: u16) {
        (**self).write16(off, value)
    }
    fn read8(&self, off: u32) -> u8 {
        (**self).read8(off)
    }
    fn write8(&self, off: u32, value: u8) {
        (**self).write8(off, value)
    }
}

/// Register widths a [`Window`] supports.
pub trait Width: UIntLike {
    const MASK: u32;
    fn read<W: Window + ?Sized>(window: &W, off: u32) -> Self;
    fn write<W: Window + ?Sized>(window: &W, off: u32, value: Self);
    fn to_u32(self) -> u32;
    fn from_u32(value: u32) -> Self;
}

macro_rules! width {
    ($t:ty, $read:ident, $write:ident) => {
        impl Width for $t {
            const MASK: u32 = <$t>::MAX as u32;
            fn read<W: Window + ?Sized>(window: &W, off: u32) -> Self {
                window.$read(off)
            }
            fn write<W: Window + ?Sized>(window: &W, off: u32, value: Self) {
                window.$write(off, value)
            }
            fn to_u32(self) -> u32 {
                self.into()
            }
            fn from_u32(value: u32) -> Self {
                value as $t
            }
        }
    };
}
width!(u8, read8, write8);
width!(u16, read16, write16);
width!(u32, read32, write32);

/// The register of width `T` at `off` in `window` (a window or a
/// reference to one), with the bitfields `R`; implements the
/// tock-registers interfaces.
pub struct Reg<W, T, R: RegisterLongName = ()> {
    window: W,
    off: u32,
    _reg: PhantomData<fn() -> (T, R)>,
}

impl<W: Clone, T, R: RegisterLongName> Clone for Reg<W, T, R> {
    fn clone(&self) -> Self {
        Self::new(self.window.clone(), self.off)
    }
}

impl<W: Copy, T, R: RegisterLongName> Copy for Reg<W, T, R> {}

impl<W, T, R: RegisterLongName> Reg<W, T, R> {
    pub const fn new(window: W, off: u32) -> Self {
        Self {
            window,
            off,
            _reg: PhantomData,
        }
    }
}

impl<W: Window, T: Width, R: RegisterLongName> Readable for Reg<W, T, R> {
    type T = T;
    type R = R;
    #[inline(always)]
    fn get(&self) -> T {
        T::read(&self.window, self.off)
    }
}

impl<W: Window, T: Width, R: RegisterLongName> Writeable for Reg<W, T, R> {
    type T = T;
    type R = R;
    #[inline(always)]
    fn set(&self, value: T) {
        T::write(&self.window, self.off, value)
    }
}

/// The part of `window` from `base` on: a block inside a larger window
/// (an FCH's relocated ACPI-MMIO block in physical MMIO).
#[derive(Clone, Copy)]
pub struct Offset<W> {
    window: W,
    base: u32,
}
impl<W: Window> Offset<W> {
    pub const fn new(window: W, base: u32) -> Self {
        Self { window, base }
    }
}
impl<W: Window> Window for Offset<W> {
    fn read32(&self, off: u32) -> u32 {
        self.window.read32(self.base + off)
    }
    fn write32(&self, off: u32, value: u32) {
        self.window.write32(self.base + off, value)
    }
    fn read16(&self, off: u32) -> u16 {
        self.window.read16(self.base + off)
    }
    fn write16(&self, off: u32, value: u16) {
        self.window.write16(self.base + off, value)
    }
    fn read8(&self, off: u32) -> u8 {
        self.window.read8(self.base + off)
    }
    fn write8(&self, off: u32, value: u8) {
        self.window.write8(self.base + off, value)
    }
}

/// One step of a recovered register program: `x = (x & !clear) | set` at
/// `off` with `width` bytes; `clear` covering the whole width is a plain
/// store without a read (FCH_SP3 §0 notation `&= ~c |= s` / `= v`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Op {
    pub off: u32,
    pub width: u8,
    pub clear: u32,
    pub set: u32,
}

impl Op {
    pub const fn rmw(off: u32, width: u8, clear: u32, set: u32) -> Self {
        Self {
            off,
            width,
            clear,
            set,
        }
    }

    pub const fn store(off: u32, width: u8, value: u32) -> Self {
        Self::rmw(off, width, Self::mask(width), value)
    }

    const fn mask(width: u8) -> u32 {
        match width {
            1 => 0xff,
            2 => 0xffff,
            _ => 0xffff_ffff,
        }
    }

    /// The same step at `off + base`.
    pub const fn at(self, base: u32) -> Self {
        Self::rmw(self.off + base, self.width, self.clear, self.set)
    }

    fn apply_typed<T: Width, W: Window + ?Sized>(&self, window: &W) {
        let value = if self.clear & T::MASK == T::MASK {
            self.set
        } else {
            (T::read(window, self.off).to_u32() & !self.clear) | self.set
        };
        T::write(window, self.off, T::from_u32(value));
    }

    pub fn apply<W: Window + ?Sized>(&self, window: &W) {
        match self.width {
            1 => self.apply_typed::<u8, W>(window),
            2 => self.apply_typed::<u16, W>(window),
            _ => self.apply_typed::<u32, W>(window),
        }
    }
}

/// Register `at` of `R` behind the `index` and `data` registers of
/// `window` (`T` wide each): the PM, RTC, PCI_INTR and A-Link index/data
/// pairs.
pub fn indexed<W: Window + Copy, T: Width, R: RegisterLongName>(
    window: W,
    index: u32,
    data: u32,
    at: T,
) -> Indexed<Reg<W, T>, Reg<W, T>, R> {
    Indexed::new(Reg::new(window, index), Reg::new(window, data), at)
}

/// Run `ops` in order.
pub fn apply<W: Window + ?Sized>(window: &W, ops: &[Op]) {
    ops.iter().for_each(|op| op.apply(window));
}

/// Read-modify-write one register: `x = (x & !clear) | set`.
pub fn update<T: Width, W: Window + ?Sized>(window: &W, off: u32, clear: T, set: T) {
    Op::rmw(off, size_of::<T>() as u8, clear.to_u32(), set.to_u32()).apply(window)
}

/// Physical memory seen through the host (x86 MMIO).
#[derive(Clone, Copy)]
pub struct Mmio {
    base: usize,
}

impl Mmio {
    /// The window at physical `base`.
    ///
    /// # Safety
    /// `base` and every offset used through the window are device MMIO
    /// mapped uncached and owned by the caller.
    pub const unsafe fn new(base: usize) -> Self {
        Self { base }
    }

    fn ptr<T>(&self, off: u32) -> *mut T {
        (self.base + off as usize) as *mut T
    }
}

impl Window for Mmio {
    fn read32(&self, off: u32) -> u32 {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::mmio::read32(self.ptr(off)) }
    }
    fn write32(&self, off: u32, value: u32) {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::mmio::write32(self.ptr(off), value) }
    }
    fn read16(&self, off: u32) -> u16 {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::mmio::read16(self.ptr(off)) }
    }
    fn write16(&self, off: u32, value: u16) {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::mmio::write16(self.ptr(off), value) }
    }
    fn read8(&self, off: u32) -> u8 {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::mmio::read8(self.ptr(off)) }
    }
    fn write8(&self, off: u32, value: u8) {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::mmio::write8(self.ptr(off), value) }
    }
}

/// x86 I/O ports as a window: the offset is the port number.
#[derive(Clone, Copy)]
pub struct Ports(());

impl Ports {
    /// # Safety
    /// The caller owns the I/O ports it accesses through this window.
    pub const unsafe fn new() -> Self {
        Self(())
    }
}

impl Window for Ports {
    fn read32(&self, off: u32) -> u32 {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::pio::inl(off as u16) }
    }
    fn write32(&self, off: u32, value: u32) {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::pio::outl(off as u16, value) }
    }
    fn read16(&self, off: u32) -> u16 {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::pio::inw(off as u16) }
    }
    fn write16(&self, off: u32, value: u16) {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::pio::outw(off as u16, value) }
    }
    fn read8(&self, off: u32) -> u8 {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::pio::inb(off as u16) }
    }
    fn write8(&self, off: u32, value: u8) {
        // SAFETY: `new`'s contract.
        unsafe { fstart_core::pio::outb(off as u16, value) }
    }
}

/// A window at `base` in the SMN of one socket or die.
///
/// SMN data accesses are dwords; narrow writes are dword read-modify-writes,
/// so a narrow write next to write-1-to-clear bytes clears them. The FCH
/// byte-only blocks (IOMUX) may not take dword writes (PPR 55570 §9.1.10):
/// see `fch::gpio` for how IOMUX is reached on remote FCHs.
#[derive(Clone, Copy)]
pub struct SmnWindow<P> {
    smn: Smn<P>,
    base: SmnAddr,
}

impl<P: ConfigRegionAccess> SmnWindow<P> {
    /// # Safety
    /// The caller owns the SMN index/data pair of `smn` and the registers at
    /// `base` + offset (as [`Smn::write32`]).
    pub const unsafe fn new(smn: Smn<P>, base: SmnAddr) -> Self {
        Self { smn, base }
    }
}

impl<P: ConfigRegionAccess> Window for SmnWindow<P> {
    fn read32(&self, off: u32) -> u32 {
        self.smn.read32(self.base + off)
    }
    fn write32(&self, off: u32, value: u32) {
        self.smn.write32(self.base + off, value)
    }
}

/// One function's config space through ECAM, with exact-width accesses
/// (byte and word registers next to status bits take no read-modify-write).
impl Window for fstart_pci::EcamDevice {
    fn read32(&self, off: u32) -> u32 {
        fstart_pci::EcamDevice::read32(self, off as u16)
    }
    fn write32(&self, off: u32, value: u32) {
        fstart_pci::EcamDevice::write32(self, off as u16, value)
    }
    fn read16(&self, off: u32) -> u16 {
        fstart_pci::EcamDevice::read16(self, off as u16)
    }
    fn write16(&self, off: u32, value: u16) {
        fstart_pci::EcamDevice::write16(self, off as u16, value)
    }
    fn read8(&self, off: u32) -> u8 {
        fstart_pci::EcamDevice::read8(self, off as u16)
    }
    fn write8(&self, off: u32, value: u8) {
        fstart_pci::EcamDevice::write8(self, off as u16, value)
    }
}

/// The config space of one PCI function (dword accesses).
#[derive(Clone, Copy)]
pub struct ConfigWindow<P> {
    pci: P,
    function: PciAddress,
}

impl<P: ConfigRegionAccess> ConfigWindow<P> {
    /// # Safety
    /// The caller owns `function`'s config space.
    pub const unsafe fn new(pci: P, function: PciAddress) -> Self {
        Self { pci, function }
    }
}

impl<P: ConfigRegionAccess> Window for ConfigWindow<P> {
    fn read32(&self, off: u32) -> u32 {
        // SAFETY: `new`'s contract.
        unsafe { self.pci.read(self.function, off as u16) }
    }
    fn write32(&self, off: u32, value: u32) {
        // SAFETY: `new`'s contract.
        unsafe { self.pci.write(self.function, off as u16, value) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fch::model::Mem;
    use tock_registers::interfaces::ReadWriteable;
    use tock_registers::register_bitfields;

    register_bitfields![u8,
        T8 [
            A OFFSET(1) NUMBITS(2) [],
            B OFFSET(7) NUMBITS(1) []
        ]
    ];

    #[test]
    fn ops_and_typed_registers() {
        let m = Mem::default();
        m.put(0x10, 4, 0xffff_0000);
        apply(
            &m,
            &[
                Op::rmw(0x10, 4, 0xff00_0000, 0x12),
                Op::store(0x20, 1, 0xaa),
            ],
        );
        assert_eq!(m.get(0x10, 4), 0x00ff_0012);
        assert_eq!(m.writes(), vec![(0x10, 4, 0x00ff_0012), (0x20, 1, 0xaa)]);
        let r = m.reg::<u8, T8::Register>(0x20);
        r.modify(T8::A.val(1) + T8::B::CLEAR);
        assert_eq!(m.get(0x20, 1), 0x2a);
    }

    #[test]
    fn narrow_access_over_a_dword_space() {
        struct Dwords(Mem);
        impl Window for Dwords {
            fn read32(&self, off: u32) -> u32 {
                assert_eq!(off & 3, 0);
                self.0.read32(off)
            }
            fn write32(&self, off: u32, value: u32) {
                assert_eq!(off & 3, 0);
                self.0.write32(off, value)
            }
        }
        let d = Dwords(Mem::default());
        d.0.put(0x100, 4, 0x4433_2211);
        d.write8(0x102, 0xee);
        d.write16(0x100, 0xbeef);
        assert_eq!(d.0.get(0x100, 4), 0x44ee_beef);
        assert_eq!(d.read8(0x103), 0x44);
    }
}
