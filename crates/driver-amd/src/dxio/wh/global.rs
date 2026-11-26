// SPDX-License-Identifier: MIT OR Apache-2.0
//! WH MPIO global configuration, request `0x02`'s six arguments
//! (`MpioGlobalConfigDefaults` Dell `0xf5e790`, then
//! `MpioCfgGlobalConfigWh` Dell `0xf64550`, GIGABYTE `0x709b7f52`;
//! decompiler-inferred, both donors; MPIO.md §2.6).
//!
//! Unlike the RS firmware, skip-vetting is bit 0 of the first word and bit
//! 8 is the loopback enable.

use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u32,
    /// Argument 0.
    pub Global0 [
        /// `CfgSkipPspMessage == 1`: no PSP PHY vetting.
        SKIP_VETTING OFFSET(0) NUMBITS(1) [],
        /// 1 when `DxioSaveRestoreModes == 1`.
        SAVE_RESTORE OFFSET(2) NUMBITS(2) [],
        /// `PCIeExactMatchEnable`: match port sizes exactly.
        EXACT_MATCH OFFSET(4) NUMBITS(1) [],
        /// Use the PHY SRAM (default 1; `DxioPhyProgramming`).
        USE_PHY_SRAM OFFSET(6) NUMBITS(1) [],
        /// The PHY firmware is valid (default 1; `DxioPhyValid`).
        VALID_PHY_FW OFFSET(7) NUMBITS(1) [],
        /// `PcdCfgPcieLoopbackMode`.
        LOOPBACK OFFSET(8) NUMBITS(1) [],
        /// `PcdPcieNonPcieCompliantTrainingFailureSupport`.
        NONCOMPLIANT_WA OFFSET(11) NUMBITS(1) [],
        /// `PcdActiveSltMode`.
        ACTIVE_SLT OFFSET(12) NUMBITS(1) [],
        /// Safe recovery on BER-exceeded errors.
        SAFE_RECOVERY_BER OFFSET(22) NUMBITS(1) [],
        /// Periodic Zcal.
        PERIODIC_ZCAL OFFSET(23) NUMBITS(1) [],
        /// PCIe device authentication.
        DEVICE_AUTH OFFSET(26) NUMBITS(1) []
    ],
    /// Argument 1.
    pub Global1 [
        /// `CfgDxioClockGating`.
        CLOCK_GATING OFFSET(0) NUMBITS(1) []
    ],
    /// Argument 2 (only with `PcieDxioTimingControlEnable`).
    pub Global2 [
        RX_DETECT_POLLING OFFSET(0) NUMBITS(16) [],
        L0_POLLING OFFSET(16) NUMBITS(16) []
    ],
    /// Argument 3.
    pub Global3 [
        /// With `PcieDxioTimingControlEnable`.
        RESET_TO_TRAINING OFFSET(0) NUMBITS(16) [],
        /// `PcdLinkDisableAtPowerOffDelay`.
        LINK_DISABLE_DELAY OFFSET(18) NUMBITS(4) [],
        /// `PcdEnable2SpcGen4` (1 in both donors' input blocks: Dell
        /// `0xf0ff5d`, GIGABYTE `0x7098a7db`, instruction-checked).
        ENABLE_2SPC_GEN4 OFFSET(22) NUMBITS(1) [],
        /// `ReicResponseDisable`; always 1 on SP7 A0.
        REIC_RESPONSE_DISABLE OFFSET(23) NUMBITS(1) [],
        /// `PcdCfgDxioTxFIFORdPtrOffset`.
        TX_FIFO_RDPTR_OFFSET OFFSET(24) NUMBITS(8) []
    ],
    /// Argument 4.
    pub Global4 [
        /// `!SbrBrokenLaneAvoidanceSup`.
        NO_SBR_BROKEN_LANE_AVOIDANCE OFFSET(0) NUMBITS(1) [],
        /// `!AutoFullMarginSup`.
        NO_AUTO_FULL_MARGIN OFFSET(1) NUMBITS(1) [],
        /// `CfgPcieCVTestWA`.
        CV_TEST_WA OFFSET(2) NUMBITS(1) []
    ],
    /// Argument 5.
    pub Global5 [
        ENABLE_2SPC_GEN5 OFFSET(0) NUMBITS(1) [],
        EXTENDED_EIEOS OFFSET(1) NUMBITS(1) [],
        /// `PcieIdlePowerSettings` (Dell only; GIGABYTE leaves it 0).
        IDLE_POWER OFFSET(2) NUMBITS(1) [],
        MCTP_UNICAST OFFSET(3) NUMBITS(1) []
    ]
];

