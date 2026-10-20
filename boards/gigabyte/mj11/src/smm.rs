// SPDX-License-Identifier: MIT OR Apache-2.0
//! MJ11 SMM: the BMC's diagnostic interrupt button on GEVENT11 (vendor
//! GbtNmiBtn, without its POST code and SEL logging).
use fstart_driver_amd::{
    fam17h::registers::SmnReg,
    zeppelin::fch::smm::{Handler, SmmBoard, SmmSmn},
};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

use crate::{Board, NMI_BUTTON_GEVENT};

register_bitfields![u32, DiagnosticControl [BIT17 OFFSET(17) NUMBITS(1) []]];
/// The vendor's diagnostic interrupt registers; their meaning isn't known.
const DIAGNOSTIC_CONTROL: SmnReg<DiagnosticControl::Register> = SmnReg::new(0x13b2_015c);
const DIAGNOSTIC_TRIGGER: SmnReg = SmnReg::new(0x13b1_0108);

pub struct Mj11Smm;
impl SmmBoard for Mj11Smm {
    #[inline(always)]
    unsafe fn gevent(status: u32) {
        if status & 1 << NMI_BUTTON_GEVENT == 0 {
            return;
        }
        // The vendor's SMN sequence that raises the CPU diagnostic interrupt.
        // SAFETY: called by the SMM handler, which owns the rendezvous.
        let smn = unsafe { SmmSmn::new() };
        smn.reg(DIAGNOSTIC_CONTROL)
            .modify(DiagnosticControl::BIT17::SET);
        let trigger = smn.reg(DIAGNOSTIC_TRIGGER);
        trigger.get();
        trigger.set(1);
    }
}

fstart_platform_amd::smm::smm_bin!(Board, Handler<Mj11Smm>);
