//! The V11 PPTable ("power and performance table") the SMU DMAs at boot:
//! 0x27C bytes on Rome, 0x304 on Milan (NBIO_SMU §3.8, JSON `pptable`).
//!
//! Milan's layout is SMU interface v45.65 (public: Oxide `milan/pptable.h`
//! offsets); every offset the donor writes matches it. Rome's layout is the
//! donor's field stores; fields the donor does not write are reserved, and
//! the APBDIS (+0x40) / fixed DF P-state (+0x6C) placement is **inferred**
//! from Milan's layout minus its extra xGMI-min and HTF fields (NBIO_SMU
//! §7.6). At donor defaults every limit is 0 (fused), determinism is off,
//! and the CPPC APIC-ID map is filled.

use zerocopy::little_endian::{I32, U16, U32};
use zerocopy::{FromBytes, FromZeros, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Default limits (W, A, °C); 0 = fused value.
#[derive(Debug, Clone, Copy, Default, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Limits {
    pub tdp: U32,
    pub ppt: U32,
    pub tdc: U32,
    pub edc: U32,
    pub tjmax: U32,
}

/// Platform limits; 0 = fused value.
#[derive(Debug, Clone, Copy, Default, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct PlatformLimits {
    pub tdp: U32,
    pub ppt: U32,
    pub tdc: U32,
    pub edc: U32,
}

/// The fields at +0x30..+0x44 common to both layouts.
#[derive(Debug, Clone, Copy, Default, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct Tuning {
    pub core_dldo_psm_margin: I32,
    /// mV.
    pub vddcr_cpu_voltage_margin: I32,
    /// mV (Milan; reserved on Rome).
    pub vddcr_soc_voltage_margin: I32,
    pub cc1_disable: u8,
    pub determinism_percentage_enable: u8,
    /// 100 = performance, 0 = power.
    pub determinism_percentage: u8,
    /// Milan: CCX DCI mode (1 = async); reserved on Rome.
    pub ccx_dci_mode: u8,
    /// APBDIS: run the data fabric at a fixed P-state (Rome: INF).
    pub apbdis: u8,
    pub efficiency_mode: u8,
    pub power_management_override: u8,
    pub power_management: u8,
}

/// Rome PPTable (0x27C).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct RomePpTable {
    pub limits: Limits,
    pub platform: PlatformLimits,
    /// Fan table (written only with PCD 0x27D).
    pub fan: [u8; 12],
    pub tuning: Tuning,
    pub pcie_esm: [u8; 4],
    /// DF C-state configuration.
    pub df_cstate: [u8; 7],
    /// EDC select mode extension (donor: 3 when APCB 0x500AB, else PCD
    /// 0x28F − 1; meaning not decoded).
    pub edc_select_mode: u8,
    pub xgmi_max_width_enable: u8,
    pub xgmi_max_width: u8,
    pub xgmi_force_width_enable: u8,
    pub xgmi_force_width: u8,
    pub telemetry: [u8; 16],
    pub oc_disable: u8,
    pub oc_min_vid: u8,
    pub oc_max_freq: U16,
    _rsvd_68: [u8; 4],
    /// DF P-state used when APBDIS is set (INF).
    pub apbdis_df_pstate: u8,
    _rsvd_6d: [u8; 7],
    pub cppc_thread_count: U16,
    _rsvd_76: [u8; 2],
    /// CPPC thread → APIC ID map, 0xFFFF fill.
    pub cppc_apic_ids: [U16; 256],
    _rsvd_278: [u8; 4],
}

