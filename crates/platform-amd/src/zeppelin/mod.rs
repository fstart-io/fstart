// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AMD Zeppelin (EPYC Embedded 3000) flow. One DRAM stage runs from
//! the PSP-released reset vector to the payload.
pub mod acpi;
#[cfg(feature = "host")]
pub mod host;
pub mod layout;
pub mod silicon;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod stage;
#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
mod tests;

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
    soc::{
        Soc,
        zeppelin::{Die, MAX_DIES, Zeppelin},
    },
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
        Gpio::new(self.fch(), Zeppelin::GPIO_BANK)
    }
}

/// A board's Zeppelin configuration, by IP block.
#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// Subsystem ID of the FCH, NBIO and PCIe functions.
    pub subsystem_id: u32,
    pub fch: FchConfig,
    pub dxio: DxioConfig,
}

/// The Taishan FCH.
#[derive(Debug, Clone, Copy)]
pub struct FchConfig {
    /// AC power-loss policy.
    pub power_failure: PowerFailure,
    /// FCH devices in use, per die.
    pub devices: [Devices; MAX_DIES],
    /// The PCI interrupt router (0xc00/0xc01) table.
    pub irq_routes: &'static [Route],
    /// GEVENT pins (bit n = GEVENTn) routed to the board's SMM handler,
    /// `fstart_driver_amd::fch::taishan::smm::SmmBoard::gevent`.
    pub smi_gevents: u32,
}
impl FchConfig {
    pub const DEFAULT: Self = Self {
        power_failure: PowerFailure::Previous,
        devices: [Devices {
            xhci: true,
            sata: true,
        }; MAX_DIES],
        irq_routes: &[],
        smi_gevents: 0,
    };
}
impl Default for FchConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The DXIO engines (PCIe, SATA lanes) of each die.
#[derive(Debug, Clone, Copy, Default)]
pub struct DxioConfig {
    pub descriptors: [&'static [Descriptor]; MAX_DIES],
}

/// Board wiring and hooks. The platform owns the order of calls.
pub trait Board: 'static {
    const CONFIG: &'static Config;

    /// LPC decodes and Super I/O routing for the console, before any
    /// output.
    fn early_init(lpc: &Lpc<impl fstart_driver_amd::fch::Bus>);
    /// The board's console UART.
    fn console() -> Ns16550Config;
    /// After the FCH reset tasks, where the vendor board PEIMs run.
    fn fch_reset_done(_ctx: &mut BoardCtx<'_>) {}
    /// After DXIO link training, before PCI enumeration.
    fn dxio_training_done(_ctx: &mut BoardCtx<'_>) {}
    /// After PCI enumeration.
    fn mainboard_final(_ctx: &mut BoardCtx<'_>) {}
    /// Right before the payload.
    fn ready_to_boot(_ctx: &mut BoardCtx<'_>) {}
    /// What the board adds to the ACPI tables.
    fn acpi() -> crate::acpi::BoardAcpi {
        crate::acpi::BoardAcpi::default()
    }
}

/// Declare the board binary: `fstart_platform_amd::zeppelin_stage_bin!(Board);`
#[macro_export]
macro_rules! zeppelin_stage_bin {
    ($board:ty) => {
        $crate::__stage::stage_bin!(program: $crate::zeppelin::Program<$board>);
    };
}
