// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SMU v14 PPTable ("power and performance table", `9Ch` bytes) the
//! SMU reads from DRAM ([`super::msg::SET_PPTABLE_DRAM_ADDRESS`], then
//! [`super::msg::TRANSFER_TABLE_DRAM_TO_SMU`]).
//!
//! Layout: the fill functions of both donors' AmdNbioDxe (GIGABYTE MD25-CG0
//! F01 `0xab3c..0xb0a7`, Dell PR9XJ 1.0.8 `0x11ae0`; instruction-read: one
//! PCD per store), with the field names of the GIGABYTE log
//! (`RUNTIME/pptable_v14.json`). Zero is "the fused value" for the limits;
//! the other fields are what the PCDs give. Fields neither donor writes
//! are zero.

use zerocopy::little_endian::{I32, U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// The PPTable.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct PpTable {
    /// `PcdAmdcTDP` (W; 0 = fused).
    pub tdp: U32,
    /// `PcdCfgPPT` (W; 0 = fused).
    pub ppt: U32,
    pub tdc_cpu: U32,
    pub tdc_soc: U32,
    pub tdc_vddio_mem_s3: U32,
    pub edc: U32,
    pub tjmax: U32,
    /// `PcdCfgPlatformTDP` (W): the platform's limits, board data (VRM and
    /// cooling).
    pub tdp_platform_limit: U32,
    /// `PcdCfgPlatformPPT` (W).
    pub ppt_platform_limit: U32,
    /// `PcdCfgPlatformTDC` (A).
    pub tdc_cpu_platform_limit: U32,
    /// `PcdCfgPlatformTdcSoc` (A).
    pub tdc_soc_platform_limit: U32,
    /// `PcdCfgPlatformTdcMemVddioS3` (A).
    pub tdc_vddio_mem_s3_platform_limit: U32,
    /// `PcdCfgPlatformEDC` (A).
    pub edc_platform_limit: U32,
    pub core_dldo_psm_margin: I32,
    pub vddcr_cpu_voltage_margin: I32,
    pub vddcr_soc_voltage_margin: I32,
    pub vddio_voltage_margin: I32,
    pub cc1_disable: u8,
    /// `PcdAmdDeterminismControl`.
    pub determinism_control: u8,
    pub ccx_vdci_async: u8,
    /// `PcdCfgApbDis`.
    pub apbdis: u8,
    /// `PcdPowerProfileSelect`.
    pub policy: u8,
    pub pcie_speed_control: u8,
    /// `PcdThrottlerMode`, written only when not `0xF` (auto).
    pub throttler_mode: u8,
    pub ccd_fll_selection: u8,
    /// `DfCstateConfigOverride`, `...DdrCmnPllPwrDnDis`, `...XgmiPwrDnDis`,
    /// `...SelfRefrDis`, `...FclkPwrDnDis`, `...UclkPwrDnDis`,
    /// `...SSBDCIPwrDnDis`.
    pub df_cstate: [u8; 7],
    /// `PcdAmdMemPostPackageRepairConfigInitiator`.
    pub ppr_initiator_oob: u8,
    /// `PcdxGMILinkWidthRangeCtrl`.
    pub xgmi_link_width_range_ctrl: u8,
    /// `PcdxGMIMaxLinkWidth`.
    pub xgmi_max_link_width: u8,
    pub xgmi_force_link_width_enable: u8,
    pub xgmi_force_link_width: u8,
    pub telemetry_current_guardband: u8,
    /// `PcdAmdSvi3SvcSpeed`.
    pub svi3_svc_speed: u8,
    pub pcc_limit: U16,
    /// `PcdFchI3cPPHcnt`.
    pub i3c_pp_hcnt: u8,
    /// `PcdAmdFchI2cI3cRetryWatermark`.
    pub i2c_i3c_retry_watermark: u8,
    pub reserved_5e: [u8; 2],
    /// Per bus: `PcdAmdFchI2c<n>SdaRxHold` when the bus is I2C
    /// (`PcdAmdFchI2cI3c<n>` non-zero), else 0.
    pub i2c_i3c_rx_sda_hold: [u8; 4],
    /// Per bus: `PcdAmdFchI2c<n>SdaTxHold` for I2C, `PcdAmdFchI3c<n>SdaHold`
    /// for I3C.
    pub i2c_i3c_tx_sda_hold: [U16; 4],
    /// Per bus: `PcdAmdFchI2c<n>BusFrequency` (low byte) for I2C,
    /// `PcdFchI3c<n>Speed` for I3C.
    pub i2c_i3c_speed: [u8; 4],
    /// Per bus: APCB `CbsCmnFchI3C<n>RetryConfig` (default 10).
    pub i2c_i3c_retry_count: [u8; 4],
    pub oc_disable: u8,
    pub oc_max_vid: u8,
    pub oc_max_frequency: U16,
    pub force_cclk_frequency: U16,
    pub fmax_override: U16,
    /// `PcdCfgFixedSocPstate`.
    pub apbdis_df_pstate: u8,
    /// `PcdDFFODisable`.
    pub dffo_disable: u8,
    pub force_vddcr_cpu_voltage: U16,
    pub force_vddcr_soc_voltage: U16,
    pub force_vddio_voltage: U16,
    /// `PcdDfPstateRangeSupportEn`.
    pub df_pstate_range_support_enable: u8,
    /// `PcdDfPstateRangeMin`.
    pub df_pstate_range_min: u8,
    /// `PcdDfPstateRangeMax`.
    pub df_pstate_range_max: u8,
    pub reserved_87: u8,
    pub xgmi_pstate_range_support_enable: u8,
    pub xgmi_pstate_range_min: u8,
    pub xgmi_pstate_range_max: u8,
    /// `PcdxGMIMinLinkWidth`.
    pub xgmi_min_link_width: u8,
    /// `PcdBalProfilePwrLimit` (logged as `UBPS_PowerLimit`).
    pub balanced_profile_power_limit: U16,
    /// `PcdBalProfileUtilThreshold`.
    pub balanced_profile_util_threshold: u8,
    /// `PcdCircuitCalibrationException`.
    pub circuit_calibration_exception: u8,
    /// `PcdCircuitCalibrationLogging`.
    pub circuit_calibration_logging: u8,
    /// `PcdBalProfileHighUtilThreshold`.
    pub balanced_profile_high_util_threshold: u8,
    pub reserved_92: u8,
    /// `PcdDfcsXgmiPwrDwnCtrl` (Dell only).
    pub dfcs_xgmi_power_down: u8,
    /// `PcdSocDimmPwrLimit`.
    pub soc_dimm_power_limit: U32,
    /// `PcdPcieIdlePowerSetting`, three times (Dell only).
    pub pcie_idle_power: [u8; 3],
    /// `PcdAmdMemDimmHotMr4Enable` | `PcdAmdMemDimmHotPmicEnable` << 1
    /// (Dell only).
    pub dimm_hot: u8,
}