/// Milan PPTable (0x304, SMU v45.65).
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned)]
#[repr(C)]
pub struct MilanPpTable {
    pub limits: Limits,
    pub platform: PlatformLimits,
    pub fan: [u8; 12],
    pub tuning: Tuning,
    pub pcie_esm: [u8; 4],
    pub df_cstate: [u8; 7],
    pub ccr_enable: u8,
    pub xgmi_max_width_enable: u8,
    pub xgmi_max_width: u8,
    pub xgmi_min_width_enable: u8,
    pub xgmi_min_width: u8,
    pub xgmi_force_width_enable: u8,
    pub xgmi_force_width: u8,
    _rsvd_56: [u8; 2],
    pub telemetry: [u8; 16],
    pub oc_disable: u8,
    pub oc_min_vid: u8,
    pub oc_max_freq: U16,
    /// MHz; 0 = don't force.
    pub cclk_freq: U16,
    /// MHz; 0 = no override.
    pub fmax_override: U16,
    /// DF P-state used when APBDIS is set.
    pub apbdis_df_pstate: u8,
    pub df_pstate_freq_override_disable: u8,
    pub df_pstate_latency_override_disable: u8,
    _rsvd_73: u8,
    /// HTF / MTF temperature and frequency overrides; 0 = fused.
    pub htf: [U16; 4],
    pub cppc_override: u8,
    pub cppc_epp: u8,
    pub cppc_perf_max: u8,
    pub cppc_perf_min: u8,
    pub cppc_thread_count: U16,
    _rsvd_82: [u8; 2],
    pub cppc_apic_ids: [U16; 256],
    pub vddcr_cpu_force: U16,
    pub vddcr_soc_force: U16,
    pub cstate_boost_override: U16,
    pub global_fmax_override: U16,
    pub max_did_override: u8,
    pub cca_enable: u8,
    _rsvd_28e: [u8; 2],
    pub l3_credit_ceiling: U32,
    _rsvd_294: [u8; 0x70],
}

const _: () = assert!(size_of::<RomePpTable>() == 0x27C);
const _: () = assert!(size_of::<MilanPpTable>() == 0x304);
const _: () = assert!(core::mem::offset_of!(RomePpTable, cppc_apic_ids) == 0x78);
const _: () = assert!(core::mem::offset_of!(MilanPpTable, cppc_apic_ids) == 0x84);
const _: () = assert!(core::mem::offset_of!(MilanPpTable, apbdis_df_pstate) == 0x70);
const _: () = assert!(core::mem::offset_of!(MilanPpTable, l3_credit_ceiling) == 0x290);

/// Board/CBS power policy; `Default` = the donor PCD defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PowerPolicy {
    /// Default limits (Rome: only TDP/PPT have PCDs).
    pub tdp: u32,
    pub ppt: u32,
    pub tdc: u32,
    pub edc: u32,
    pub tjmax: u32,
    /// Platform limits (PPT default 0x118 = 280 W on both donors).
    pub platform_tdp: u32,
    pub platform_ppt: u32,
    pub platform_tdc: u32,
    pub platform_edc: u32,
    pub core_dldo_psm_margin: i32,
    pub vddcr_cpu_voltage_margin: i32,
    pub cc1_disable: bool,
    /// Rome PCD 0x185 / Milan 0x164 (only honoured on SP3).
    pub determinism_enable: bool,
    /// Rome PCD 0x2C / Milan 0x163: 100 % (performance) or 0.
    pub determinism_performance: bool,
    /// APBDIS (Rome PCD 0x18A, Milan 0x1A8).
    pub apbdis: bool,
    /// Fixed SOC/DF P-state with APBDIS (Rome PCD 0x18C, Milan 0x1AC).
    pub fixed_soc_pstate: u8,
    pub efficiency_mode: bool,
    pub xgmi_max_width: Option<u8>,
    /// Milan only.
    pub xgmi_min_width: Option<u8>,
    pub xgmi_force_width: Option<u8>,
    /// Milan: `fmax_override` (PCD 0x30B), `global_fmax_override` (0x258),
    /// `cstate_boost_override` (0x25B), `l3_credit_ceiling` (0x25D).
    pub fmax_override: u16,
    pub global_fmax_override: u16,
    pub cstate_boost_override: u16,
    pub l3_credit_ceiling: u32,
    /// Milan PCD 0x25E; the donor writes 1 for the default 0xFF (DC).
    pub max_did_override: u8,
    /// Milan PCD 0x259.
    pub cca: bool,
}

impl Default for PowerPolicy {
    fn default() -> Self {
        Self {
            tdp: 0,
            ppt: 0,
            tdc: 0,
            edc: 0,
            tjmax: 0,
            platform_tdp: 0,
            platform_ppt: 0x118,
            platform_tdc: 0,
            platform_edc: 0,
            core_dldo_psm_margin: 0,
            vddcr_cpu_voltage_margin: 0,
            cc1_disable: false,
            determinism_enable: false,
            determinism_performance: true,
            apbdis: false,
            fixed_soc_pstate: 0,
            efficiency_mode: false,
            xgmi_max_width: None,
            xgmi_min_width: None,
            xgmi_force_width: None,
            fmax_override: 0,
            global_fmax_override: 0,
            cstate_boost_override: 0,
            l3_credit_ceiling: 0,
            max_did_override: 1,
            cca: true,
        }
    }
}

