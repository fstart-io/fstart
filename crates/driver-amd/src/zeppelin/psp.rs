// SPDX-License-Identifier: MIT OR Apache-2.0
//! Zeppelin PSP BARs.
//!
//! Every die has a PSP; its 1 MiB register window is mapped at the start of
//! the die's non-PCI MMIO pool. The C2P mailbox is the shared Family 17h
//! Gen2 one ([`crate::fam17h::psp`]); the native flow only uses die 0's, for
//! [`Mailbox::lock_df_registers`] at ready-to-boot.
use super::{MAX_DIES, topology::Die, topology::NONPCI_PSP};
#[cfg(target_arch = "x86_64")]
pub use crate::fam17h::psp::Native;
pub use crate::fam17h::psp::{Buffer, Error as MailboxError, Mailbox};
use crate::fam17h::registers::Smn;
use fstart_pci::ConfigRegionAccess;
use tock_registers::{LocalRegisterCopy, register_bitfields};

/// IOHC::PSP_BASE_ADDR_LO/HI (SMN, per die).
const BAR_LOW: u32 = 0x13b1_02e0;
const BAR_HIGH: u32 = 0x13b1_02e4;

register_bitfields![u32,
    Bar [
        ENABLE OFFSET(0) NUMBITS(1) [],
        LOCK OFFSET(8) NUMBITS(1) [],
        /// Address bits [31:20].
        BASE OFFSET(20) NUMBITS(12) []
    ]
];

/// Map the PSP register window of `die` at `nonpci_base` + [`NONPCI_PSP`],
/// enabled and locked. The vendor firmware maps die 0's in PEI and the
/// other dies' in DXE, the same way (|0x101). A locked BAR gets reported as
/// a fixed resource by the root complex.
pub fn init_bar<P: ConfigRegionAccess + Copy>(die: &Die<P>, nonpci_base: u32) {
    let mut smn = die.smn();
    let bar = LocalRegisterCopy::<u32, Bar::Register>::new(smn.read(BAR_LOW));
    let want = Bar::BASE.val((nonpci_base + NONPCI_PSP) >> 20) + Bar::LOCK::SET + Bar::ENABLE::SET;
    if want.matches_all(bar.get()) {
        return;
    }
    if bar.is_set(Bar::LOCK) {
        fstart_log::error!(
            "PSP BAR of die {} is locked at {}",
            die.index(),
            fstart_log::Hex(u64::from(bar.get()))
        );
    }
    smn.write(BAR_HIGH, 0);
    smn.write(BAR_LOW, want.value);
}

/// The PSP MMIO base of `die`, 0 if its BAR isn't enabled.
pub fn bar<P: ConfigRegionAccess + Copy>(die: &Die<P>) -> u32 {
    let bar = LocalRegisterCopy::<u32, Bar::Register>::new(die.smn().read(BAR_LOW));
    if bar.is_set(Bar::ENABLE) {
        bar.read(Bar::BASE) << 20
    } else {
        0
    }
}

/// The PSP MMIO bases of all dies, for [`super::ccx::CpuPolicy::psp_bars`].
/// The APs can't use SMN in parallel, so the BSP reads them before MP init.
pub fn bars<P: ConfigRegionAccess + Copy>(dies: &[Die<P>]) -> [u32; MAX_DIES] {
    let mut bars = [0; MAX_DIES];
    bars.iter_mut()
        .zip(dies)
        .for_each(|(slot, die)| *slot = bar(die));
    bars
}

#[cfg(test)]
mod tests {
    use super::{super::test_pci::Pci, *};

    #[test]
    fn maps_each_die_enabled_and_locked_and_reads_back_bases() {
        let pci = Pci::new(&[0, 0x40], |_| [0; 6]);
        let dies = pci.dies();
        // Die 1 is already right: no rewrite of its high half.
        pci.set_smn(1, BAR_LOW, 0xd010_0101);
        pci.set_smn(1, BAR_HIGH, 0xdead);
        init_bar(&dies[0], 0xe780_0000);
        init_bar(&dies[1], 0xd010_0000);
        assert_eq!(pci.smn(0, BAR_LOW), 0xe780_0101);
        assert_eq!(pci.smn(1, BAR_HIGH), 0xdead);
        assert_eq!(bars(&dies), [0xe780_0000, 0xd010_0000]);
        pci.set_smn(1, BAR_LOW, 0xd010_0100);
        assert_eq!(bar(&dies[1]), 0);
    }
}
