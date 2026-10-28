//! SMU V9 (Naples, one SMU per ZP die): mailbox registers, messages,
//! feature mask, policy table and the DXE / CCX-done sequences
//! (NBIO_SMU §3.8; ../sp4r2 NBIO_PCIE_SMU_ZP §2 for the shared ZP code).

use zerocopy::little_endian::{U16, U32};
use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned};

use super::mailbox::Registers;
use super::{SmuError, SmuMessages, send1};

/// V9 mailbox (`smu.v9.mailbox`): waits for the previous message first.
pub const REGS: Registers = Registers {
    message: 0x03B1_0528,
    response: 0x03B1_0564,
    args: 0x03B1_0598,
    wait_before: true,
};

/// No ID range check is known for V9.
pub const ID_LIMIT: u32 = u32::MAX;

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

fn send_split<M: SmuMessages + ?Sized>(
    smu: &M,
    lo: u32,
    hi: u32,
    addr: u64,
) -> Result<(), SmuError> {
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
) -> Result<(), SmuError> {
    match send1(smu, msg::GET_VERSION, 7)? {
        7 => {}
        other => return Err(SmuError::Status(other)),
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
pub fn ccx_done<M: SmuMessages + ?Sized>(smu: &M, features: u32) -> Result<(), SmuError> {
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
pub fn after_ap_launch<M: SmuMessages + ?Sized>(smu: &M, die0: bool) -> Result<(), SmuError> {
    if die0 {
        send1(smu, msg::DC_BTC, 0)?;
    }
    send1(smu, msg::ENABLE_FEATURES, features::CORE_CSTATES).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sp3::smn::Smn;
    use crate::sp3::smu::mailbox::model::{FakeTimer, SmuModel};
    use crate::sp3::smu::mailbox::{Mailbox, response};

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
        let m = SmuModel::new(REGS, response::OK);
        // SAFETY: model.
        let mb =
            unsafe { Mailbox::new(Smn::new(m.clone(), 0), REGS, FakeTimer::default(), ID_LIMIT) };
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
