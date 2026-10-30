// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMU V9 of the ZP die (AGESA AmdNbioSmuV9; NBIO_SMU §3.8): one SMU
//! per die, the same SMN addresses on every die, of Zeppelin (SP4) and
//! Naples (SP3). The mailbox, the messages, the feature masks, the policy
//! table and the DXE / CCX-done sequences.
//!
//! Zeppelin call order: [`name`] early; [`init`] and [`cac_weights`]
//! before MP init; [`crate::smu::threads::launch`] to release the APs;
//! [`aps_ready`] then [`ccx_done_all`] after every CPU ran
//! [`crate::ccx::zeppelin::cpu_init`].
use crate::smn::SmnReg;
use crate::smu::{Error, Mailbox, SmuMessages, send1};
use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned};
#[cfg(feature = "zeppelin")]
use {
    crate::ccx::tsc::DelayNs,
    crate::ccx::{CAC_WEIGHT_COUNT, CacWeights},
    crate::smu::ARGUMENT_COUNT,
    crate::soc::zeppelin::{Die, MAX_DIES},
    fstart_pci::ConfigRegionAccess,
};

pub const MAILBOX: Mailbox = Mailbox {
    message: SmnReg::new(0x03b1_0528),
    response: SmnReg::new(0x03b1_0564),
    arguments: SmnReg::new(0x03b1_0598),
    wait_before: true,
};
/// No ID range check is known for V9.
pub const ID_LIMIT: u32 = u32::MAX;
const INTERFACE_VERSION: u32 = 7;

/// V9 message IDs (`smu_messages`, generation zp; FCH_SP3 for 0x13;
/// PCIE_DXIO §4 for hotplug).
pub mod msg {
    /// Test / apply ThreadEn, downcore and SMT (arg 0xAA55AA55).
    pub const TEST: u32 = 0x01;
    /// GetSmuVersion: arg 7 in, 7 back.
    pub const GET_VERSION: u32 = 0x03;
    pub const ENABLE_FEATURES: u32 = 0x09;
    pub const SET_DRAM_BUFFER_ADDR_HI: u32 = 0x0D;
    pub const SET_DRAM_BUFFER_ADDR_LO: u32 = 0x0E;
    /// DXIO RPC (PCIe track).
    pub const DXIO: u32 = 0x0F;
    pub const GET_CAC_WEIGHTS: u32 = 0x10;
    /// Last message at Sx entry (FCH_SP3 §Sx).
    pub const SLEEP_ENTRY: u32 = 0x13;
    /// Hotplug enable: argument laid out like V11's 0x18 (PCIE_DXIO §3.10;
    /// the donor's failure strings name 0x3E the poll interval).
    pub const HOTPLUG_ENABLE: u32 = 0x14;
    pub const HOTPLUG_I2C_SWITCH_BASE: u32 = 0x17;
    pub const SET_POLICY_ADDR_HI: u32 = 0x1D;
    pub const SET_POLICY_ADDR_LO: u32 = 0x1E;
    pub const SET_AGM_LOG_ADDR_HI: u32 = 0x1F;
    pub const SET_AGM_LOG_ADDR_LO: u32 = 0x20;
    pub const TRANSFER_POLICY_TABLE: u32 = 0x22;
    /// AcBtc steps 0, 1, 2 (PCD default off).
    pub const AC_BTC: u32 = 0x23;
    pub const DC_BTC: u32 = 0x26;
    pub const GET_NAME_STRING: u32 = 0x28;
    pub const CHECK_TDC_COMPATIBILITY: u32 = 0x29;
    pub const CHECK_EDC_COMPATIBILITY: u32 = 0x2A;
    pub const COMMIT_DRAM_BUFFER: u32 = 0x2B;
    pub const SET_PERFMON_ADDR_HI: u32 = 0x2D;
    pub const SET_PERFMON_ADDR_LO: u32 = 0x2E;
    /// Unnamed, PCD 0x183 (default 0).
    pub const MSG_33: u32 = 0x33;
    /// Unnamed, PCD 0x18 (default 0).
    pub const MSG_35: u32 = 0x35;
    /// Unnamed, sent with 0 after CCX done (NBIO_SMU §2 row 11).
    pub const MSG_38: u32 = 0x38;
    /// Unnamed, sent with 0 after CCX done.
    pub const MSG_39: u32 = 0x39;
    /// Unnamed, per die with the die's root bus.
    pub const DIE_BUS_3B: u32 = 0x3B;
    /// Unnamed, per die with the die's root bus.
    pub const DIE_BUS_3D: u32 = 0x3D;
    /// "EnableHotplugPollInterval", argument PCD32 0x181.
    pub const HOTPLUG_POLL_INTERVAL: u32 = 0x3E;
    /// Hotplug setup step (unnamed, PCIE_DXIO §3.10).
    pub const HOTPLUG_45: u32 = 0x45;
    pub const CCX_DONE: u32 = 0x41;
}

