//! Any FCH of a multi-socket SoC (a ZP die's, an IOD's) through windows:
//! the same code serves the primary FCH and the secondary ones, which the
//! x86 reaches over SMN. Used by the Taishan of the SP3 ZP dies and by
//! Huashan (SSP, GN), and FCH 0 of every SoC ([`Native`]).
//!
//! Written from the SP3 FCH reports (`FCH_SP3.md`, `fch-contracts.json`) and
//! the public register names of the family 17h PPRs (55570 for the
//! ACPI-MMIO layout, which Taishan and Huashan share).
//!
//! # Layers
//!
//! - **Register access.** Every register space is a [`Window`]: byte
//!   offsets plus 8/16/32-bit accesses. [`Reg`](crate::window::Reg) puts
//!   tock-registers bitfields on any window, [`Op`](crate::window::Op)
//!   tables replay the recovered fixed register programs. Windows:
//!   [`Mmio`](crate::window::Mmio) (the host view), [`SmnWindow`]
//!   (the FCH block of any socket or die over SMN, FCH_SP3 §4.2),
//!   [`Ports`](crate::window::Ports) (x86 I/O), [`ConfigWindow`] (one PCI function) and
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
pub mod lpc;
pub mod multi;
pub mod rom3;
pub mod smbus;
pub mod smi;
pub mod spi;
pub(crate) mod tables;
pub mod tasks;
pub mod uart;

use fstart_core::services::Timer;
use fstart_pci::ecam::GlobalEcam;
use fstart_pci::{ConfigRegionAccess, PciAddress};

use crate::smn::Smn;
use crate::window::{ConfigWindow, Mmio, Ports, SmnWindow, Window};

/// Clear an NBIF function's EXIST strap (`nbio::nbif::FUNC_STRAP0`): the
/// FCH tasks hide the SATA, USB and xGbE functions they turn off.
pub(crate) fn hide_nbif_function<W: Window>(smn: &W, strap: u32) {
    use crate::nbio::nbif::FUNC_STRAP0;
    use tock_registers::interfaces::ReadWriteable;
    smn.reg::<u32, FUNC_STRAP0::Register>(strap)
        .modify(FUNC_STRAP0::EXIST::CLEAR);
}

/// The hardware of one FCH as the tasks see it.
///
/// - `acpi`: the FCH's ACPI-MMIO block (offsets from `0xFED8_0000`): the
///   host MMIO view on the primary FCH, an [`SmnWindow`] at
///   [`acpimmio::SMN_BASE`] or the relocated MMIO on a secondary;
/// - `mmio`: absolute physical MMIO (SPI `FEC1_0000h`, IOAPIC, `FEDCxxxxh`);
/// - `io`: x86 ports (PM1/GPE blocks, SMI command, PCI_INTR, A-Link, RTC);
/// - `config`: the config space of one of its functions, the LPC bridge
///   ([`FchHw::lpc`]) or the SMBus function ([`FchHw::smbus`]);
/// - `pci`: config space; [`FchHw::smn`] uses the FCH's root bus.
pub trait FchHw {
    type Acpi: Window;
    type Mmio: Window;
    type Io: Window;
    type Config: Window;
    type Pci: ConfigRegionAccess + Clone;
    type Timer: Timer;

    fn acpi(&self) -> &Self::Acpi;
    fn mmio(&self) -> &Self::Mmio;
    fn io(&self) -> &Self::Io;
    /// The config space of `function`.
    fn config(&self, function: PciAddress) -> Self::Config;
    fn pci(&self) -> &Self::Pci;
    /// Root bus whose host bridge reaches this FCH's die/socket over SMN.
    fn bus(&self) -> u8;
    fn timer(&self) -> &Self::Timer;

    fn smn(&self) -> Smn<Self::Pci> {
        // SAFETY: the BSP owns this bus's SMN index/data pair during init.
        unsafe { Smn::on_bus(self.pci().clone(), self.bus()) }
    }

    /// The whole SMN of this FCH's die/socket as a window (offset = SMN
    /// address).
    fn smn_window(&self) -> SmnWindow<Self::Pci> {
        // SAFETY: tasks own the FCH's SMN registers while they run.
        unsafe { SmnWindow::new(self.smn(), 0) }
    }

