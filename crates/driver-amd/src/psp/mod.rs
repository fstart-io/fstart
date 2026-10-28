// SPDX-License-Identifier: MIT OR Apache-2.0
//! The PSP (MP0) interfaces the x86 side uses: the C2P (CPU to PSP)
//! mailbox ([`mailbox`]) with the per-generation precheck and status rules
//! over an MMIO or SMN transport, the typed command buffers ([`command`]:
//! SmmInfo, BootDone, LockDFReg, ...) and the SP3 RAS commands ([`ras`]).
//! The flash image format the PSP boots from is `fstart_amdfw_format`.
//!
//! None of the commands keeps the PSP from booting (PSP_APOB §3.3): every
//! sender in AGESA is gated by a PCD and the boot does not depend on them.
//! Every die has a PSP; the native flows only use die 0's mailbox.

pub mod command;
pub mod mailbox;
#[cfg(feature = "sp3")]
pub mod ras;

pub use command::{C2pBuffer, Command};
pub use mailbox::{C2pTransport, Mailbox, MmioTransport, PspError, Session, SmnTransport, Storage};

/// The PSP generation, which the mailbox rules and the commands differ by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    /// The ZP die: Zeppelin and Naples.
    Zp,
    /// Raven and Picasso.
    Rv,
    /// Rome.
    Ssp,
    /// Milan.
    Gn,
}

/// The PSP MMIO bases of all dies, 0 where the BAR isn't enabled, for the
/// per-CPU init (Zeppelin's `CpuPolicy::psp_bars`).
/// The APs can't use SMN in parallel, so the BSP reads them before MP init.
#[cfg(any(feature = "picasso", feature = "zeppelin"))]
pub fn bars<S: crate::soc::Soc, P: fstart_pci::ConfigRegionAccess + Copy, const N: usize>(
    dies: &[crate::df::Die<S, P>],
) -> [u32; N] {
    let mut bars = [0; N];
    bars.iter_mut().zip(dies).for_each(|(slot, die)| {
        *slot = crate::nbio::iohc::PSP_BAR
            .base_below_4g(&die.smn())
            .unwrap_or(0)
    });
    bars
}