/// V9 feature bits (names from the ZP SMU interface, ../sp4r2 §2).
pub mod features {
    pub const CCLK_CONTROLLER: u32 = 1 << 0;
    pub const FAN_CONTROLLER: u32 = 1 << 1;
    pub const DATA_CALCULATION: u32 = 1 << 2;
    pub const PPT: u32 = 1 << 3;
    pub const TDC: u32 = 1 << 4;
    pub const THERMAL: u32 = 1 << 5;
    pub const FIT: u32 = 1 << 6;
    pub const QOS: u32 = 1 << 7;
    pub const CORE_CSTATES: u32 = 1 << 8;
    pub const PROCHOT: u32 = 1 << 9;
    pub const MCM_DATA_TRANSFER: u32 = 1 << 10;
    pub const DLWM: u32 = 1 << 11;
    pub const PC6: u32 = 1 << 12;
    pub const CSTATE_BOOST: u32 = 1 << 13;
    pub const VOLTAGE_CONTROLLER: u32 = 1 << 14;
    pub const HOT_PLUG: u32 = 1 << 15;
    /// Unnamed.
    pub const BIT17: u32 = 1 << 17;

    /// The SP3 mask the Naples entry stores in PCD 0x91.
    pub const SP3: u32 = 0x2EF3F;
    /// The mask on SP4 and SP4r2 (vendor PCD 0xbb).
    pub const SP4: u32 = 0xe73d;
}

/// The Naples feature mask: SP3 base, QOS from PCD 0x17, minus the bits
/// some revisions (12/13) or the APCB (11) remove; the caller supplies
/// those removals (not decoded per revision, open item).
pub const fn feature_mask(qos: bool, remove: u32) -> u32 {
    (features::SP3 | if qos { features::QOS } else { 0 }) & !remove
}

/// The 0x7C-byte BIOS policy table (AmdNbioSmuV9Dxe, field stores
/// instruction-checked in ../sp4r2 `smu-v9-policy-table.json`; Naples uses
/// the same layout). Zero means "use the fused value".
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct PolicyTable {
    /// MaxSusPwr (TDP).
    pub tdp: U32,
    pub tdc: U32,
    pub edc: U32,
    pub determinism_enable: U32,
    pub determinism_percentage: U32,
    /// Fan table override (AM4 only).
    pub fan: [u8; 11],
    _rsvd_1f: u8,
    pub dldo_psm_margin: u8,
    pub tjmax_override: [u8; 2],
    _rsvd_23: [u8; 2],
    pub core_stretch_thresh_en: u8,
    pub core_stretch_thresh: u8,
    pub l3_stretch_thresh_en: u8,
    pub l3_stretch_thresh: u8,
    pub dldo_bypass: u8,
    pub xi_separation_en: u8,
    pub xi_separation_high: u8,
    pub xi_separation_low: u8,
    _unnamed_2d: u8,
    pub force_vddcr_cpu_vid_en: u8,
    pub force_vddcr_cpu_vid: u8,
    pub vddcr_cpu_voltage_margin: U32,
    pub fcw_slew_frac_l3_fid_total_steps: U16,
    pub fcw_slew_frac_l3_fid_total_steps_en: u8,
    pub force_cclk_frequency_en: u8,
    pub force_cclk_frequency: U32,
    pub use_telemetry_data: u8,
    _unnamed_3d: u8,
    pub xgmi_force_link_width_en: u8,
    pub xgmi_force_link_width: u8,
    pub xgmi_max_link_width_en: u8,
    pub xgmi_max_link_width: u8,
    _rsvd_42: [u8; 2],
    /// Core PSF: freq, temp, sidd, cac, active cores, other die, sigma.
    pub core_psf: [U32; 7],
    /// L3 PSF, same order.
    pub l3_psf: [U32; 7],
}

const _: () = assert!(size_of::<PolicyTable>() == 0x7C);

