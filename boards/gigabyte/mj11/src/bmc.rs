// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the vendor firmware tells the AST2500 BMC over IPMI KCS at the end
//! of POST.
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::fch::{
    FchHw,
    early::{self, PowerFailure},
};
use fstart_driver_ipmi::kcs::{self, Kcs};
use fstart_driver_ipmi::{ChassisStatus, NETFN_CHASSIS, NETFN_STORAGE, RestorePolicy, SelRecord};
use zerocopy::{
    IntoBytes,
    little_endian::{U16, U32},
};

/// Gigabyte OEM NetFn; the request starts with their IANA 0x003c0a.
const NETFN_GIGABYTE: u8 = 0x2e;
const SET_ERP: u8 = 0x20;
const GET_CHASSIS_STATUS: u8 = 0x01;
const ADD_SEL_ENTRY: u8 = 0x44;
/// ErP off, the Setup default.
const ERP_OFF: [u8; 5] = [0x0a, 0x3c, 0x00, 0x0d, 0x1c];

/// GbtSystemBootEvent: SEL "System Boot Initiated" (system event sensor).
const BOOT_SEL: SelRecord = SelRecord {
    record_id: U16::ZERO,
    record_type: 2,
    timestamp: U32::ZERO,
    generator_id: U16::new(1),
    event_message_revision: 4,
    sensor_type: 0x12,
    sensor_number: 0,
    event_dir_type: 0x6f,
    event_data: [0x01, 0xff, 0xff],
};

/// Result of the three IPMI KCS POST-done commands, ready to be applied to the
/// FCH and logged once the board reaches the post-display handoff point.
pub struct PostDone {
    sel_ok: bool,
    restore_policy: Option<RestorePolicy>,
    erp_ok: bool,
}

/// Run the three IPMI KCS POST-done commands asynchronously so they can overlap
/// with SMU DXIO link training while the BSP waits on the SMU.
///
/// # Safety
/// The KCS ports (`0xca2`/`0xca3`) must be decoded to the BMC and not used
/// concurrently by anything else.
pub async unsafe fn run_post_done(clock: Tsc) -> PostDone {
    // SAFETY: caller guarantees KCS decode and exclusive ownership.
    let mut kcs = unsafe { Kcs::new(kcs::DEFAULT_PORT, kcs::DEFAULT_SPACING, clock) };
    let mut buffer = [0u8; 16];

    let sel_ok = matches!(
        kcs.request_async(NETFN_STORAGE, ADD_SEL_ENTRY, BOOT_SEL.as_bytes(), &mut buffer)
            .await,
        Ok(data) if data.len() >= 2
    );
    let restore_policy = match kcs
        .request_async(NETFN_CHASSIS, GET_CHASSIS_STATUS, &[], &mut buffer)
        .await
    {
        Ok([power_state, ..]) => Some(
            ChassisStatus {
                power_state: *power_state,
            }
            .restore_policy(),
        ),
        _ => None,
    };
    let erp_ok = kcs
        .request_async(NETFN_GIGABYTE, SET_ERP, &ERP_OFF, &mut buffer)
        .await
        .is_ok();

    PostDone {
        sel_ok,
        restore_policy,
        erp_ok,
    }
}

/// Apply the FCH AC-loss setting and log the results of [`run_post_done`].
pub fn apply_post_done(fch: &impl FchHw, done: PostDone) {
    if !done.sel_ok {
        fstart_log::warn!("Adding the boot SEL entry failed");
    }
    match done.restore_policy {
        Some(policy) => {
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
        None => fstart_log::warn!("Get Chassis Status failed"),
    }
    if !done.erp_ok {
        fstart_log::warn!("BMC ErP command failed");
    }
}

pub fn post_done(fch: &impl FchHw, clock: Tsc) {
    // SAFETY: the KCS ports the board decodes in early_init; only used here.
    let done = fstart_core::task::block_on(unsafe { run_post_done(clock) });
    apply_post_done(fch, done);
}
