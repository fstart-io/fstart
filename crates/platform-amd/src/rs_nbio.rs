// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rs IOD NBIO base configuration, shared by Genoa and Siena (one IOD; Siena
//! fuses IOS1 off). NFX AmdNbioPei AmdNbioBaseConfiguration and its wrapper
//! callback; openSIL NbioBaseInitGenoa.c. Mechanisms live in
//! `fstart_driver_amd::nbio::{iohc, rs}`; this is their platform sequence.
use fstart_driver_amd::{
    df::v4::{PRIMARY_IOS, Root},
    nbio::{iohc, rs},
    smn::Smn,
};
use fstart_pci::ecam::GlobalEcam;
use tock_registers::interfaces::{Readable, Writeable};

/// Run before any MPIO request, link or AP code: two loops over every live
/// root, as the vendor does. First (ffa31e96..ffa31f7b) each root's own bus
/// and LAT_MODE plus its southbridge location; only the FCH's IOHC may claim
/// the southbridge port, otherwise unclaimed config requests on the other
/// roots never complete (Linux's ff:00.0 probe, absent root-bus devices).
/// Second (ffa3207e..ffa322cb) the vendor SMN tables (P2P, CRS retry,
/// arbitration, IOAPIC routing, SDP, NBIF enables/straps) and NBIF upstream
/// identities, under `policy`.
pub fn initialize(roots: &[Root], policy: rs::Policy) -> Result<(), ()> {
    // SAFETY: BSP-exclusive SMN index/data on validated live roots.
    let smn = unsafe { Smn::new(GlobalEcam) };
    for root in roots {
        let ios = root.ios.physical_index;
        let bank = u32::from(ios) << 20;
        let reg = iohc::NB_BUS_NUM_CNTL.offset(bank);
        let before = smn.reg(reg).get();
        iohc::set_bus_number_at(&smn, bank, root.route.base).map_err(|actual| {
            fstart_log::error!("rs: IOHC IOS={} bus identity readback {:#x}", ios, actual);
        })?;
        fstart_log::info!(
            "rs: IOHC IOS={} bus-identity before={:#x} after={:#x}",
            ios,
            before,
            smn.reg(reg).get()
        );
        let fch = ios == PRIMARY_IOS;
        let sb_before = smn.reg(iohc::SB_LOCATION.offset(bank)).get();
        let sb = iohc::set_southbridge_location_at(&smn, bank, fch).map_err(|actual| {
            fstart_log::error!("rs: IOHC IOS={} SB_LOCATION rejected {:#x}", ios, actual);
        })?;
        fstart_log::info!(
            "rs: IOHC IOS={} FCH={} SB_LOCATION before={:#x} after={:#x}",
            ios,
            fch,
            sb_before,
            sb
        );
    }
    for root in roots {
        let ios = root.ios.physical_index;
        rs::base_configuration(&smn, ios, root.route.base, policy);
        let bank = u32::from(ios) << 20;
        fstart_log::info!(
            "rs: NBIO IOS={} base tables applied properties={:#x} CRS={:#x} feature={:#x}",
            ios,
            rs::properties(ios, policy),
            smn.reg(iohc::CONFIG_RETRY.offset(bank)).get(),
            smn.reg(iohc::IOHC_FEATURE_CNTL.offset(bank)).get()
        );
    }
    Ok(())
}

/// After MPIO training. NFX wrapper callback ffa375c0, PCD B4 default true:
/// keep normal completion-timeout semantics but do not escalate the report
/// to a fatal platform error. B5 (timeout-to-success) defaults false and is
/// left untouched.
pub fn after_training(roots: &[Root]) -> Result<(), ()> {
    // SAFETY: BSP-exclusive SMN index/data; MPIO has completed.
    let smn = unsafe { Smn::new(GlobalEcam) };
    for root in roots {
        let ios = root.ios.physical_index;
        for wrapper in 0..3 {
            let Some(control) = rs::completion_control(ios, wrapper) else {
                continue;
            };
            let reg = smn.reg(control);
            let mut value = reg.extract();
            let before = value.get();
            if before == u32::MAX {
                return Err(());
            }
            value.modify(rs::CompletionControl::IGNORE_TIMEOUT_ERROR::SET);
            reg.set(value.get());
            let actual = reg.get();
            fstart_log::info!(
                "rs: IOS={} wrapper={} completion[{:#x}] before={:#x} after={:#x}",
                ios,
                wrapper,
                control.address(),
                before,
                actual
            );
            if actual != value.get() {
                return Err(());
            }
        }
    }
    Ok(())
}
