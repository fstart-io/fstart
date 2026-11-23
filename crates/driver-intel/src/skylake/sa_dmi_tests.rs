// SPDX-License-Identifier: MIT OR Apache-2.0
extern crate std;
use super::*;
use core::cell::UnsafeCell;
use fstart_pci::{PciAddress, ecam_host::PciEcam};
use std::boxed::Box;

#[repr(C, align(4096))]
pub(crate) struct Image(pub(crate) [UnsafeCell<u32>; 1024]);
impl Image {
    pub(crate) fn new() -> Box<Self> {
        Box::new(Self(core::array::from_fn(|_| UnsafeCell::new(0))))
    }
    pub(crate) fn dmi(&self) -> Dmi<'_> {
        unsafe { Dmi::at(self as *const Self as usize, early_sa::DMI_BASE).unwrap() }
    }
}
pub(crate) fn cpu() -> Cpu {
    Cpu::from_signature(0x506e3).unwrap()
}
pub(crate) fn policy() -> Policy {
    Policy {
        pch: Kind::H,
        ecam: early_sa::EcamConfig {
            base: 0xe000_0000,
            buses: 256,
        },
        hsio_gen1_only: false,
        vc1: true,
        poll_budget_ms: 3,
    }
}
pub(crate) fn root(pci: &PciEcam, opi: bool) -> EcamDevice {
    let sa = pci.device(PciAddress::new(0, 0, 0, 0)).unwrap();
    sa.write32(0, if opi { 0x1904_8086 } else { 0x191f_8086 });
    sa.write32(8, 0x0600_0001);
    sa.write8(0xe, 0);
    sa.write32(0x60, 0xe000_0001);
    sa.write32(0x64, 0);
    sa.write32(0x48, MCHBAR_BASE as u32 | 1);
    sa.write32(0x4c, 0);
    sa.write32(0x68, early_sa::DMI_BASE | 1);
    sa.write32(0x6c, 0);
    sa
}