/// Board/CBS inputs of the V9 policy table; `Default` = donor defaults
/// (everything fused, determinism enabled at 100 %).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    pub tdp: u32,
    pub tdc: u32,
    pub edc: u32,
    /// Determinism slider "performance" (Naples PCD 0x16): 100 %, else 0.
    pub determinism_performance: bool,
    pub xgmi_max_link_width: Option<u8>,
    pub xgmi_force_link_width: Option<u8>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            tdp: 0,
            tdc: 0,
            edc: 0,
            determinism_performance: true,
            xgmi_max_link_width: None,
            xgmi_force_link_width: None,
        }
    }
}

impl PolicyTable {
    pub fn new(p: &Policy) -> Self {
        let mut t = Self::new_zeroed();
        t.tdp = p.tdp.into();
        t.tdc = p.tdc.into();
        t.edc = p.edc.into();
        t.determinism_enable = 1.into();
        t.determinism_percentage = if p.determinism_performance { 100 } else { 0 }.into();
        if let Some(w) = p.xgmi_max_link_width {
            (t.xgmi_max_link_width_en, t.xgmi_max_link_width) = (1, w);
        }
        if let Some(w) = p.xgmi_force_link_width {
            (t.xgmi_force_link_width_en, t.xgmi_force_link_width) = (1, w);
        }
        t
    }
}

/// DRAM pages the SMU needs per die (physical addresses below 4 GiB or
/// above; the SMU DMAs from them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DxeBuffers {
    /// The [`PolicyTable`] copy.
    pub policy_table: u64,
    /// 4 KiB ACPI NVS page for the AGM log.
    pub agm_log: u64,
    /// Perfmon page.
    pub perfmon: u64,
}

fn send_split<M: SmuMessages + ?Sized>(smu: &M, lo: u32, hi: u32, addr: u64) -> Result<(), Error> {
    send1(smu, lo, addr as u32)?;
    send1(smu, hi, (addr >> 32) as u32).map(drop)
}

/// SMU DXE entry per die: version check, policy table, AGM log, perfmon
/// and the two per-die bus messages (NBIO_SMU §2 row 8). `bus` is the die's
/// root bus.
pub fn dxe_init<M: SmuMessages + ?Sized>(
    smu: &M,
    bus: u8,
    buffers: &DxeBuffers,
) -> Result<(), Error> {
    // The vendor code asserts (AGESA event 0xa2411310) and goes on.
    let version = send1(smu, msg::GET_VERSION, INTERFACE_VERSION)?;
    if version != INTERFACE_VERSION {
        fstart_log::error!(
            "SMU: interface version {}, expected {}",
            version,
            INTERFACE_VERSION
        );
    }
    send_split(
        smu,
        msg::SET_POLICY_ADDR_LO,
        msg::SET_POLICY_ADDR_HI,
        buffers.policy_table,
    )?;
    send1(smu, msg::TRANSFER_POLICY_TABLE, 0)?;
    send_split(
        smu,
        msg::SET_AGM_LOG_ADDR_LO,
        msg::SET_AGM_LOG_ADDR_HI,
        buffers.agm_log,
    )?;
    send_split(
        smu,
        msg::SET_PERFMON_ADDR_LO,
        msg::SET_PERFMON_ADDR_HI,
        buffers.perfmon,
    )?;
    send1(smu, msg::DIE_BUS_3B, bus as u32)?;
    send1(smu, msg::DIE_BUS_3D, bus as u32).map(drop)
}

/// CCX-done per die: CcxDone, feature enable, then 0x38/0x39 with 0.
pub fn ccx_done<M: SmuMessages + ?Sized>(smu: &M, features: u32) -> Result<(), Error> {
    [
        (msg::CCX_DONE, 0),
        (msg::ENABLE_FEATURES, features),
        (msg::MSG_38, 0),
        (msg::MSG_39, 0),
    ]
    .iter()
    .try_for_each(|&(id, arg)| send1(smu, id, arg).map(drop))
}

/// After the APs are launched: DcBtc on die 0 (PCD 0x17E, set by the SP3
/// entry), then feature enable 0x100 (CORE_CSTATES) on every die.
pub fn after_ap_launch<M: SmuMessages + ?Sized>(smu: &M, die0: bool) -> Result<(), Error> {
    if die0 {
        send1(smu, msg::DC_BTC, 0)?;
    }
    send1(smu, msg::ENABLE_FEATURES, features::CORE_CSTATES).map(drop)
}

/// A failed SMU interaction of the Zeppelin flow.
#[cfg(feature = "zeppelin")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// A message to a die's SMU didn't complete successfully.
    Message { die: u8, message: u32, error: Error },
    /// A DMA buffer address is unaligned or missing.
    Buffer,
}

