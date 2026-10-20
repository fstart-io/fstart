// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AMD Zeppelin (EPYC Embedded 3000) flow. One DRAM stage runs from
//! the PSP-released reset vector to the payload.
#[cfg(feature = "acpi")]
pub mod acpi;
#[cfg(feature = "host")]
pub mod host;
pub mod layout;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod boot;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod cpu;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod memory;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod pci;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod stage;

extern crate alloc;

use alloc::vec::Vec;
use fstart_driver_amd::{
    fch::early::PowerFailure,
    lpc::Lpc,
    zeppelin::{
        MAX_DIES,
        dxio::Descriptor,
        fch::{self, Devices, irq::Route},
        topology::Die,
    },
};
use fstart_driver_uart::ns16550::Ns16550Config;
use fstart_pci::{ConfigRegionAccess, ecam::GlobalEcam};

#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
pub use stage::Program;

/// The TSC as the time base, at its P0 frequency.
#[derive(Debug, Clone, Copy)]
pub struct Clock {
    pub(crate) mhz: u32,
}
#[cfg(target_arch = "x86_64")]
impl Clock {
    pub fn delay_us(&self, us: u32) {
        fstart_driver_amd::fam17h::tsc::delay_us(self.mhz, us)
    }
    pub fn now_us(&self) -> u64 {
        // SAFETY: RDTSC has no side effects.
        unsafe { core::arch::x86_64::_rdtsc() / u64::from(self.mhz) }
    }
}
#[cfg(target_arch = "x86_64")]
impl embedded_hal::delay::DelayNs for Clock {
    fn delay_ns(&mut self, ns: u32) {
        Clock::delay_us(self, ns.div_ceil(1000))
    }
    fn delay_us(&mut self, us: u32) {
        Clock::delay_us(self, us)
    }
}

/// What the board hooks get: the dies and the clock.
pub struct BoardCtx<'a> {
    pub dies: &'a [Die<GlobalEcam>],
    pub clock: Clock,
}
impl BoardCtx<'_> {
    /// The FCH registers of die 0, for pad routing and the like.
    pub fn fch(&mut self) -> fch::NativeBus {
        // SAFETY: board hooks run on the BSP between the platform's own FCH
        // accesses, with the ACPI MMIO and ECAM decoded; the borrow of the
        // context keeps them sequential.
        unsafe { fch::NativeBus::new() }
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

    /// LPC decodes and Super I/O routing for the console, before any
    /// output.
    fn early_init(lpc: &mut Lpc<impl ConfigRegionAccess>);
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
