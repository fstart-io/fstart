// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin PSP BARs.
//!
//! Every die has a PSP; the platform maps its 1 MiB register window at the
//! start of the die's non-PCI MMIO pool ([`super::NONPCI_PSP`]) with
//! [`crate::fam17h::iohc::map_psp`]. The C2P mailbox is the shared Family
//! 17h Gen2 one ([`crate::fam17h::psp`]); the native flow only uses die 0's,
//! for the SMM info after MP init, [`Mailbox::lock_df_registers`] at
//! ready-to-boot and BootDone before the payload.
use super::{Die, MAX_DIES};
#[cfg(target_arch = "x86_64")]
pub use crate::fam17h::psp::Native;
pub use crate::fam17h::psp::{Buffer, Error as MailboxError, Mailbox};
use crate::fam17h::iohc::PSP_BAR;
use fstart_pci::ConfigRegionAccess;

/// The PSP MMIO bases of all dies, 0 where the BAR isn't enabled, for
/// [`super::ccx::CpuPolicy::psp_bars`]. The APs can't use SMN in parallel,
/// so the BSP reads them before MP init.
pub fn bars<P: ConfigRegionAccess + Copy>(dies: &[Die<P>]) -> [u32; MAX_DIES] {
    let mut bars = [0; MAX_DIES];
    bars.iter_mut().zip(dies).for_each(|(slot, die)| {
        *slot = PSP_BAR.base_below_4g(&die.smn()).unwrap_or(0)
    });
    bars
}

#[cfg(test)]
mod tests {
    use super::{super::test_pci::Pci, *};
    use crate::fam17h::iohc::map_psp;

    #[test]
    fn maps_each_die_enabled_and_locked_and_reads_back_bases() {
        let pci = Pci::new(&[0, 0x40], |_| [0; 6]);
        let dies = pci.dies();
        // Die 1 is already right: no rewrite of its high half.
        pci.set_smn(1, PSP_BAR.low, 0xd010_0101);
        pci.set_smn(1, PSP_BAR.high, 0xdead);
        assert_eq!(map_psp(&dies[0].smn(), 0xe780_0000), Ok(()));
        assert_eq!(map_psp(&dies[1].smn(), 0xd010_0000), Ok(()));
        assert_eq!(pci.smn(0, PSP_BAR.low), 0xe780_0101);
        assert_eq!(pci.smn(1, PSP_BAR.high), 0xdead);
        assert_eq!(bars(&dies), [0xe780_0000, 0xd010_0000]);
        pci.set_smn(1, PSP_BAR.low, 0xd010_0100);
        assert_eq!(bars(&dies)[1], 0);
    }
}