const _: () = assert!(core::mem::size_of::<PpTable>() == 0x9c);

const fn u16le(v: u16) -> U16 {
    U16::from_bytes(v.to_le_bytes())
}

impl PpTable {
    /// Both donors' table without platform limits (every non-limit field
    /// equal in both PCD databases, VALUES.md §3): fused TDP/PPT/TDC/EDC,
    /// SVI3 speed 3, I3C push-pull high count 8, I2C/I3C retry watermark
    /// `0x32`, four I2C buses (`PcdAmdFchI2cI3c<n>` 1) with TX SDA hold
    /// `0x35`, RX hold 0, speed 0 and 10 retries, DFFO disabled, DF P-state
    /// range minimum 2 (support off), balanced-profile high utilization
    /// 100; everything else 0.
    pub const DEFAULT: Self = Self {
        tdp: U32::ZERO,
        ppt: U32::ZERO,
        tdc_cpu: U32::ZERO,
        tdc_soc: U32::ZERO,
        tdc_vddio_mem_s3: U32::ZERO,
        edc: U32::ZERO,
        tjmax: U32::ZERO,
        tdp_platform_limit: U32::ZERO,
        ppt_platform_limit: U32::ZERO,
        tdc_cpu_platform_limit: U32::ZERO,
        tdc_soc_platform_limit: U32::ZERO,
        tdc_vddio_mem_s3_platform_limit: U32::ZERO,
        edc_platform_limit: U32::ZERO,
        core_dldo_psm_margin: I32::ZERO,
        vddcr_cpu_voltage_margin: I32::ZERO,
        vddcr_soc_voltage_margin: I32::ZERO,
        vddio_voltage_margin: I32::ZERO,
        cc1_disable: 0,
        determinism_control: 0,
        ccx_vdci_async: 0,
        apbdis: 0,
        policy: 0,
        pcie_speed_control: 0,
        throttler_mode: 0,
        ccd_fll_selection: 0,
        df_cstate: [0; 7],
        ppr_initiator_oob: 0,
        xgmi_link_width_range_ctrl: 0,
        xgmi_max_link_width: 0,
        xgmi_force_link_width_enable: 0,
        xgmi_force_link_width: 0,
        telemetry_current_guardband: 0,
        svi3_svc_speed: 3,
        pcc_limit: U16::ZERO,
        i3c_pp_hcnt: 8,
        i2c_i3c_retry_watermark: 0x32,
        reserved_5e: [0; 2],
        i2c_i3c_rx_sda_hold: [0; 4],
        i2c_i3c_tx_sda_hold: [u16le(0x35); 4],
        i2c_i3c_speed: [0; 4],
        i2c_i3c_retry_count: [10; 4],
        oc_disable: 0,
        oc_max_vid: 0,
        oc_max_frequency: U16::ZERO,
        force_cclk_frequency: U16::ZERO,
        fmax_override: U16::ZERO,
        apbdis_df_pstate: 0,
        dffo_disable: 1,
        force_vddcr_cpu_voltage: U16::ZERO,
        force_vddcr_soc_voltage: U16::ZERO,
        force_vddio_voltage: U16::ZERO,
        df_pstate_range_support_enable: 0,
        df_pstate_range_min: 2,
        df_pstate_range_max: 0,
        reserved_87: 0,
        xgmi_pstate_range_support_enable: 0,
        xgmi_pstate_range_min: 0,
        xgmi_pstate_range_max: 0,
        xgmi_min_link_width: 0,
        balanced_profile_power_limit: U16::ZERO,
        balanced_profile_util_threshold: 0,
        circuit_calibration_exception: 0,
        circuit_calibration_logging: 0,
        balanced_profile_high_util_threshold: 100,
        reserved_92: 0,
        dfcs_xgmi_power_down: 0,
        soc_dimm_power_limit: U32::ZERO,
        pcie_idle_power: [0; 3],
        dimm_hot: 0,
    };

