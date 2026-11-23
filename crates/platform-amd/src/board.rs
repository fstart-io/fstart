// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a board is to the platform: the SoCs it carries and the facts the
//! flow's services and the host plan read ([`Board`]). Its configuration of
//! each SoC is the argument of that SoC's first phase
//! ([`crate::flow::Identified::enter`]), and its own work runs in its stage
//! `main` between the phases:
//!
//! ```ignore
//! fn main() -> ! {
//!     match fstart_platform_amd::flow::start::<Board>() {
//!         Cpu::Zeppelin(id) => flow(id.enter(CONFIG)),
//!         cpu => cpu.unsupported(),
//!     }
//! }
//! ```

/// The SoCs of the platform, as a board lists the ones it carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SocId {
    Picasso,
    Zeppelin,
    Naples,
    Rome,
    Milan,
    Genoa,
    Siena,
}

impl SocId {
    /// The name of the SoC's firmware tables
    /// (`[package.metadata.fstart.amd.<name>]`) and of its log prefix.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Picasso => "picasso",
            Self::Zeppelin => "zeppelin",
            Self::Naples => "naples",
            Self::Rome => "rome",
            Self::Milan => "milan",
            Self::Genoa => "genoa",
            Self::Siena => "siena",
        }
    }

    /// The SoC is in the SP3 socket (one flash half per SoC).
    pub const fn sp3(self) -> bool {
        matches!(self, Self::Naples | Self::Rome | Self::Milan)
    }

    /// The SoC is made of the RS I/O die.
    pub const fn rs(self) -> bool {
        matches!(self, Self::Genoa | Self::Siena)
    }
}

/// A board's facts. Host planning and the board's stage read the same
/// values; the flow is the board's `main`.
pub trait Board: 'static {
    /// Shown in the log.
    const NAME: &'static str;
    /// The SoCs the board carries: one image serves all of them.
    const SOCS: &'static [SocId];
    /// Logical CPU ceiling over every SoC the board carries (all sockets):
    /// sizes the AP stacks and the SMM image.
    const MAX_CPUS: u16;
    /// A discrete TPM may be fitted (FIFO interface at `0xFED4_0000`, on
    /// LPC or eSPI): probe it, measure the boot (`fstart-tcg`) and describe
    /// it and its event log in ACPI. Off leaves a TPM untouched and hidden.
    const DISCRETE_TPM: bool = false;
    /// BMC sleep-state notification through SMM (ACPI and the handler).
    const BMC_SX_NOTIFY: Option<crate::smm::SleepNotify> = None;
    /// The option sets the board offers, in the order its boot resolves
    /// them after the console level: on SP3 its policy knobs
    /// ([`crate::sp3::board::PolicyOptions`]), then its own sets, each the
    /// `fstart_options::OptionSet` its stage `main` resolves. The build
    /// lists their options (`fbuild options`); nothing else describes
    /// them. A SoC without a settings store lists none.
    const OPTION_SETS: &'static [&'static dyn fstart_options::Describe] = &[];
    /// Machine checks: observed and counted in SMM, then left to the OS,
    /// or the OS's alone ([`crate::smm::ras`]). The board's SMM image
    /// binds the handler with it and its BMC hook.
    const RAS: crate::smm::ras::Policy = crate::smm::ras::Policy::OS_FIRST;
    /// The board's BMC for RAS events: sends one. The boot reports the
    /// last boot's errors with it (beside the BERT); the board's SMM image
    /// binds its handler with the same function.
    const BMC_EVENT: Option<BmcEvent> = None;
    /// RO/A/B firmware update policy (ESRT/FMP GUID, capsule PKCS#7 trust
    /// certificates, lowest supported version, and optional test fault
    /// injection). `None` means the board does not support `--option ab=true`.
    const AB_UPDATE: Option<AbUpdateConfig> = None;
    /// Whether this board/variant enables RO/A/B updates by default when
    /// `--option ab` is not explicitly specified.
    const AB_DEFAULT: bool = false;
}

/// Board-level RO/A/B firmware update configuration ([`Board::AB_UPDATE`]).
#[derive(Debug, Clone, Copy)]
pub struct AbUpdateConfig {
    /// Stable physical-board firmware family identifier hashed into signed
    /// `Root.image_family` and RO `TrustBlock.image_family` so `--option ab=true`
    /// builds and `-ab` variants share one update family.
    pub image_family: &'static str,
    /// UEFI ESRT / FMP firmware class GUID.
    pub firmware_guid: [u8; 16],
    /// Minimum firmware version advertised in ESRT (`lowest_supported_version`).
    pub lowest_supported_version: u32,
    /// Trusted X.509 DER certificates for UEFI FMP PKCS#7 capsule authentication.
    pub capsule_trust_certs: &'static [&'static [u8]],
    /// Fail closed in RO if a discrete FIFO TPM 2.0 is not responding.
    /// Set `true` for platforms/deployments with a mandatory fitted TPM; leave
    /// `false` for optional TPM headers (RO still halts if `FloorPending` is
    /// recorded without a live TPM, preventing silent floor-advance skips).
    pub require_tpm_floor: bool,
    /// Fault-injection hook for automated rollback testing: when true and a
    /// trial boot is active, reset before confirming the trial so RO debits
    /// attempts and falls back to the previous slot.
    pub fail_trial_before_confirm: bool,
}

/// A board's BMC report of a RAS event ([`Board::BMC_EVENT`]).
pub type BmcEvent =
    fn(&crate::smm::ras::Event<'_>, fstart_driver_amd::ccx::tsc::Tsc) -> crate::smm::ras::Delivery;

/// What the flow's services read of the board ([`Board`]).
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    pub name: &'static str,
    pub max_cpus: u16,
    pub discrete_tpm: bool,
    pub bmc_sx_notify: Option<crate::smm::SleepNotify>,
    pub ras: crate::smm::ras::Policy,
    pub bmc_event: Option<BmcEvent>,
    pub ab_update: Option<AbUpdateConfig>,
}

impl Facts {
    pub const fn of<B: Board>() -> Self {
        Self {
            name: B::NAME,
            max_cpus: B::MAX_CPUS,
            discrete_tpm: B::DISCRETE_TPM,
            bmc_sx_notify: B::BMC_SX_NOTIFY,
            ras: B::RAS,
            bmc_event: B::BMC_EVENT,
            ab_update: B::AB_UPDATE,
        }
    }
}