impl PowerPolicy {
    fn limits(&self) -> (Limits, PlatformLimits) {
        (
            Limits {
                tdp: self.tdp.into(),
                ppt: self.ppt.into(),
                tdc: self.tdc.into(),
                edc: self.edc.into(),
                tjmax: self.tjmax.into(),
            },
            PlatformLimits {
                tdp: self.platform_tdp.into(),
                ppt: self.platform_ppt.into(),
                tdc: self.platform_tdc.into(),
                edc: self.platform_edc.into(),
            },
        )
    }

    fn tuning(&self) -> Tuning {
        Tuning {
            core_dldo_psm_margin: self.core_dldo_psm_margin.into(),
            vddcr_cpu_voltage_margin: self.vddcr_cpu_voltage_margin.into(),
            cc1_disable: self.cc1_disable as u8,
            determinism_percentage_enable: self.determinism_enable as u8,
            determinism_percentage: if self.determinism_performance { 100 } else { 0 },
            apbdis: self.apbdis as u8,
            efficiency_mode: self.efficiency_mode as u8,
            ..Tuning::default()
        }
    }
}

/// `(enable, value)` of an optional width.
fn width(w: Option<u8>) -> (u8, u8) {
    w.map_or((0, 0), |w| (1, w))
}

/// The CPPC map: `ids` in thread order, the rest 0xFFFF.
fn cppc_map(ids: &[u16]) -> (U16, [U16; 256]) {
    let n = ids.len().min(256);
    let map = core::array::from_fn(|i| ids.get(i).copied().unwrap_or(0xFFFF).into());
    (U16::new(n as u16), map)
}

impl RomePpTable {
    /// Build the table from `p` and the socket's thread APIC IDs.
    pub fn new(p: &PowerPolicy, cppc_apic_ids: &[u16]) -> Self {
        let mut t = Self::new_zeroed();
        (t.limits, t.platform) = p.limits();
        t.tuning = p.tuning();
        (t.xgmi_max_width_enable, t.xgmi_max_width) = width(p.xgmi_max_width);
        (t.xgmi_force_width_enable, t.xgmi_force_width) = width(p.xgmi_force_width);
        t.apbdis_df_pstate = p.fixed_soc_pstate;
        (t.cppc_thread_count, t.cppc_apic_ids) = cppc_map(cppc_apic_ids);
        t
    }
}

impl MilanPpTable {
    /// Build the table from `p` and the socket's thread APIC IDs.
    pub fn new(p: &PowerPolicy, cppc_apic_ids: &[u16]) -> Self {
        let mut t = Self::new_zeroed();
        (t.limits, t.platform) = p.limits();
        t.tuning = p.tuning();
        (t.xgmi_max_width_enable, t.xgmi_max_width) = width(p.xgmi_max_width);
        (t.xgmi_min_width_enable, t.xgmi_min_width) = width(p.xgmi_min_width);
        (t.xgmi_force_width_enable, t.xgmi_force_width) = width(p.xgmi_force_width);
        t.fmax_override = p.fmax_override.into();
        t.apbdis_df_pstate = p.fixed_soc_pstate;
        (t.cppc_thread_count, t.cppc_apic_ids) = cppc_map(cppc_apic_ids);
        t.cstate_boost_override = p.cstate_boost_override.into();
        t.global_fmax_override = p.global_fmax_override.into();
        t.max_did_override = p.max_did_override;
        t.cca_enable = p.cca as u8;
        t.l3_credit_ceiling = p.l3_credit_ceiling.into();
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apbdis_offsets() {
        let p = PowerPolicy {
            apbdis: true,
            fixed_soc_pstate: 2,
            ..PowerPolicy::default()
        };
        let r = RomePpTable::new(&p, &[0, 1]);
        let rb = r.as_bytes();
        assert_eq!((rb[0x40], rb[0x6C]), (1, 2));
        assert_eq!(&rb[0x18..0x1C], &0x118u32.to_le_bytes());
        assert_eq!(rb[0x3E], 100);
        assert_eq!(&rb[0x74..0x7C], &[2, 0, 0, 0, 0, 0, 1, 0]);
        assert_eq!(&rb[0x7C..0x7E], &[0xFF, 0xFF]);

        let m = MilanPpTable::new(&p, &[]);
        let mb = m.as_bytes();
        assert_eq!((mb[0x40], mb[0x70]), (1, 2));
        assert_eq!((mb[0x28C], mb[0x28D]), (1, 1));
    }
}