/// The link timing overrides (`PcieDxioTimingControlEnable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    pub rx_detect_polling: u16,
    pub l0_polling: u16,
    pub reset_to_training: u16,
}

/// MPIO global configuration policy. [`Default`] is the vendor's
/// `MpioGlobalConfigDefaults` with its fixed input-block values and
/// `ReicResponseDisable` as both donors' PCD has it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlobalConfig {
    pub skip_psp_vetting: bool,
    /// `DxioSaveRestoreModes == 1`.
    pub save_restore: bool,
    pub exact_match: bool,
    pub use_phy_sram: bool,
    pub valid_phy_fw: bool,
    pub loopback: bool,
    pub noncompliant_training_wa: bool,
    pub active_slt: bool,
    pub safe_recovery_ber: bool,
    pub periodic_zcal: bool,
    /// PCIe device authentication: MPIO holds the links until each
    /// socket's SMU got `PcieLinkClearToTrain`, which the platform sends
    /// once every instance finished phase 1
    /// ([`crate::smu::v14::msg::clear_to_train`]; MPIO.md G12).
    pub device_auth: bool,
    pub clock_gating: bool,
    pub timing: Option<Timing>,
    /// 4 bits.
    pub link_disable_poweroff_delay: u8,
    pub enable_2spc_gen4: bool,
    /// `ReicResponseDisable`, forced on SP7 A0 ([`Self::dwords`]). The
    /// vendor presets it (Dell `0xf5e790`) and then overwrites it from
    /// `PcdReicResponseDisable` (Dell token 0xEF, GIGABYTE ex `0x4161D`),
    /// which is **0** in both donors (VALUES.md §3): off in
    /// [`Self::DEFAULT`].
    pub reic_response_disable: bool,
    pub tx_fifo_rdptr_offset: u8,
    pub sbr_broken_lane_avoidance: bool,
    pub auto_full_margin: bool,
    pub cv_test_wa: bool,
    pub enable_2spc_gen5: bool,
    pub extended_eieos: bool,
    pub idle_power: bool,
    pub mctp_unicast: bool,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl GlobalConfig {
    /// `MpioGlobalConfigDefaults` (dword 0 bits 6/7 and dword 3 bit 23 set,
    /// all else clear) with `PcdEnable2SpcGen4` = 1, SBR broken-lane
    /// avoidance and auto full margining supported and the donors'
    /// `PcdReicResponseDisable` 0 over the preset bit 23.
    pub const DEFAULT: Self = Self {
        skip_psp_vetting: false,
        save_restore: false,
        exact_match: false,
        use_phy_sram: true,
        valid_phy_fw: true,
        loopback: false,
        noncompliant_training_wa: false,
        active_slt: false,
        safe_recovery_ber: false,
        periodic_zcal: false,
        device_auth: false,
        clock_gating: false,
        timing: None,
        link_disable_poweroff_delay: 0,
        enable_2spc_gen4: true,
        reic_response_disable: false,
        tx_fifo_rdptr_offset: 0,
        sbr_broken_lane_avoidance: true,
        auto_full_margin: true,
        cv_test_wa: false,
        enable_2spc_gen5: false,
        extended_eieos: false,
        idle_power: false,
        mctp_unicast: false,
    };

    /// The Dell PR9XJ 1.0.8 build's values (PEI PCD database [P],
    /// DEVICES.md §2.4): `CfgSkipPspMessage` 1, `PCIeExactMatchEnable` 1,
    /// `NonPcieCompliantTrainingFailureSupport` 1 (dword 0 `0x8D1`),
    /// `CfgDxioClockGating` 1 (dword 1 `1`), no timing control (dword 2
    /// 0), 2SPC Gen4, `TxFIFORdPtrOffset` `0xEC`, `ReicResponseDisable` 0
    /// (dword 3 `0xEC40_0000`), SBR broken-lane avoidance and auto full
    /// margining supported (dword 4 0), `PcdAmdNbioGen5SpcMode` 1 and
    /// `EieosMode` 1 (dword 5 `3`); save/restore, SLT, safe recovery,
    /// periodic calibration, device authentication and idle power off.
    pub const DELL_BUILD: Self = Self {
        skip_psp_vetting: true,
        exact_match: true,
        noncompliant_training_wa: true,
        clock_gating: true,
        reic_response_disable: false,
        tx_fifo_rdptr_offset: 0xec,
        enable_2spc_gen5: true,
        extended_eieos: true,
        ..Self::DEFAULT
    };

    /// The GIGABYTE MD25-CG0 F01 build's values (DEVICES.md §2.4: dword 0
    /// `0x8C1`, no exact match; dwords 3 and 5 as Dell's).
    /// `CfgDxioClockGating` (dword 1) is 1 as Dell's: GIGABYTE's host glue
    /// stores ModulePkg ex `0x4150B` (bool, PCD database 1, no setter) in
    /// the MPIO input block's byte 0 (SilVenicePei `0x7092b6e8`), where
    /// Dell's stores `PcdCfgDxioClockGating` (`0xf0f5c8`; RESIDUAL.md §8).
    pub const GIGABYTE_BUILD: Self = Self {
        exact_match: false,
        ..Self::DELL_BUILD
    };

    /// The six arguments of request `0x02`; `a0` is SP7 A0.
    pub fn dwords(&self, a0: bool) -> [u32; 6] {
        let mut w0 = LocalRegisterCopy::<u32, Global0::Register>::new(0);
        w0.write(
            Global0::SKIP_VETTING.val(self.skip_psp_vetting.into())
                + Global0::SAVE_RESTORE.val(self.save_restore.into())
                + Global0::EXACT_MATCH.val(self.exact_match.into())
                + Global0::USE_PHY_SRAM.val(self.use_phy_sram.into())
                + Global0::VALID_PHY_FW.val(self.valid_phy_fw.into())
                + Global0::LOOPBACK.val(self.loopback.into())
                + Global0::NONCOMPLIANT_WA.val(self.noncompliant_training_wa.into())
                + Global0::ACTIVE_SLT.val(self.active_slt.into())
                + Global0::SAFE_RECOVERY_BER.val(self.safe_recovery_ber.into())
                + Global0::PERIODIC_ZCAL.val(self.periodic_zcal.into())
                + Global0::DEVICE_AUTH.val(self.device_auth.into()),
        );
        let mut w1 = LocalRegisterCopy::<u32, Global1::Register>::new(0);
        w1.write(Global1::CLOCK_GATING.val(self.clock_gating.into()));
        let timing = self.timing.unwrap_or(Timing {
            rx_detect_polling: 0,
            l0_polling: 0,
            reset_to_training: 0,
        });
        let mut w2 = LocalRegisterCopy::<u32, Global2::Register>::new(0);
        w2.write(
            Global2::RX_DETECT_POLLING.val(timing.rx_detect_polling.into())
                + Global2::L0_POLLING.val(timing.l0_polling.into()),
        );
        let mut w3 = LocalRegisterCopy::<u32, Global3::Register>::new(0);
        w3.write(
            Global3::RESET_TO_TRAINING.val(timing.reset_to_training.into())
                + Global3::LINK_DISABLE_DELAY
                    .val(u32::from(self.link_disable_poweroff_delay & 0xf))
                + Global3::ENABLE_2SPC_GEN4.val(self.enable_2spc_gen4.into())
                + Global3::REIC_RESPONSE_DISABLE.val((a0 || self.reic_response_disable).into())
                + Global3::TX_FIFO_RDPTR_OFFSET.val(self.tx_fifo_rdptr_offset.into()),
        );
        let mut w4 = LocalRegisterCopy::<u32, Global4::Register>::new(0);
        w4.write(
            Global4::NO_SBR_BROKEN_LANE_AVOIDANCE.val((!self.sbr_broken_lane_avoidance).into())
                + Global4::NO_AUTO_FULL_MARGIN.val((!self.auto_full_margin).into())
                + Global4::CV_TEST_WA.val(self.cv_test_wa.into()),
        );
        let mut w5 = LocalRegisterCopy::<u32, Global5::Register>::new(0);
        w5.write(
            Global5::ENABLE_2SPC_GEN5.val(self.enable_2spc_gen5.into())
                + Global5::EXTENDED_EIEOS.val(self.extended_eieos.into())
                + Global5::IDLE_POWER.val(self.idle_power.into())
                + Global5::MCTP_UNICAST.val(self.mctp_unicast.into()),
        );
        [w0.get(), w1.get(), w2.get(), w3.get(), w4.get(), w5.get()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn donor_builds_give_the_donors_dwords() {
        assert_eq!(
            GlobalConfig::DELL_BUILD.dwords(false),
            [0x8d1, 1, 0, 0xec40_0000, 0, 3]
        );
        assert_eq!(GlobalConfig::GIGABYTE_BUILD.dwords(false)[0], 0x8c1);
        // A0 still forces ReicResponseDisable.
        assert_eq!(GlobalConfig::DELL_BUILD.dwords(true)[3], 0xecc0_0000);
    }

    #[test]
    fn defaults_match_the_vendor_and_bits_are_where_the_donors_put_them() {
        assert_eq!(
            GlobalConfig::default().dwords(false),
            [0xc0, 0, 0, 1 << 22, 0, 0]
        );
        // SP7 A0 forces ReicResponseDisable.
        assert_eq!(GlobalConfig::DEFAULT.dwords(true)[3], 3 << 22);
        let reic_on = GlobalConfig {
            reic_response_disable: true,
            ..GlobalConfig::DEFAULT
        };
        assert_eq!(reic_on.dwords(false)[3], 3 << 22);
        let all = GlobalConfig {
            skip_psp_vetting: true,
            save_restore: true,
            exact_match: true,
            use_phy_sram: false,
            valid_phy_fw: false,
            loopback: true,
            noncompliant_training_wa: true,
            active_slt: true,
            safe_recovery_ber: true,
            periodic_zcal: true,
            device_auth: true,
            clock_gating: true,
            timing: Some(Timing {
                rx_detect_polling: 0x1234,
                l0_polling: 0x5678,
                reset_to_training: 0x9abc,
            }),
            link_disable_poweroff_delay: 0xf,
            enable_2spc_gen4: false,
            reic_response_disable: false,
            tx_fifo_rdptr_offset: 0xa5,
            sbr_broken_lane_avoidance: false,
            auto_full_margin: false,
            cv_test_wa: true,
            enable_2spc_gen5: true,
            extended_eieos: true,
            idle_power: true,
            mctp_unicast: true,
        };
        assert_eq!(
            all.dwords(false),
            [
                1 | 1 << 2 | 1 << 4 | 1 << 8 | 1 << 11 | 1 << 12 | 1 << 22 | 1 << 23 | 1 << 26,
                1,
                0x5678_1234,
                0xa500_0000 | 0xf << 18 | 0x9abc,
                0b111,
                0b1111
            ]
        );
    }
}
