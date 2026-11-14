// SPDX-License-Identifier: MIT OR Apache-2.0
//! The FCH of every Family 17h SoC: Raven/Picasso's Sandstone, the
//! Taishan of the Zeppelin dies (the die SoC's and Naples') and the Huashan
//! of the Rome and Milan IODs.
//!
//! # Access
//!
//! Every FCH is reached through one handle, [`FchHw`]: its register spaces
//! as windows ([`crate::window`]), the same code for FCH 0 of any SoC
//! ([`Native`]) and for a secondary FCH that the x86 reaches over SMN or
//! through relocated MMIO. A register is defined once, as an
//! [`crate::window::At`] in its window: the ACPI MMIO banks
//! ([`acpimmio`], below) are offsets in [`FchHw::acpi`], the SPI
//! controller's absolute addresses in [`FchHw::mmio`], the LPC bridge's
//! offsets in [`FchHw::lpc`]. The access widths are those of the vendor
//! code: several of these registers have neighbours with side effects.
//!
//! ```ignore
//! fch.acpi().at(PM_DECODE).modify(Decode::CF9::SET);
//! ```
//!
//! # Blocks and tasks
//!
//! - **Blocks.** [`acpimmio`] (PM, SMI, MISC and AOAC fields, the PM
//!   index/data view), [`gpio`], [`lpc`], [`spi`] (and the flash windows,
//!   [`rom3`]), [`smbus`], [`irq`], [`uart`], [`smi`], [`alink`], [`espi`],
//!   [`emmc`], [`i2c`]; the ACPI description in [`aml`].
//! - **Tasks.** [`early`] holds the pre-console setup of the die SoCs;
//!   [`tasks`] the vendor reset, env and late tasks of the SP3 FCHs
//!   (FCH_SP3 §2.1–2.2); [`sandstone`], [`taishan`] and [`huashan`] what
//!   each FCH version does differently; [`multi`] the SP3 secondary FCHs
//!   (Naples dies 1..7, Rome/Milan socket 1).
//!
//! Written from the vendor code of each SoC (the SP3 FCH reports
//! `FCH_SP3.md` and `fch-contracts.json`, the sp4r2 report `FCH_TAISHAN`,
//! coreboot's Picasso support) and the public register names of the family
//! 17h PPRs (55570 for the ACPI-MMIO layout). Task functions take the
//! hardware as [`FchHw`] and typed policy whose `Default` is the donor
//! default.
use crate::smn::Smn;
use crate::window::{At, ConfigWindow, Mmio, Ports, Reg, SmnWindow, Window};
use fstart_core::indexed::Indexed;
use fstart_core::services::Timer;
use fstart_pci::ecam::GlobalEcam;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::{RegisterLongName, interfaces::Writeable};
pub mod acpimmio;
pub mod alink;
#[cfg(feature = "acpi")]
pub mod aml;
pub mod early;
pub mod emmc;
pub mod espi;
#[cfg(test)]
mod espi_tests;
pub mod gpio;
#[cfg(test)]
mod gpio_tests;
pub mod huashan;
pub mod i2c;
pub mod irq;
pub mod lpc;
#[cfg(test)]
pub(crate) mod model;
pub mod multi;
pub mod rom3;
pub mod rtc;
pub mod sandstone;
pub mod smbus;
pub mod smi;
pub mod spi;
pub mod sx;
pub(crate) mod tables;
pub mod taishan;
pub mod tasks;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod uart;

/// FCH ACPI MMIO window of die 0 (PM, SMI, GPIO, IOMUX, MISC, AOAC).
pub const ACPI_MMIO: u32 = 0xfed8_0000;
/// SPI controller/eSPI aperture of every Family 17h FCH.
pub const SPI_BASE: u32 = 0xfec1_0000;
/// The FCH IOAPIC (GSI 0-23).
pub const IOAPIC: u32 = 0xfec0_0000;
/// The fixed ACPI SCI input, shared by interrupt routing and the FADT/MADT.
pub const SCI_IRQ: u8 = 9;
/// The HPET.
pub const HPET: u32 = 0xfed0_0000;
/// The HPET's minimum clock tick the vendor firmware's HPET table reports
/// (AMI SbDxe).
pub const HPET_MIN_TICK: u16 = 0x37ee;
/// A-Link-to-AHB bridge and its AHB devices.
pub const AL2AHB: u32 = 0xfedc_0000;

/// The POST code port, which the FCH forwards to LPC/eSPI (a BMC's POST
/// snoop, a debug card).
pub const POST_CODE: At<u8> = At::new(0x80);

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
        self.config(lpc::function())
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

/// A POST code to the port 80 snoop.
pub fn post(fch: &impl FchHw, code: u8) {
    fch.io().at(POST_CODE).set(code);
}

/// A POST code to the port 80 snoop, without an FCH handle: the port is
/// the same on every SoC.
pub fn post_native(code: u8) {
    // SAFETY: an I/O port write only.
    unsafe { Ports::new() }.at(POST_CODE).set(code);
}

/// An 8-bit register behind an I/O index/data pair (`data = index + 1`).
pub type IndexedIo<'a, W, R = ()> = Indexed<Reg<&'a W, u8>, Reg<&'a W, u8>, R>;
pub fn indexed_io<W: Window, R: RegisterLongName>(
    io: &W,
    index: u16,
    at: u8,
) -> IndexedIo<'_, W, R> {
    crate::window::indexed(io, index.into(), u32::from(index) + 1, at)
}

/// Legacy PM index/data I/O; reaches whichever FCH the DF routes I/O to.
pub const PM_INDEX: u16 = 0xcd6;

#[cfg(test)]
mod tests;
