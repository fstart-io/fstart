// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native AMD Zeppelin (EPYC Embedded 3000) flow. One DRAM stage runs from
//! the PSP-released reset vector to the payload.
#[cfg(feature = "host")]
pub mod host;
pub mod layout;
#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
mod stage;

use fstart_driver_amd::lpc::Lpc;
use fstart_driver_uart::ns16550::Ns16550Config;
use fstart_pci::ConfigRegionAccess;

#[cfg(all(feature = "bundle-zeppelin", target_os = "none"))]
pub use stage::Program;

/// Board wiring and hooks. The platform owns the order of calls.
pub trait Board: 'static {
    /// LPC decodes and Super I/O routing for the console, before any
    /// output.
    fn early_init(lpc: &mut Lpc<impl ConfigRegionAccess>);
    /// The board's console UART.
    fn console() -> Ns16550Config;
}

/// Declare the board binary: `fstart_platform_amd::zeppelin_stage_bin!(Board);`
#[macro_export]
macro_rules! zeppelin_stage_bin {
    ($board:ty) => {
        $crate::__stage::stage_bin!(program: $crate::zeppelin::Program<$board>);
    };
}
