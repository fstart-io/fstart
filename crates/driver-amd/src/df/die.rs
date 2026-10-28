// SPDX-License-Identifier: MIT OR Apache-2.0
//! The dies of a SoC as its Data Fabric reports them, and their register
//! transports.
//!
//! Every die has its own Data Fabric PCI device on bus 0 (0x18 + die), its
//! own root bus and its own SMU. The SMN addresses are the same on every die
//! and are reached through D0F0 0xb8/0xbc of that die's root bus. A DF
//! "broadcast" access in AGESA terms is a direct access to the die's own
//! register ([`Df::reg`]). Single-die SoCs (Raven/Picasso) have die 0 on
//! bus 0. The threads of the dies are [`crate::smu::threads`]'.
use super::{
    Df,
    regs::{self, CFG_ADDR_CTL, COMPONENT_CNT},
};
use crate::{ccx::tsc::DelayNs, smn::Smn, smu, soc::Soc};
use core::marker::PhantomData;
use fstart_pci::ConfigRegionAccess;
use tock_registers::interfaces::Readable;

/// Number of dies from DF1x204 (live topology).
pub fn die_count(pci: &impl ConfigRegionAccess) -> u8 {
    // SAFETY: a read of node 0's own register.
    let df = unsafe { Df::new(pci) };
    df.reg(regs::COMPONENT_CNT).read(COMPONENT_CNT::PIE) as u8
}

/// One die of SoC `S`: its index, root bus and register transports.
pub struct Die<S, P> {
    pci: P,
    index: u8,
    bus: u8,
    soc: PhantomData<S>,
}
impl<S, P: Copy> Clone for Die<S, P> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S, P: Copy> Copy for Die<S, P> {}
impl<S: Soc, P: ConfigRegionAccess + Copy> Die<S, P> {
    /// # Safety
    /// `index` is below [`die_count`]. The caller serializes all SMN, FICAA
    /// and SMU mailbox accesses of this die (BSP only during silicon init).
    pub unsafe fn new(pci: P, index: u8) -> Self {
        let df = unsafe { Df::for_node(pci, index) };
        let bus = df.reg(regs::CFG_ADDR_CTL).read(CFG_ADDR_CTL::BUS_NUM) as u8;
        Self {
            pci,
            index,
            bus,
            soc: PhantomData,
        }
    }
    pub const fn index(&self) -> u8 {
        self.index
    }
    /// Root bus of the die (0 on die 0).
    pub const fn bus(&self) -> u8 {
        self.bus
    }
    pub fn ioms_fabric_id(&self) -> u8 {
        S::ioms_fabric_id(self.index)
    }
    pub fn pci(&self) -> P {
        self.pci
    }
    pub fn smn(&self) -> Smn<P> {
        // SAFETY: exclusivity is the caller's contract of `Die::new`.
        unsafe { Smn::on_bus(self.pci, self.bus) }
    }
    pub fn df(&self) -> Df<P> {
        // SAFETY: as above.
        unsafe { Df::for_node(self.pci, self.index) }
    }
    /// Send a message to this die's SMU.
    pub fn smu(
        &self,
        message: u32,
        arguments: [u32; smu::ARGUMENT_COUNT],
        delay: &mut impl DelayNs,
    ) -> Result<[u32; smu::ARGUMENT_COUNT], smu::Error> {
        S::SMU.request(&self.smn(), message, arguments, delay)
    }
}

/// All dies of the system, in order: die 0 on bus 0 alone on a single-die
/// SoC, else as many as DF1x204 reports, up to [`Soc::MAX_DIES`].
/// # Safety
/// As [`Die::new`]: the caller serializes the dies' register accesses.
pub unsafe fn dies<S: Soc, P: ConfigRegionAccess + Copy>(
    pci: P,
) -> impl Iterator<Item = Die<S, P>> {
    let count = if S::MAX_DIES > 1 {
        die_count(&pci).min(S::MAX_DIES as u8)
    } else {
        1
    };
    (0..count).map(move |index| {
        if S::MAX_DIES == 1 {
            Die {
                pci,
                index,
                bus: 0,
                soc: PhantomData,
            }
        } else {
            // SAFETY: the caller's contract.
            unsafe { Die::new(pci, index) }
        }
    })
}
