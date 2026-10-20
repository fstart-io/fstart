// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the vendor firmware tells the AST2500 BMC over IPMI KCS at the end
//! of POST.
use fstart_driver_amd::zeppelin::fch::{Acpimmio, Bus};
use fstart_driver_ipmi::{SelRecord, kcs::Kcs};
use fstart_platform_amd::zeppelin::BoardCtx;
use zerocopy::little_endian::{U16, U32};

/// Gigabyte OEM NetFn; the request starts with their IANA 0x003c0a.
const NETFN_GIGABYTE: u8 = 0x2e;
const SET_ERP: u8 = 0x20;
/// ErP off, the Setup default.
const ERP_OFF: [u8; 5] = [0x0a, 0x3c, 0x00, 0x0d, 0x1c];

/// PMx5B[3:0], the AC-loss policy, per IPMI restore policy: off, previous,
/// on.
const PM_RTC_SHADOW: u32 = 0x5b;
const RESTORE_POLICY: [u8; 3] = [4, 7, 5];

pub fn post_done(ctx: &mut BoardCtx<'_>) {
    // SAFETY: the KCS ports the board decodes in early_init; only used here.
    let mut kcs = unsafe { Kcs::new(crate::acpi::KCS_PORT, 1, ctx.clock) };

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
            if let Some(&value) = RESTORE_POLICY.get(usize::from(policy)) {
                ctx.fch()
                    .update(Acpimmio::DIE0.pm::<u8, ()>(PM_RTC_SHADOW), 0x0f, value);
                fstart_log::info!("BMC power restore policy {}", policy);
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
