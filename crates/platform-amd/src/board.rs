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
        }
    }

    /// The SoC is in the SP3 socket (one flash half per SoC).
    pub const fn sp3(self) -> bool {
        matches!(self, Self::Naples | Self::Rome | Self::Milan)
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
    const BMC_SX_NOTIFY: Option<crate::sp3::smm::SleepNotify> = None;
    /// Workspace-relative vendor ROM an SP3 image is composed into when the
    /// build names none: the 32 MiB dual image, one 16 MiB half, or a full
    /// readout with [`Board::FLASH_MIRRORS`].
    const VENDOR_IMAGE: &'static str = "";
    /// Flash offsets past the 16 MiB boot bank where the board's flash
    /// export mirrors it ([`Board::VENDOR_IMAGE`] may then be the full
    /// readout, and the build is a full flash image with the bank at 0 and
    /// at every mirror).
    const FLASH_MIRRORS: &'static [u32] = &[];
}

/// What the flow's services read of the board ([`Board`]).
#[derive(Debug, Clone, Copy)]
pub struct Facts {
    pub name: &'static str,
    pub max_cpus: u16,
    pub discrete_tpm: bool,
    pub bmc_sx_notify: Option<crate::sp3::smm::SleepNotify>,
}

impl Facts {
    pub const fn of<B: Board>() -> Self {
        Self {
            name: B::NAME,
            max_cpus: B::MAX_CPUS,
            discrete_tpm: B::DISCRETE_TPM,
            bmc_sx_notify: B::BMC_SX_NOTIFY,
        }
    }
}
