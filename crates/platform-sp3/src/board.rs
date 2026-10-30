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

use fstart_driver_amd::sp3::dxio::DxioPolicy;
use fstart_driver_amd::sp3::fch::common::env::EnvPolicy;
use fstart_driver_amd::sp3::fch::gpio::Pad;
use fstart_driver_amd::sp3::nbio::NbioPolicy;
use fstart_driver_amd::sp3::soc::Generation;
use fstart_driver_amd::sp3::{ccx, df};
use fstart_driver_uart::ns16550::Ns16550Config;

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
    /// Per-socket AHCI policy, used consistently at reset, env and late.
    /// A secondary socket with SATA lanes must not inherit the donor's
    /// all-disabled secondary-controller policy.
    #[cfg(any(feature = "rome", feature = "milan"))]
    pub sata: [fstart_driver_amd::sp3::fch::huashan::sata::SataPolicy; 2],
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
    /// The detected board/slot configuration is not supported.
    Configuration,
}

/// The board contract of the SP3 flow. Implemented once per board crate;
/// the flow calls the generation-dependent hooks only with a generation in
/// [`Sp3BoardFacts::GENERATIONS`] (the running CPU's).
pub trait Board: Sp3BoardFacts + 'static {
    /// Shown in the log.
    const NAME: &'static str;

    /// Board identity only. CPU packages, DIMMs and memory ranges are
    /// discovered by shared drivers; do not describe the current bench here.
    #[cfg(feature = "acpi")]
    fn smbios_identity() -> fstart_acpi::smbios::SmbiosIdentity<'static> {
        fstart_acpi::smbios::SmbiosIdentity::new("", Self::NAME)
    }
    /// The firmware console: an MMIO or LPC-connected 16550.
    const CONSOLE: Ns16550Config;

    /// Silicon policy for `generation`.
    fn policy(generation: Generation) -> Policy;

    /// Pads configured and read before [`Board::gpio`] (M.2 SATA/PCIe
    /// detect); the board's flow gets their levels, in this order, to pick its DXIO
    /// lane map (`Stage::pad_levels`).
    fn detect_pads(generation: Generation) -> &'static [Pad] {
        let _ = generation;
        &[]
    }

    /// GPIO program after the detect pads.
    fn gpio(generation: Generation) -> Gpio;

    /// A discrete TPM may be fitted (FIFO interface at `0xFED4_0000`, on
    /// LPC or eSPI): probe it, measure the boot (`fstart-tcg`) and describe
    /// it and its event log in ACPI. Off leaves a TPM untouched and hidden.
    const DISCRETE_TPM: bool = false;

    /// BMC sleep-state notification through SMM (ACPI and the handler).
    const BMC_SX_NOTIFY: Option<crate::smm::SleepNotify> = None;

    /// AML inside `\_SB.PCI0.LPCB` (Super I/O devices).
    #[cfg(feature = "acpi")]
    fn acpi_lpc_aml() -> alloc::vec::Vec<u8>;

    /// The board's ACPI inputs over [`Board::acpi_lpc_aml`]'s output.
    #[cfg(feature = "acpi")]
    fn acpi_board(lpc_aml: &[u8]) -> crate::acpi::Board<'_>;
}