/// The SMU of a Zeppelin die, logging the message that failed.
#[cfg(feature = "zeppelin")]
struct DieSmu<'a, P, D> {
    die: &'a Die<P>,
    delay: core::cell::RefCell<&'a mut D>,
    failed: core::cell::Cell<u32>,
}
#[cfg(feature = "zeppelin")]
impl<'a, P: ConfigRegionAccess + Copy, D: DelayNs> DieSmu<'a, P, D> {
    fn new(die: &'a Die<P>, delay: &'a mut D) -> Self {
        Self {
            die,
            delay: core::cell::RefCell::new(delay),
            failed: core::cell::Cell::new(0),
        }
    }
    fn failure(&self, error: Error) -> Failure {
        Failure::Message {
            die: self.die.index(),
            message: self.failed.get(),
            error,
        }
    }
}
#[cfg(feature = "zeppelin")]
impl<P: ConfigRegionAccess + Copy, D: DelayNs> SmuMessages for DieSmu<'_, P, D> {
    fn send(&self, id: u32, args: &mut [u32; ARGUMENT_COUNT]) -> Result<(), Error> {
        let reply = self.die.smu(id, *args, &mut **self.delay.borrow_mut());
        *args = reply.inspect_err(|_| {
            fstart_log::error!(
                "SMU {} message {} failed",
                self.die.index(),
                fstart_log::Hex(u64::from(id))
            );
            self.failed.set(id);
        })?;
        Ok(())
    }
}

/// The CPU name string from die `die`'s SMU (the fallback if it has none),
/// logged.
#[cfg(feature = "zeppelin")]
pub fn name<P: ConfigRegionAccess + Copy>(
    die: &Die<P>,
    delay: &mut impl DelayNs,
) -> crate::ccx::brand::Name {
    let name = crate::ccx::brand::read(&DieSmu::new(die, delay), msg::GET_NAME_STRING);
    fstart_log::info!("CPU: {}", crate::ccx::brand::as_str(&name));
    name
}

/// Physical addresses of the DRAM the SMU firmware reads its policy from
/// and writes its AGM log and performance monitor data to.
#[cfg(feature = "zeppelin")]
#[derive(Debug, Clone, Copy)]
pub struct DmaBuffers {
    /// A [`PolicyTable`], 64-byte aligned.
    pub policy: u64,
    /// One zeroed 4-KiB page per die.
    pub agm_log: [u64; MAX_DIES],
    /// One zeroed 4-KiB page per die.
    pub perfmon: [u64; MAX_DIES],
}
#[cfg(feature = "zeppelin")]
impl DmaBuffers {
    fn valid(&self, dies: usize) -> bool {
        let page = |address: u64| address != 0 && address & 0xfff == 0;
        self.policy != 0
            && self.policy & 63 == 0
            && dies <= MAX_DIES
            && self.agm_log[..dies].iter().all(|&a| page(a))
            && self.perfmon[..dies].iter().all(|&a| page(a))
    }
}

/// [`dxe_init`] on every die (AmdNbioSmuV9Dxe entry). The SMU gets its AGM
/// log and perfmon buffers before it enables features.
///
/// # Safety
/// `dies` are all dies in order. `buffers.policy` holds the
/// [`PolicyTable`] and the AGM log and perfmon pages are zeroed. All of
/// them are DRAM the platform reserves from the OS until reset: the SMU
/// keeps using them.
#[cfg(feature = "zeppelin")]
pub unsafe fn init<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    buffers: &DmaBuffers,
    delay: &mut impl DelayNs,
) -> Result<(), Failure> {
    if !buffers.valid(dies.len()) {
        return Err(Failure::Buffer);
    }
    dies.iter().try_for_each(|die| {
        let index = usize::from(die.index());
        let smu = DieSmu::new(die, delay);
        let buffers = DxeBuffers {
            policy_table: buffers.policy,
            agm_log: buffers.agm_log[index],
            perfmon: buffers.perfmon[index],
        };
        dxe_init(&smu, die.bus(), &buffers).map_err(|error| smu.failure(error))
    })
}

