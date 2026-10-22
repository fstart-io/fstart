//! FCH: Taishan (Naples) and Huashan (Rome, Milan).
//!
//! Written from the SP3 FCH reports (`FCH_SP3.md`, `fch-contracts.json`) and
//! the public register names of the family 17h PPRs (55570 for the
//! ACPI-MMIO layout, which Taishan and Huashan share).
//!
//! # Layers
//!
//! - **Register access.** Every register space is a [`Window`]: byte
//!   offsets plus 8/16/32-bit accesses. [`Reg`] puts tock-registers
//!   bitfields on any window, [`Op`] tables replay the recovered fixed
//!   register programs. Windows: [`Mmio`] (the host view), [`SmnWindow`]
//!   (the FCH block of any socket or die over SMN, FCH_SP3 §4.2),
//!   [`Ports`] (x86 I/O), [`ConfigWindow`] (one PCI function) and
//!   [`acpimmio::PmIo`] (PM registers through `CD6h/CD7h`).
//! - **Blocks.** [`acpimmio`] (offsets and PM/SMI/MISC/AOAC fields),
//!   [`gpio`], [`lpc`], [`spi`] (and the flash windows, [`rom3`]),
//!   [`smbus`], [`irq`], [`uart`], [`smi`], [`alink`].
//! - **Tasks.** [`common`] holds the tasks both FCHs run (the eight PEI
//!   reset tasks, the DXE env and late tasks, FCH_SP3 §2.1–2.2);
//!   [`taishan`] and [`huashan`] hold the per-IP SATA, USB, xGbE and SMM
//!   parts; [`multi`] the secondary FCHs (Naples dies 1..7, Rome/Milan
//!   socket 1).
//!
//! Task functions take the hardware as [`FchHw`] and typed policy whose
//! `Default` is the donor default (FCH_SP3 §8).

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod acpimmio;
pub mod alink;
pub mod common;
pub mod gpio;
#[cfg(any(feature = "rome", feature = "milan"))]
pub mod huashan;
pub mod irq;
pub mod lpc;
pub mod multi;
pub mod rom3;
pub mod smbus;
pub mod smi;
pub mod spi;
mod tables;
#[cfg(feature = "naples")]
pub mod taishan;
pub mod uart;

use core::marker::PhantomData;

use fstart_core::services::Timer;
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
    fn reg<T: Width, R: RegisterLongName>(&self, off: u32) -> Reg<'_, Self, T, R>
    where
        Self: Sized,
    {
        Reg {
            window: self,
            off,
            _reg: PhantomData,
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

/// A register in a [`Window`]; implements the tock-registers interfaces.
pub struct Reg<'a, W: ?Sized, T, R: RegisterLongName = ()> {
    window: &'a W,
    off: u32,
    _reg: PhantomData<(T, R)>,
}

impl<W: Window + ?Sized, T: Width, R: RegisterLongName> Readable for Reg<'_, W, T, R> {
    type T = T;
    type R = R;
    fn get(&self) -> T {
        T::read(self.window, self.off)
    }
}

