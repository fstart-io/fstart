//! What a board supplies to the SP3 flow besides its facts
//! ([`crate::board::Board`]): its silicon policy and the knobs of it
//! ([`Config`]), its GPIO program and its DXIO lane map
//! given what the board detected. Everything else the flow derives from
//! the silicon (topology, APOB, fuses). One build serves every SoC the
//! board lists; the generation-dependent hooks get the generation of the
//! running CPU. The policies are plain driver policy values whose
//! `Default` is the donor default; a board only overrides what its setup
//! defaults change (e.g. `boards/tyan/s8030/src/policy.rs`).

extern crate alloc;

use fstart_driver_amd::fch::gpio::Pad;

pub use crate::sp3::silicon::Policy;

/// Knobs of the silicon [`Policy`] a board can offer in
/// [`Config::options`].
pub mod knob {
    use fstart_driver_amd::fch::hw::tasks::env::PowerFailState;
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

/// A board's configuration of the socket's SoCs: its silicon policy, and
/// the knobs of it the user may override (`fstart_options`). The flow
/// applies them when it has the console, after the FCH reset tasks: a knob
/// may change any policy but [`Policy::sata`], which the reset tasks
/// already used. Board knobs outside the policy go through
/// `Stage::resolve_options`.
pub struct Config {
    pub policy: Policy,
    pub options: &'static [fstart_options::Knob<Policy>],
}