    /// The LPC bridge (D14F3) config space.
    fn lpc(&self) -> Self::Config {
        self.config(lpc::lpc())
    }

    /// The SMBus function (D14F0) config space.
    fn smbus(&self) -> Self::Config {
        self.config(smbus::function())
    }
}

/// The plain [`FchHw`] implementation, with dword config accesses.
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
    type Config = ConfigWindow<P>;
    type Pci = P;
    type Timer = T;

    fn acpi(&self) -> &A {
        &self.acpi
    }
    fn mmio(&self) -> &M {
        &self.mmio
    }
    fn io(&self) -> &I {
        &self.io
    }
    fn config(&self, function: PciAddress) -> ConfigWindow<P> {
        // SAFETY: tasks own the FCH's functions.
        unsafe { ConfigWindow::new(self.pci.clone(), function) }
    }
    fn pci(&self) -> &P {
        &self.pci
    }
    fn bus(&self) -> u8 {
        self.bus
    }
    fn timer(&self) -> &T {
        &self.timer
    }
}

/// FCH 0 of any SoC as the host sees it: its ACPI-MMIO block at
/// `FED8_0000h`, the physical MMIO, the ports and its functions through
/// ECAM with exact-width accesses.
pub struct Native<T> {
    acpi: Mmio,
    mmio: Mmio,
    io: Ports,
    timer: T,
}

impl<T: Timer> Native<T> {
    /// # Safety
    /// The caller owns the FCH's ACPI-MMIO block, the MMIO and ports it
    /// decodes and its config space, all decoded (ECAM published through
    /// `fstart_pci::ecam::init`) and mapped UC.
    pub const unsafe fn new(timer: T) -> Self {
        // SAFETY: the caller's contract.
        unsafe {
            Self {
                acpi: Mmio::new(acpimmio::BASE as usize),
                mmio: Mmio::new(0),
                io: Ports::new(),
                timer,
            }
        }
    }
}

impl<T: Timer> FchHw for Native<T> {
    type Acpi = Mmio;
    type Mmio = Mmio;
    type Io = Ports;
    type Config = fstart_pci::EcamDevice;
    type Pci = GlobalEcam;
    type Timer = T;

    fn acpi(&self) -> &Mmio {
        &self.acpi
    }
    fn mmio(&self) -> &Mmio {
        &self.mmio
    }
    fn io(&self) -> &Ports {
        &self.io
    }
    fn config(&self, function: PciAddress) -> fstart_pci::EcamDevice {
        fstart_pci::EcamDevice::new_address(function)
    }
    fn pci(&self) -> &GlobalEcam {
        &GlobalEcam
    }
    fn bus(&self) -> u8 {
        0
    }
    fn timer(&self) -> &T {
        &self.timer
    }
}

#[cfg(test)]
pub(crate) mod model {
    //! A byte-addressed register model with a write log, and an FCH built
    //! from models.
    use fstart_core::services::ManualTimer;
    use std::cell::RefCell;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    use super::Fch;
    use crate::smn::model::Model;
    use crate::smu::{ARGUMENT_COUNT, Error as SmuError, SmuMessages};
    use crate::window::Window;

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

    /// An SMU that answers OK and records `(id, args)`.
    #[derive(Clone, Default)]
    pub struct Smu(pub Rc<RefCell<Vec<Message>>>);
    pub type Message = (u32, [u32; ARGUMENT_COUNT]);
    impl SmuMessages for Smu {
        fn send(&self, id: u32, args: &mut [u32; ARGUMENT_COUNT]) -> Result<(), SmuError> {
            self.0.borrow_mut().push((id, *args));
            Ok(())
        }
    }
    impl Smu {
        pub fn ids(&self) -> Vec<u32> {
            self.0.borrow().iter().map(|m| m.0).collect()
        }
    }

    pub type TestFch = Fch<Mem, Mem, Mem, Model, ManualTimer>;

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
            timer: ManualTimer::new(),
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
    use crate::window::{Op, apply};
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
