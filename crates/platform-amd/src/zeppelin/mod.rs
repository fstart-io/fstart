// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AMD Zeppelin (EPYC Embedded 3000) flow. One DRAM stage runs from
//! the PSP-released reset vector to the payload.
#[cfg(feature = "acpi")]
pub mod acpi;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod boot;
#[cfg(feature = "host")]
pub mod host;
pub mod layout;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod memory;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod pci;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod smm;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod stage;

extern crate alloc;

use alloc::vec::Vec;
use fstart_driver_amd::{
    ccx::tsc::Tsc,
    dxio::Descriptor,
    fch::{
        Acpimmio, Fch, NativeBus,
        early::PowerFailure,
        gpio::Gpio,
        lpc::Lpc,
        taishan::{Devices, irq::Route},
    },
    zeppelin::{Die, MAX_DIES},
};
use fstart_driver_uart::ns16550::Ns16550Config;
use fstart_pci::ecam::GlobalEcam;

#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
pub use stage::Program;

/// What the board hooks get: the dies and the clock.
pub struct BoardCtx<'a> {
    pub dies: &'a [Die<GlobalEcam>],
    pub clock: Tsc,
}
impl BoardCtx<'_> {
    /// The FCH of die 0, for pad routing and the like.
    pub fn fch(&mut self) -> Fch<NativeBus> {
        // SAFETY: board hooks run on the BSP between the platform's own FCH
        // accesses, with the ACPI MMIO and ECAM decoded; the borrow of the
        // context keeps them sequential.
        Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0)
    }

    /// The GPIO pads of die 0.
    pub fn gpio(&mut self) -> Gpio<NativeBus> {
        Gpio::new(self.fch(), &fstart_driver_amd::zeppelin::GPIO_BANK)
    }
}

/// Board wiring and hooks. The platform owns the order of calls.
pub trait Board: 'static {
    /// Subsystem ID for the FCH and PCIe devices.
    const SUBSYSTEM_ID: u32;
    /// AC power-loss policy.
    const POWER_FAILURE: PowerFailure = PowerFailure::Previous;
    /// FCH devices in use, per die.
    const FCH_DEVICES: [Devices; MAX_DIES] = [Devices {
        xhci: true,
        sata: true,
    }; MAX_DIES];
    /// The FCH PCI interrupt router (0xc00/0xc01) table.
    const IRQ_ROUTES: &'static [Route];
    /// GEVENT pins (bit n = GEVENTn) routed to the board's SMM handler,
    /// `fstart_driver_amd::fch::taishan::smm::SmmBoard::gevent`.
    const SMI_GEVENTS: u32 = 0;

    /// LPC decodes and Super I/O routing for the console, before any
    /// output.
    fn early_init(lpc: &Lpc<impl fstart_driver_amd::fch::Bus>);
    /// The board's console UART.
    fn console() -> Ns16550Config;
    /// After the FCH reset tasks, where the vendor board PEIMs run.
    fn fch_reset_done(_ctx: &mut BoardCtx<'_>) {}
    /// DXIO engines of a die.
    fn dxio_descriptors(ctx: &mut BoardCtx<'_>, die: u8) -> &'static [Descriptor];
    /// After DXIO link training, before PCI enumeration.
    fn dxio_training_done(_ctx: &mut BoardCtx<'_>) {}
    /// After PCI enumeration.
    fn mainboard_final(_ctx: &mut BoardCtx<'_>) {}
    /// Right before the payload.
    fn ready_to_boot(_ctx: &mut BoardCtx<'_>) {}
    /// What the board adds to the ACPI tables.
    fn acpi() -> BoardAcpi {
        BoardAcpi::default()
    }
}

/// Board ACPI content.
#[derive(Debug, Default)]
pub struct BoardAcpi {
    /// AML for `\_SB.PCI0.LPCB`: Super I/O and BMC devices.
    pub lpc_aml: Vec<u8>,
    /// Standalone tables (SPMI, ...).
    pub tables: Vec<Vec<u8>>,
}

/// Declare the board binary: `fstart_platform_amd::zeppelin_stage_bin!(Board);`
#[macro_export]
macro_rules! zeppelin_stage_bin {
    ($board:ty) => {
        $crate::__stage::stage_bin!(program: $crate::zeppelin::Program<$board>);
    };
}
