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

use fstart_driver_amd::sp3::fch::gpio::Pad;

use crate::sp3::layout::Sp3BoardFacts;

pub use fstart_driver_amd::sp3::soc::Policy;

/// Knobs of the silicon [`Policy`] a board can offer in
/// [`Board::OPTIONS`].
pub mod knob {
    use fstart_driver_amd::sp3::fch::common::env::PowerFailState;
    use fstart_options::{Kind, Knob, Meta};

    use super::Policy;

    /// What the system does when AC power returns.
    pub const POWER_FAIL: Knob<Policy> = Knob::new(
        Meta {
            key: "power_on_after_fail",
            form: "Power",
            label: "Restore on AC power loss",
            help: "What the system does when AC power returns.",
            kind: Kind::Enum(PowerFailState::CHOICES),
        },
        |p| p.fch_env.power_fail.option(),
        |p, v| p.fch_env.power_fail = PowerFailState::from_option(v),
    );
}

/// The board's FCH GPIO program (`Stage::program_gpio`), applied in this
/// order after the M.2 / strap detect pads were read.
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

/// The board facts the SP3 phases read. The board's silicon policy,
/// console, pads, GPIO table and table contributions are arguments to the
/// phases its `main` calls.
pub trait Board: Sp3BoardFacts + 'static {
    /// Shown in the log.
    const NAME: &'static str;

    /// A discrete TPM may be fitted (FIFO interface at `0xFED4_0000`, on
    /// LPC or eSPI): probe it, measure the boot (`fstart-tcg`) and describe
    /// it and its event log in ACPI. Off leaves a TPM untouched and hidden.
    const DISCRETE_TPM: bool = false;

    /// BMC sleep-state notification through SMM (ACPI and the handler).
    const BMC_SX_NOTIFY: Option<crate::sp3::smm::SleepNotify> = None;

    /// The silicon policy knobs the user may override (`fstart_options`).
    /// The flow applies them when it has the console, after the FCH reset
    /// tasks: a knob may change any policy but [`Policy::sata`], which the
    /// reset tasks already used. Board knobs outside the policy go through
    /// `Stage::resolve_options`.
    const OPTIONS: &'static [fstart_options::Knob<Policy>] = &[];
}
