// SPDX-License-Identifier: MIT OR Apache-2.0
//! MJ11 SMM: the BMC's diagnostic interrupt button on GEVENT11 (vendor
//! GbtNmiBtn, without its POST code and SEL logging). The sleep states the
//! DSDT's `_PTS`/`_WAK` report to the BMC (AMI BmcAcpi's SW SMIs 0xC0 +
//! Sx) are the platform handler's, from the board's `BMC_SX_NOTIFY`.
use fstart_driver_amd::{
    fch::{FchHw, smi::Pending, smi::source},
    smn::{Smn, SmnReg},
};
use fstart_platform_amd::smm::{SmmConfig, SmmContext, SmmFch};
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

use crate::NMI_BUTTON_GEVENT;

register_bitfields![u32, DiagnosticControl [BIT17 OFFSET(17) NUMBITS(1) []]];
/// The vendor's diagnostic interrupt registers; their meaning isn't known.
const DIAGNOSTIC_CONTROL: SmnReg<DiagnosticControl::Register> = SmnReg::new(0x13b2_015c);
const DIAGNOSTIC_TRIGGER: SmnReg = SmnReg::new(0x13b1_0108);

/// The vendor's SMN sequence that raises the CPU diagnostic interrupt.
fn diagnostic_interrupt(fch: &SmmFch) {
    let smn: Smn<_> = fch.smn();
    smn.reg(DIAGNOSTIC_CONTROL)
        .modify(DiagnosticControl::BIT17::SET);
    let trigger = smn.reg(DIAGNOSTIC_TRIGGER);
    trigger.get();
    trigger.set(1);
}

/// The FCH's SMI sources plus the BMC's diagnostic interrupt button.
fn smi_handler(ctx: &mut SmmContext<'_>, config: &SmmConfig) {
    fstart_platform_amd::smm::handle_with(ctx, config, |fch, pending: &Pending| {
        if pending.has(source::gevent(NMI_BUTTON_GEVENT)) {
            diagnostic_interrupt(fch);
        }
    });
}

fstart_platform_amd::smm::smm_bin!(handler: smi_handler);
