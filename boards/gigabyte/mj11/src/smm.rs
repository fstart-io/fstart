// SPDX-License-Identifier: MIT OR Apache-2.0
//! MJ11 SMM: the BMC's diagnostic interrupt button on GEVENT11 (vendor
//! GbtNmiBtn, without its POST code and SEL logging).
use fstart_driver_amd::{
    fam17h::registers::Smn,
    zeppelin::fch::smm::{Handler, SmmBoard, SmmSmn},
};

use crate::{Board, NMI_BUTTON_GEVENT};

pub struct Mj11Smm;
impl SmmBoard for Mj11Smm {
    #[inline(always)]
    unsafe fn gevent(status: u32) {
        if status & 1 << NMI_BUTTON_GEVENT == 0 {
            return;
        }
        // The vendor's SMN sequence that raises the CPU diagnostic interrupt.
        // SAFETY: called by the SMM handler, which owns the rendezvous.
        let mut smn = unsafe { SmmSmn::new() };
        smn.update(0x13b2_015c, 0, 1 << 17);
        smn.read(0x13b1_0108);
        smn.write(0x13b1_0108, 1);
    }
}

fstart_platform_amd::smm::smm_bin!(Board, Handler<Mj11Smm>);