/// The CAC weights the CCX code loads into the DPM weight MSRs, from die 0.
/// Before MP init: the APs can't talk to the SMU in parallel.
#[cfg(feature = "zeppelin")]
pub fn cac_weights<P: ConfigRegionAccess + Copy>(
    die: &Die<P>,
    delay: &mut impl DelayNs,
) -> Result<CacWeights, Failure> {
    let smu = DieSmu::new(die, delay);
    let mut weights = [0; CAC_WEIGHT_COUNT];
    for (index, weight) in (0..).zip(&mut weights) {
        let mut reply = [index, 0, 0, 0, 0, 0];
        smu.send(msg::GET_CAC_WEIGHTS, &mut reply)
            .map_err(|error| smu.failure(error))?;
        *weight = (u64::from(reply[0]) << 32) | u64::from(reply[1]);
    }
    Ok(weights)
}

/// The vendor SMU driver's MP-services callback, after the APs ran their
/// CPU init: [`after_ap_launch`] on every die. AC BTC, which runs a
/// workload on every AP with messages 0x23/0x24/0x25, is off (vendor PCD
/// 0x1bf = 0); DC BTC is on for SP4 and SP4r2 (the SMU driver sets PCD
/// 0x1be).
#[cfg(feature = "zeppelin")]
pub fn aps_ready<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    delay: &mut impl DelayNs,
) -> Result<(), Failure> {
    dies.iter().enumerate().try_for_each(|(n, die)| {
        let smu = DieSmu::new(die, delay);
        after_ap_launch(&smu, n == 0).map_err(|error| smu.failure(error))
    })
}

/// CallbackAfterCcxDxeDone of the vendor SMU driver: [`ccx_done`] on every
/// die with the SP4 features. After [`aps_ready`].
#[cfg(feature = "zeppelin")]
pub fn ccx_done_all<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    delay: &mut impl DelayNs,
) -> Result<(), Failure> {
    dies.iter().try_for_each(|die| {
        let smu = DieSmu::new(die, delay);
        ccx_done(&smu, features::SP4).map_err(|error| smu.failure(error))
    })
}

#[cfg(all(test, feature = "zeppelin"))]
mod tests {
    extern crate std;
    use super::*;
    use crate::ccx::tsc::testing::Delay;
    use crate::soc::zeppelin::test_pci::{Message, Pci};
    use std::vec::Vec;

    fn sent(pci: &Pci) -> Vec<(u8, u32, u32)> {
        pci.messages()
            .iter()
            .map(|m| (m.die, m.message, m.arguments[0]))
            .collect()
    }

    #[test]
    fn launches_aps_across_dies_with_the_complex_stride() {
        use crate::smu::{
            THREAD_CONFIG, THREAD_ENABLE,
            threads::{self, Policy},
        };
        let pci = Pci::new(&[0, 0x40], |_| [0; 6]);
        pci.set_smn(0, THREAD_CONFIG, 0x013); // 4 cores, 2 complexes, SMT
        pci.set_smn(1, THREAD_CONFIG, 0x103); // 4 cores, 1 complex, no SMT
        pci.set_smn(0, THREAD_ENABLE, 0b11); // BSP and thread 1 already run
        let dies = pci.dies();
        assert_eq!(threads::thread_count(&dies, Policy::DEFAULT), Ok(20));
        unsafe { threads::launch(&dies, Policy::DEFAULT, 17, &mut Delay(|_| {})) }.unwrap();
        assert_eq!(pci.smn(0, THREAD_ENABLE), 0xffff);
        assert_eq!(pci.smn(1, THREAD_ENABLE), 0b11);
        let tests: Vec<_> = sent(&pci).iter().map(|&(die, ..)| die).collect();
        assert_eq!(tests.iter().filter(|&&die| die == 0).count(), 14);
        assert_eq!(tests.iter().filter(|&&die| die == 1).count(), 2);
        assert!(
            sent(&pci)
                .iter()
                .all(|&(_, m, a)| m == msg::TEST && a == 0xaa55_aa55)
        );

        assert_eq!(
            unsafe { threads::launch(&dies, Policy::DEFAULT, 20, &mut Delay(|_| {})) },
            Err(threads::Error::ApCount {
                started: 19,
                requested: 20
            })
        );
        assert_eq!(pci.smn(1, THREAD_ENABLE), 0xf);
    }

