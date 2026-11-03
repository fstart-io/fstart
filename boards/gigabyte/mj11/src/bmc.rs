// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the vendor firmware tells the AST2500 BMC over IPMI KCS at the end
//! of POST.
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::fch::{
    Bus, Fch,
    early::{self, PowerFailure},
};
use fstart_driver_ipmi::kcs::{self, Kcs};
use fstart_driver_ipmi::{RestorePolicy, SelRecord};
use zerocopy::little_endian::{U16, U32};

/// Gigabyte OEM NetFn; the request starts with their IANA 0x003c0a.
const NETFN_GIGABYTE: u8 = 0x2e;
const SET_ERP: u8 = 0x20;
/// ErP off, the Setup default.
const ERP_OFF: [u8; 5] = [0x0a, 0x3c, 0x00, 0x0d, 0x1c];

pub fn post_done(fch: &Fch<impl Bus>, clock: Tsc) {
    // SAFETY: the KCS ports the board decodes in early_init; only used here.
    let mut kcs = unsafe { Kcs::new(kcs::DEFAULT_PORT, kcs::DEFAULT_SPACING, clock) };

    // GbtSystemBootEvent: SEL "System Boot Initiated" (system event sensor).
    let boot = SelRecord {
        record_id: U16::new(0),
        record_type: 2,
        timestamp: U32::new(0),
        generator_id: U16::new(1),
        event_message_revision: 4,
        sensor_type: 0x12,
        sensor_number: 0,
        event_dir_type: 0x6f,
        event_data: [0x01, 0xff, 0xff],
    };
    if kcs.add_sel(&boot).is_err() {
        fstart_log::warn!("Adding the boot SEL entry failed");
    }

    // GbtACLossIPMI: the BMC's power restore policy replaces the AC-loss
    // setting.
    match kcs.chassis_status() {
        Ok(status) => {
            let policy = status.restore_policy();
            let state = match policy {
                RestorePolicy::Off => Some(PowerFailure::Off),
                RestorePolicy::Previous => Some(PowerFailure::Previous),
                RestorePolicy::On => Some(PowerFailure::On),
                RestorePolicy::Unknown => None,
            };
            if let Some(state) = state {
                early::set_power_failure_state(fch, state);
                fstart_log::info!("BMC power restore policy {}", policy as u8);
            }
        }
        Err(_) => fstart_log::warn!("Get Chassis Status failed"),
    }

    let mut buffer = [0; 8];
    if kcs
        .request(NETFN_GIGABYTE, SET_ERP, &ERP_OFF, &mut buffer)
        .is_err()
    {
        fstart_log::warn!("BMC ErP command failed");
    }
}