impl<W: Window + ?Sized, T: Width, R: RegisterLongName> Writeable for Reg<'_, W, T, R> {
    type T = T;
    type R = R;
    fn set(&self, value: T) {
        T::write(self.window, self.off, value)
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

/// Run `ops` in order.
pub fn apply<W: Window + ?Sized>(window: &W, ops: &[Op]) {
    ops.iter().for_each(|op| op.apply(window));
}

/// Read-modify-write one register: `x = (x & !clear) | set`.
pub fn update<T: Width, W: Window + ?Sized>(window: &W, off: u32, clear: T, set: T) {
    Op::rmw(off, size_of::<T>() as u8, clear.to_u32(), set.to_u32()).apply(window)
}

/// Clear an NBIF function's EXIST strap (`nbio::nbif::FUNC_STRAP0`): the
/// FCH tasks hide the SATA, USB and xGbE functions they turn off.
pub(crate) fn hide_nbif_function<W: Window>(smn: &W, strap: u32) {
    use crate::nbio::nbif::FUNC_STRAP0;
    use tock_registers::interfaces::ReadWriteable;
    smn.reg::<u32, FUNC_STRAP0::Register>(strap)
        .modify(FUNC_STRAP0::EXIST::CLEAR);
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
/// see [`gpio`] for how IOMUX is reached on remote FCHs.
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
        // SAFETY: `new`'s contract.
        unsafe { self.smn.read32(self.base + off) }
    }
    fn write32(&self, off: u32, value: u32) {
        // SAFETY: `new`'s contract.
        unsafe { self.smn.write32(self.base + off, value) }
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

/// The hardware of one FCH as the tasks see it.
///
/// - `acpi`: the FCH's ACPI-MMIO block (offsets from `0xFED8_0000`): the
///   host MMIO view on the primary FCH, an [`SmnWindow`] at
///   [`acpimmio::SMN_BASE`] or the relocated MMIO on a secondary;
/// - `mmio`: absolute physical MMIO (SPI `FEC1_0000h`, IOAPIC, `FEDCxxxxh`);
/// - `io`: x86 ports (PM1/GPE blocks, SMI command, PCI_INTR, A-Link, RTC);
/// - `pci`: config space; [`FchHw::smn`] uses the FCH's root bus.
pub trait FchHw {
    type Acpi: Window;
    type Mmio: Window;
    type Io: Window;
    type Pci: ConfigRegionAccess + Clone;

    fn acpi(&self) -> &Self::Acpi;
    fn mmio(&self) -> &Self::Mmio;
    fn io(&self) -> &Self::Io;
    fn pci(&self) -> &Self::Pci;
    /// Root bus whose host bridge reaches this FCH's die/socket over SMN.
    fn bus(&self) -> u8;
    fn timer(&self) -> &dyn Timer;

    fn smn(&self) -> Smn<Self::Pci> {
        Smn::new(self.pci().clone(), self.bus())
    }

    /// The whole SMN of this FCH's die/socket as a window (offset = SMN
    /// address).
    fn smn_window(&self) -> SmnWindow<Self::Pci> {
        // SAFETY: tasks own the FCH's SMN registers while they run.
        unsafe { SmnWindow::new(self.smn(), 0) }
    }

    /// The LPC bridge (D14F3) config space.
    fn lpc(&self) -> ConfigWindow<Self::Pci> {
        // SAFETY: tasks own the LPC bridge.
        unsafe { ConfigWindow::new(self.pci().clone(), lpc::lpc()) }
    }
}

/// The plain [`FchHw`] implementation.
pub struct Fch<A, M, I, P, T> {
    pub acpi: A,
    pub mmio: M,
    pub io: I,
    pub pci: P,
    pub bus: u8,
    pub timer: T,
}

impl<A, M, I, P, T> FchHw for Fch<A, M, I, P, T>
where
    A: Window,
    M: Window,
    I: Window,
    P: ConfigRegionAccess + Clone,
    T: Timer,
{
    type Acpi = A;
    type Mmio = M;
    type Io = I;
    type Pci = P;

    fn acpi(&self) -> &A {
        &self.acpi
    }
    fn mmio(&self) -> &M {
        &self.mmio
    }
    fn io(&self) -> &I {
        &self.io
    }
    fn pci(&self) -> &P {
        &self.pci
    }
    fn bus(&self) -> u8 {
        self.bus
    }
    fn timer(&self) -> &dyn Timer {
        &self.timer
    }
}

/// Poll `done` up to `tries` times with `delay_us` between tries. AGESA
/// polls several FCH status bits forever; every poll here is bounded and the
/// caller decides what a timeout means.
pub fn poll(timer: &dyn Timer, tries: u32, delay_us: u64, mut done: impl FnMut() -> bool) -> bool {
    (0..tries).any(|_| {
        if done() {
            return true;
        }
        timer.delay_us(delay_us);
        false
    }) || done()
}

#[cfg(test)]
pub(crate) mod model {
    //! A byte-addressed register model with a write log, and an FCH built
    //! from models.
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use fstart_core::services::Timer;

    use super::{Fch, Window};
    use crate::smn::model::Model;
    use crate::smu::{SMU_ARGS, SmuError, SmuMessages};

    #[derive(Default)]
    pub struct State {
        pub bytes: BTreeMap<u32, u8>,
        /// `(offset, width, value)` of every write.
        pub log: Vec<(u32, u8, u32)>,
        /// Values returned (once each, then sticky) by reads of an offset.
        pub reads: BTreeMap<u32, Vec<u32>>,
        /// Index/data pairs `data port -> index port`; the data port then
        /// reads and writes `indexed[(data, index)]`.
        pub pairs: BTreeMap<u32, u32>,
        pub indexed: BTreeMap<(u32, u32), u32>,
    }

    /// A memory-like register space; unwritten bytes read 0.
    #[derive(Clone, Default)]
    pub struct Mem(pub Rc<RefCell<State>>);

    impl Mem {
        /// I/O ports with the FCH index/data pairs (PM, AB, PCI_INTR, RTC).
        pub fn ports() -> Self {
            let m = Self::default();
            m.0.borrow_mut().pairs =
                [(0xcd7, 0xcd6), (0xcdc, 0xcd8), (0xc01, 0xc00), (0x71, 0x70)].into();
            m
        }
        /// The value behind `index` of the pair with data port `data`.
        pub fn at(&self, data: u32, index: u32) -> u32 {
            self.0
                .borrow()
                .indexed
                .get(&(data, index))
                .copied()
                .unwrap_or(0)
        }
        pub fn set_at(&self, data: u32, index: u32, value: u32) {
            self.0.borrow_mut().indexed.insert((data, index), value);
        }
        fn pair(&self, off: u32) -> Option<u32> {
            let index = *self.0.borrow().pairs.get(&off)?;
            let width = if index == 0xcd8 { 4 } else { 1 };
            Some(self.get(index, width))
        }
        pub fn get(&self, off: u32, width: u8) -> u32 {
            let s = self.0.borrow();
            (0..u32::from(width)).fold(0, |v, i| {
                v | u32::from(s.bytes.get(&(off + i)).copied().unwrap_or(0)) << (8 * i)
            })
        }
        pub fn put(&self, off: u32, width: u8, value: u32) {
            let mut s = self.0.borrow_mut();
            (0..u32::from(width)).for_each(|i| {
                s.bytes.insert(off + i, (value >> (8 * i)) as u8);
            });
        }
        pub fn writes(&self) -> Vec<(u32, u8, u32)> {
            self.0.borrow().log.clone()
        }
        pub fn writes_to(&self, off: u32) -> Vec<u32> {
            self.writes()
                .into_iter()
                .filter(|w| w.0 == off)
                .map(|w| w.2)
                .collect()
        }
        fn read(&self, off: u32, width: u8) -> u32 {
            let scripted = {
                let mut s = self.0.borrow_mut();
                s.reads.get_mut(&off).and_then(|v| {
                    if v.len() > 1 {
                        Some(v.remove(0))
                    } else {
                        v.first().copied()
                    }
                })
            };
            if let Some(index) = self.pair(off) {
                return scripted.unwrap_or_else(|| self.at(off, index));
            }
            scripted.unwrap_or_else(|| self.get(off, width))
        }
        fn write(&self, off: u32, width: u8, value: u32) {
            match self.pair(off) {
                Some(index) => self.set_at(off, index, value),
                None => self.put(off, width, value),
            }
            self.0.borrow_mut().log.push((off, width, value));
        }
    }

    impl Window for Mem {
        fn read32(&self, off: u32) -> u32 {
            self.read(off, 4)
        }
        fn write32(&self, off: u32, value: u32) {
            self.write(off, 4, value)
        }
        fn read16(&self, off: u32) -> u16 {
            self.read(off, 2) as u16
        }
        fn write16(&self, off: u32, value: u16) {
            self.write(off, 2, value.into())
        }
        fn read8(&self, off: u32) -> u8 {
            self.read(off, 1) as u8
        }
        fn write8(&self, off: u32, value: u8) {
            self.write(off, 1, value.into())
        }
    }

    /// A timer that only counts.
    #[derive(Default)]
    pub struct Clock(pub AtomicU64);
    impl Timer for Clock {
        fn delay_us(&self, us: u64) {
            self.0.fetch_add(us, Ordering::Relaxed);
        }
        fn timestamp_us(&self) -> u64 {
            self.0.load(Ordering::Relaxed)
        }
    }

    /// An SMU that answers OK and records `(id, args)`.
    #[derive(Clone, Default)]
    pub struct Smu(pub Rc<RefCell<Vec<Message>>>);
    pub type Message = (u32, [u32; SMU_ARGS]);
    impl SmuMessages for Smu {
        fn send(&self, id: u32, args: &mut [u32; SMU_ARGS]) -> Result<(), SmuError> {
            self.0.borrow_mut().push((id, *args));
            Ok(())
        }
    }
    impl Smu {
        pub fn ids(&self) -> Vec<u32> {
            self.0.borrow().iter().map(|m| m.0).collect()
        }
    }

    pub type TestFch = Fch<Mem, Mem, Mem, Model, Clock>;

    pub fn fch() -> TestFch {
        let pci = Model::default();
        // The LPC bridge and SMBus function read 0 until written.
        (0..0x100).step_by(4).for_each(|off| {
            pci.0.borrow_mut().config.insert((0, 0x14, 3, off), 0);
            pci.0.borrow_mut().config.insert((0, 0x14, 0, off), 0);
        });
        Fch {
            acpi: Mem::default(),
            mmio: Mem::default(),
            io: Mem::ports(),
            pci,
            bus: 0,
            timer: Clock::default(),
        }
    }

    /// SMN dword at `addr` on `bus`.
    pub fn smn(pci: &Model, bus: u8, addr: u32) -> u32 {
        pci.0.borrow().smn.get(&(bus, addr)).copied().unwrap_or(0)
    }

    /// SMN writes on `bus` in order: `(addr, value)`.
    pub fn smn_writes(pci: &Model, bus: u8) -> Vec<(u32, u32)> {
        pci.0
            .borrow()
            .log
            .iter()
            .filter(|w| w.0 == bus)
            .map(|w| (w.1, w.2))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::model::Mem;
    use super::*;
    use tock_registers::interfaces::ReadWriteable;
    use tock_registers::register_bitfields;

    register_bitfields![u8, T8 [A OFFSET(1) NUMBITS(2) [], B OFFSET(7) NUMBITS(1) []]];

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
