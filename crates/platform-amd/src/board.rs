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

    /// The board's ACPI contributions (`\_SB`, `\_SB.PCI0.LPCB`, extra tables),
    /// shared by firmware table emission and the host ACPI baseline check.
    fn acpi_board() -> crate::acpi::Board;

    /// Optional SoC-side `\_SB` AML derived from a board's static silicon
    /// configuration (for example Picasso's FCH UART/I2C/GPIO inventory) for
    /// host ACPI baseline checks.
    fn acpi_soc_sb_aml(_soc: SocId) -> Result<alloc::vec::Vec<u8>, alloc::string::String> {
        Ok(alloc::vec::Vec::new())
    }
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
    pub acpi_board: fn() -> crate::acpi::Board,
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
            acpi_board: B::acpi_board,
        }
    }
}