    #[test]
    fn init_hands_each_die_its_buffers_and_root_bus() {
        let pci = Pci::new(&[0, 0x40], |m: &Message| {
            [
                if m.message == msg::GET_VERSION { 7 } else { 0 },
                0,
                0,
                0,
                0,
                0,
            ]
        });
        let dies = pci.dies();
        let mut buffers = DmaBuffers {
            policy: 0x1_2345_6740,
            agm_log: [0x7000_0000, 0x7000_1000],
            perfmon: [0x7000_2000, 0],
        };
        assert_eq!(
            unsafe { init(&dies, &buffers, &mut Delay(|_| {})) },
            Err(Failure::Buffer)
        );
        assert!(pci.messages().is_empty());
        buffers.perfmon[1] = 0x7000_3000;
        unsafe { init(&dies, &buffers, &mut Delay(|_| {})) }.unwrap();
        let die1: Vec<_> = sent(&pci)
            .into_iter()
            .filter(|&(die, ..)| die == 1)
            .map(|(_, m, a)| (m, a))
            .collect();
        assert_eq!(
            die1,
            [
                (msg::GET_VERSION, 7),
                (msg::SET_POLICY_ADDR_LO, 0x2345_6740),
                (msg::SET_POLICY_ADDR_HI, 1),
                (msg::TRANSFER_POLICY_TABLE, 0),
                (msg::SET_AGM_LOG_ADDR_LO, 0x7000_1000),
                (msg::SET_AGM_LOG_ADDR_HI, 0),
                (msg::SET_PERFMON_ADDR_LO, 0x7000_3000),
                (msg::SET_PERFMON_ADDR_HI, 0),
                (msg::DIE_BUS_3B, 0x40),
                (msg::DIE_BUS_3D, 0x40),
            ]
        );
        assert_eq!(PolicyTable::new(&Policy::default()).as_bytes()[0x0c], 1);
        assert_eq!(PolicyTable::new(&Policy::default()).as_bytes()[0x10], 100);
    }

    #[test]
    fn cac_weights_and_post_mp_messages() {
        let pci = Pci::new(&[0, 0x40], |m: &Message| {
            [0x100 + m.arguments[0], m.arguments[0], 0, 0, 0, 0]
        });
        let dies = pci.dies();
        let weights = cac_weights(&dies[0], &mut Delay(|_| {})).unwrap();
        assert_eq!(weights[20], 0x114_0000_0014);
        assert!(
            sent(&pci)
                .iter()
                .all(|&(die, m, _)| die == 0 && m == msg::GET_CAC_WEIGHTS)
        );

        let pci = Pci::new(&[0, 0x40], |_| [0; 6]);
        let dies = pci.dies();
        aps_ready(&dies, &mut Delay(|_| {})).unwrap();
        ccx_done_all(&dies, &mut Delay(|_| {})).unwrap();
        assert_eq!(
            sent(&pci),
            [
                (0, msg::DC_BTC, 0),
                (0, msg::ENABLE_FEATURES, 0x100),
                (1, msg::ENABLE_FEATURES, 0x100),
                (0, msg::CCX_DONE, 0),
                (0, msg::ENABLE_FEATURES, 0xe73d),
                (0, msg::MSG_38, 0),
                (0, msg::MSG_39, 0),
                (1, msg::CCX_DONE, 0),
                (1, msg::ENABLE_FEATURES, 0xe73d),
                (1, msg::MSG_38, 0),
                (1, msg::MSG_39, 0),
            ]
        );
    }
}

#[cfg(all(test, feature = "naples"))]
mod naples_tests {
    use super::*;
    use crate::smu::model::SmuModel;
    use crate::smu::response;

    #[test]
    fn policy_table_defaults() {
        let t = PolicyTable::new(&Policy::default());
        let b = t.as_bytes();
        assert_eq!(b[0x0C], 1);
        assert_eq!(b[0x10], 100);
        assert!(b[0x14..].iter().all(|&x| x == 0));
        assert_eq!(feature_mask(false, 0), 0x2EF3F);
    }

    #[test]
    fn dxe_sequence() {
        let m = SmuModel::new(MAILBOX, response::OK);
        // SAFETY: model.
        let mb = m.smu(ID_LIMIT);
        let b = DxeBuffers {
            policy_table: 0x1_0000_1000,
            agm_log: 0x2000,
            perfmon: 0x3000,
        };
        dxe_init(&mb, 0x20, &b).unwrap();
        ccx_done(&mb, feature_mask(false, 0)).unwrap();
        assert_eq!(
            m.ids(),
            [
                3, 0x1E, 0x1D, 0x22, 0x20, 0x1F, 0x2E, 0x2D, 0x3B, 0x3D, 0x41, 0x09, 0x38, 0x39
            ]
        );
        let sent = m.sent.borrow();
        assert_eq!((sent[1].1[0], sent[2].1[0]), (0x1000, 1));
        assert_eq!(sent[11].1[0], 0x2EF3F);
    }
}
