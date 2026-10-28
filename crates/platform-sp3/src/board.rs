//! What a board supplies to the SP3 flow.
//!
//! A board is data plus three small hooks: the FCH-side bring-up of its
//! console path, its GPIO pads and its DXIO lane map given what the board
//! detected. Everything else the flow derives from the silicon (topology,
//! APOB, fuses). One build serves every generation the board lists
//! ([`Sp3BoardFacts::GENERATIONS`]); the generation-dependent hooks get the
//! generation of the running CPU. The policies are plain driver policy values whose
//! `Default` is the donor default; a board only overrides what its setup
//! defaults change (e.g. `boards/tyan/s8030/src/policy.rs`).

extern crate alloc;

use fstart_core::services::Timer;
use fstart_driver_amd::sp3::dxio::DxioPolicy;
use fstart_driver_amd::sp3::dxio::engine::MAX_ENGINES;
use fstart_driver_amd::sp3::dxio::lanes::Engine;
use fstart_driver_amd::sp3::fch::FchHw;
use fstart_driver_amd::sp3::fch::common::env::EnvPolicy;
use fstart_driver_amd::sp3::fch::gpio::Pad;
use fstart_driver_amd::sp3::nbio::NbioPolicy;
use fstart_driver_amd::sp3::soc::Generation;
use fstart_driver_amd::sp3::{ccx, df};
use fstart_driver_uart::ns16550::Ns16550Config;
use heapless::Vec;

use crate::layout::Sp3BoardFacts;

/// Silicon policy of a board (NATIVE_PORT_PLAN §4 steps 6–12).
#[derive(Debug, Clone, Copy)]
pub struct Policy {
    /// DF resource planner: the generation's split plus bus/IO requests.
    pub df: df::plan::Policy,
    pub nbio: NbioPolicy,
    /// SMU PPTable inputs (Rome/Milan; Naples has no PPTable).
    #[cfg(any(feature = "rome", feature = "milan"))]
    pub power: fstart_driver_amd::smu::v11::pptable::PowerPolicy,
    pub ccx: ccx::Policy,
    /// FCH env: power-failure state after AC loss.
    pub fch_env: EnvPolicy,
    pub dxio: DxioPolicy,
}

/// The board's FCH GPIO program, applied in this order after the M.2 /
/// strap detect pads were read.
#[derive(Debug, Clone, Copy)]
pub struct Gpio {
    /// OEM pads the vendor programs before its table (e.g. S8030 Rome
    /// GPIO4/5).
    pub early: &'static [Pad],
    /// The board GPIO table (CPM `$A10`).
    pub table: &'static [Pad],
}

/// A board hook failed; the flow halts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardError {
    /// A board device (Super I/O, BMC, ...) did not respond.
    Device,
}

/// The board contract of the SP3 flow. Implemented once per board crate;
/// the flow calls the generation-dependent hooks only with a generation in
/// [`Sp3BoardFacts::GENERATIONS`] (the running CPU's).
pub trait Board: Sp3BoardFacts + 'static {
    /// Shown in the log.
    const NAME: &'static str;
    /// The firmware console (a 16550 behind the board's LPC decodes).
    const CONSOLE: Ns16550Config;

    /// Silicon policy for `generation`.
    fn policy(generation: Generation) -> Policy;

    /// After socket 0's FCH reset tasks, before the console: open the
    /// board's LPC/eSPI decodes and set up its Super I/O so the console
    /// UART answers. Also runs for a CPU the board does not list (when its
    /// generation is compiled in) so the flow can log why it halts.
    fn early_init<H: FchHw>(fch: &H) -> Result<(), BoardError>;

    /// Pads configured and read before [`Board::gpio`] (M.2 SATA/PCIe
    /// detect); their levels go to [`Board::dxio_engines`] in this order.
    fn detect_pads(generation: Generation) -> &'static [Pad] {
        let _ = generation;
        &[]
    }

    /// GPIO program after the detect pads.
    fn gpio(generation: Generation) -> Gpio;

    /// The DXIO engines of every socket given the detect-pad levels. The
    /// board may query its BMC here (the console and LPC decodes are up);
    /// `timer` paces such a query.
    fn dxio_engines(
        generation: Generation,
        levels: &[bool],
        timer: &dyn Timer,
    ) -> Vec<Engine<'static>, MAX_ENGINES>;

    /// BMC sleep-state notification through SMM (ACPI and the handler).
    const BMC_SX_NOTIFY: Option<crate::smm::BmcSxNotify> = None;

    /// AML inside `\_SB.PCI0.LPCB` (Super I/O devices).
    #[cfg(feature = "acpi")]
    fn acpi_lpc_aml() -> alloc::vec::Vec<u8>;

    /// The board's ACPI inputs over [`Board::acpi_lpc_aml`]'s output.
    #[cfg(feature = "acpi")]
    fn acpi_board(lpc_aml: &[u8]) -> crate::acpi::Board<'_>;
}