    /// This table with a board's platform limits.
    pub const fn with_platform_limits(self, limits: PlatformLimits) -> Self {
        Self {
            tdp_platform_limit: U32::new(limits.tdp_w),
            ppt_platform_limit: U32::new(limits.ppt_w),
            tdc_cpu_platform_limit: U32::new(limits.tdc_cpu_a),
            tdc_soc_platform_limit: U32::new(limits.tdc_soc_a),
            tdc_vddio_mem_s3_platform_limit: U32::new(limits.tdc_vddio_a),
            edc_platform_limit: U32::new(limits.edc_a),
            ..self
        }
    }
}

/// A board's platform power limits (`PcdCfgPlatform{TDP,PPT,TDC,TdcSoc,
/// TdcMemVddioS3,EDC}`); 0 = fused. The values are the board's (its
/// VRMs and cooling), named in its crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformLimits {
    pub tdp_w: u32,
    pub ppt_w: u32,
    pub tdc_cpu_a: u32,
    pub tdc_soc_a: u32,
    pub tdc_vddio_a: u32,
    pub edc_a: u32,
}

impl PlatformLimits {
    /// Fused values everywhere.
    pub const FUSED: Self = Self {
        tdp_w: 0,
        ppt_w: 0,
        tdc_cpu_a: 0,
        tdc_soc_a: 0,
        tdc_vddio_a: 0,
        edc_a: 0,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_the_fill_functions() {
        let t = PpTable::DEFAULT.with_platform_limits(PlatformLimits {
            tdp_w: 600,
            ppt_w: 600,
            tdc_cpu_a: 330,
            tdc_soc_a: 310,
            tdc_vddio_a: 210,
            edc_a: 430,
        });
        let b = t.as_bytes();
        let u32_at = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        assert_eq!(b.len(), 0x9c);
        assert_eq!(
            [0x1c, 0x20, 0x24, 0x28, 0x2c, 0x30].map(u32_at),
            [600, 600, 330, 310, 210, 430]
        );
        assert_eq!((b[0x59], b[0x5c], b[0x5d]), (3, 8, 0x32));
        assert_eq!(&b[0x64..0x6c], &[0x35, 0, 0x35, 0, 0x35, 0, 0x35, 0]);
        assert_eq!(&b[0x70..0x74], &[10; 4]);
        assert_eq!((b[0x7d], b[0x85], b[0x91]), (1, 2, 100));
        // Five two-byte limits and 210 (one byte); 3 + 4 + 4 + 3 bytes.
        assert_eq!(b.iter().filter(|&&x| x != 0).count(), 11 + 14);
        assert!(
            PpTable::DEFAULT.as_bytes()[..0x34].iter().all(|&x| x == 0),
            "fused limits"
        );
    }
}
