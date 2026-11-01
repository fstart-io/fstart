// SPDX-License-Identifier: MIT OR Apache-2.0
//! MJ11 SMM: the BMC's diagnostic interrupt button on GEVENT11 (vendor
//! GbtNmiBtn, without its POST code and SEL logging), and the sleep states
//! the DSDT's `_PTS`/`_WAK` report to the BMC (AMI BmcAcpi's SW SMIs
//! 0xC0 + Sx, [`fstart_driver_ipmi::SleepNotify`]).
use fstart_driver_amd::{
    fch::{
        smi::{FchSmmConfig, PendingSmi},
        taishan::smm::{self, SmmSmn},
    },
    smn::SmnReg,
};
use fstart_platform_amd::smm::SmmContext;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

use crate::{BMC_SLEEP_NOTIFY, NMI_BUTTON_GEVENT};

register_bitfields![u32, DiagnosticControl [BIT17 OFFSET(17) NUMBITS(1) []]];
/// The vendor's diagnostic interrupt registers; their meaning isn't known.
const DIAGNOSTIC_CONTROL: SmnReg<DiagnosticControl::Register> = SmnReg::new(0x13b2_015c);
const DIAGNOSTIC_TRIGGER: SmnReg = SmnReg::new(0x13b1_0108);

/// The vendor's SMN sequence that raises the CPU diagnostic interrupt.
fn diagnostic_interrupt() {
    // SAFETY: called from the SMI handler, which owns the rendezvous.
    let smn = unsafe { SmmSmn::new() };
    smn.reg(DIAGNOSTIC_CONTROL)
        .modify(DiagnosticControl::BIT17::SET);
    let trigger = smn.reg(DIAGNOSTIC_TRIGGER);
    trigger.get();
    trigger.set(1);
}

/// Report SW SMI 0xC0 + Sx to the BMC.
fn report_sleep_state(command: u8) {
    if BMC_SLEEP_NOTIFY.states(command).is_none() {
        return;
    }
    // SAFETY: ring 0 in SMM; the P-state MSRs.
    let msr = unsafe { fstart_arch::x86::cpu::msr::NativeMsr::new() };
    let Some(mhz) =
        fstart_arch::x86::cpu::amd::msr::tsc_mhz(&msr).and_then(core::num::NonZeroU32::new)
    else {
        return;
    };
    // SAFETY: the board's KCS ports; SMM owns the CPUs.
    unsafe { BMC_SLEEP_NOTIFY.report(command, fstart_driver_amd::ccx::tsc::Tsc::new(mhz)) };
}

/// The FCH's SMI sources plus the BMC's diagnostic interrupt button and
/// the sleep states the DSDT's `_PTS`/`_WAK` report to the BMC.
fn smi_handler(_ctx: &mut SmmContext<'_>, config: &FchSmmConfig) {
    // SAFETY: bound as the SMI handler, so it owns the rendezvous.
    let pending = unsafe { PendingSmi::take(config) };
    if pending.gevents & 1 << NMI_BUTTON_GEVENT != 0 {
        diagnostic_interrupt();
    }
    if let Some(command) = pending.apm {
        report_sleep_state(command);
    }
    if let Some(slp_typ) = pending.sleep {
        smm::sleep(pending, slp_typ);
    }
    pending.finish();
}

fstart_platform_amd::smm::smm_bin!(handler: smi_handler);
