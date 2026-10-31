// SPDX-License-Identifier: MIT OR Apache-2.0
// Reviewed firmware defaults. Rust owns the ABI; these are not user templates.
use super::*;
fn mj11_psp_override() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Psp,
        layer: ParameterLayer::Override,
        values: vec![Setting {
            token: amd_apcb::ParameterTokenConfig::Cbs01,
            width: Width::U8,
            value: 0,
            time: amd_apcb::ParameterTimePoint::Any,
        }],
    })
}
fn mj11_ccx_override() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Ccx,
        layer: ParameterLayer::Override,
        values: vec![Setting {
            token: amd_apcb::ParameterTokenConfig::CcxMinSevAsid,
            width: Width::U32,
            value: 1,
            time: amd_apcb::ParameterTimePoint::Any,
        }],
    })
}
fn mj11_df_override() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Df,
        layer: ParameterLayer::Override,
        values: vec![
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfGmiEncrypt,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfXgmiEncrypt,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfSaveRestoreMemEncrypt,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfSysStorageAtTopOfMem,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfProbeFilter,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfBottomIo,
                width: Width::U8,
                value: 128,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfMemInterleaving,
                width: Width::U8,
                value: 7,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfMemInterleavingSize,
                width: Width::U8,
                value: 7,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfMemInterleavingHash,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfPciMmioSize,
                width: Width::U32,
                value: 134217728,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfCakeCrcThreshPerfBounds,
                width: Width::U32,
                value: 100,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::DfMemClear,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
fn mj11_memory_override() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Memory,
        layer: ParameterLayer::Override,
        values: vec![
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemBottomIo,
                width: Width::U16,
                value: 224,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemHoleRemapping,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemLimitToBelow1TiB,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUserTimingMode,
                width: Width::U32,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemClockValue,
                width: Width::U32,
                value: 1200,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableChipSelectInterleaving,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableChannelInterleaving,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableEccFeature,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnablePowerDown,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableParity,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableBankSwizzle,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableClearing,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUmaMode,
                width: Width::U8,
                value: 2,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUmaSize,
                width: Width::U32,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemRestoreControl,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemSaveMemContextControl,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemIsCapsuleMode,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemForceTraining,
                width: Width::U8,
                value: 2,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmTypeMixedConfig,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableAmp,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDramDoubleRefreshRate,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemPmuTrainingMode,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEccRedirection,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubDramRate,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubL2Rate,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubL3Rate,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubInstructionCacheRate,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubDataCacheRate,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEccSyncFlood,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEccSymbolSize,
                width: Width::U16,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDqsTrainingControl,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUmaAbove4GiB,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUmaAlignment,
                width: Width::U32,
                value: 16777152,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableAllClocks,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemBusFrequencyLimit,
                width: Width::U32,
                value: 1333,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemPowerDownMode,
                width: Width::U32,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemIgnoreSpdChecksum,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemModeUnganged,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemQuadRankCapable,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemRdimmCapable,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemLrdimmCapable,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUdimmCapable,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemSodimmCapable,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableDoubleRefreshRate,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmTypeDdr4Capable,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmTypeDdr3Capable,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmTypeLpddr3Capable,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableZqReset,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableBankGroupSwap,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableOdtsCmdThrottle,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableSwCmdThrottle,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableForcePowerDownThrotle,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemOdtsCmdThrottleCycles,
                width: Width::U8,
                value: 87,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemSwCmdThrottleCycles,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorConf,
                width: Width::U16,
                value: 1036,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorUpper,
                width: Width::U16,
                value: 80,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorLower,
                width: Width::U16,
                value: 10,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorCritical,
                width: Width::U16,
                value: 95,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorResolution,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemAutoRefreshFineGranMode,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnablePState,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDdrRouteBalancedTee,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableMbistTest,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemMbistSubtestType,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemMbistAggressorOn,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemMbistHaltOnError,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemCpuVrefRange,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDramVrefRange,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableTsme,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemNvdimmPowerSource,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDataPoison,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDataScramble,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemPlatformSpecificErrorHandling,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableTemperatureControlledRefresh,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemMbistPerBitSecondaryDieReport,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableBankGroupSwapAlt,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
fn mj11_spd_smbus() -> Result<Table> {
    Ok(Table::SpdSmbus {
        slots: native(
            json!([{"socket":0,"channel":0,"dimm":0,"address":160,"mux":226,"mux-channel":4,"mux-control":4},{"socket":0,"channel":0,"dimm":1,"address":162,"mux":226,"mux-channel":4,"mux-control":4},{"socket":0,"channel":1,"dimm":0,"address":164,"mux":226,"mux-channel":4,"mux-control":4},{"socket":0,"channel":1,"dimm":1,"address":166,"mux":226,"mux-channel":4,"mux-control":4},{"socket":0,"channel":2,"dimm":0,"address":168,"mux":226,"mux-channel":5,"mux-control":5},{"socket":0,"channel":2,"dimm":1,"address":170,"mux":226,"mux-channel":5,"mux-control":5},{"socket":0,"channel":3,"dimm":0,"address":172,"mux":226,"mux-channel":5,"mux-control":5},{"socket":0,"channel":3,"dimm":1,"address":174,"mux":226,"mux-channel":5,"mux-control":5}]),
        )?,
    })
}
fn mj11_platform_override() -> Result<Table> {
    Ok(Table::PlatformOverride {
        scope: OverrideScope::All,
        operations: vec![
            Override::MemclkMap {
                connections: [0, 1, 2, 3, 0, 0, 0, 0],
            },
            Override::CkeTristateMap {
                connections: [0, 1, 2, 3],
            },
            Override::OdtTristateMap {
                connections: [0, 1, 2, 3],
            },
            Override::CsTristateMap {
                connections: [0, 1, 2, 3, 0, 0, 0, 0],
            },
            Override::MaxDimmsPerChannel { value: 2 },
            Override::MaxChannelsPerSocket { value: 4 },
        ],
    })
}
fn mj11_udimm_odt() -> Result<Table> {
    Ok(Table::UdimmOdt {
        rows: native(
            json!([{"ranks":[["empty"],["single"],[]],"patterns":[[0,0],[0,0],[0,2],[0,0]]},{"ranks":[["empty"],["dual"],[]],"patterns":[[0,0],[0,0],[0,2],[0,8]]},{"ranks":[["single"],["empty"],[]],"patterns":[[0,1],[0,0],[0,0],[0,0]]},{"ranks":[["dual"],["empty"],[]],"patterns":[[0,1],[0,4],[0,0],[0,0]]},{"ranks":[["single"],["single"],[]],"patterns":[[2,2],[0,0],[1,1],[0,0]]},{"ranks":[["single"],["dual"],[]],"patterns":[[2,2],[0,0],[1,1],[1,1]]},{"ranks":[["dual"],["single"],[]],"patterns":[[2,2],[2,2],[1,1],[0,0]]},{"ranks":[["dual"],["dual"],[]],"patterns":[[2,2],[2,2],[1,1],[1,1]]}]),
        )?,
    })
}
fn mj11_udimm_cad() -> Result<Table> {
    Ok(Table::UdimmCad {
        rows: native(
            json!([{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":0,"slow_mode":0,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":3538944,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1333":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":0,"slow_mode":0,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":0,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr2667":true,"ddr2933":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":1,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr3200":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":1,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"single_rank":true},"dimm_slots_per_channel":2,"gear_down_mode":0,"slow_mode":0,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":3538944,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1333":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"dual_rank":true},"dimm_slots_per_channel":2,"gear_down_mode":0,"slow_mode":0,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"gear_down_mode":0,"slow_mode":0,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"20 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"dual_rank":true,"single_rank":true},"dimm_slots_per_channel":2,"gear_down_mode":0,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":3538944,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1333":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"gear_down_mode":0,"slow_mode":0,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"dual_rank":true},"dimm_slots_per_channel":2,"gear_down_mode":0,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"gear_down_mode":0,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"20 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr2400":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"single_rank":true},"dimm_slots_per_channel":2,"gear_down_mode":0,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr2667":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"dual_rank":true,"single_rank":true},"dimm_slots_per_channel":2,"gear_down_mode":1,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr2667":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"gear_down_mode":1,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr2933":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"single_rank":true},"dimm_slots_per_channel":2,"gear_down_mode":1,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr2933":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"gear_down_mode":1,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}}]),
        )?,
    })
}
fn mj11_legacy_udimm_data() -> Result<Table> {
    Ok(Table::LegacyUdimmData {
        rows: native(
            json!([{"ddr_rates":{"ddr1333":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"dq_drive_strength":30,"dqs_drive_strength":30,"odt_drive_strength":10,"rtt_nom":"80 Ω","rtt_park":"Off","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true,"ddr2933":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":14,"rtt_nom":"48 Ω","rtt_park":"Off","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr3200":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":14,"rtt_nom":"48 Ω","rtt_park":"Off","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"dual_rank":true,"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":30,"dqs_drive_strength":30,"odt_drive_strength":10,"rtt_nom":"80 Ω","rtt_park":"Off","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"dq_drive_strength":30,"dqs_drive_strength":30,"odt_drive_strength":10,"rtt_nom":"80 Ω","rtt_park":"Off","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"dual_rank":true,"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":59,"dqs_drive_strength":59,"odt_drive_strength":10,"rtt_nom":"40 Ω","rtt_park":"Off","rtt_wr":"120 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"dual_rank":true,"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":14,"rtt_nom":"48 Ω","rtt_park":"Off","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":14,"rtt_nom":"48 Ω","rtt_park":"Off","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"dual_rank":true,"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":59,"dqs_drive_strength":59,"odt_drive_strength":14,"rtt_nom":"34 Ω","rtt_park":"Off","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr2400":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":59,"dqs_drive_strength":59,"odt_drive_strength":14,"rtt_nom":"34 Ω","rtt_park":"Off","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr2933":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":14,"rtt_nom":"48 Ω","rtt_park":"Off","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr2933":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":14,"rtt_nom":"48 Ω","rtt_park":"Off","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}}]),
        )?,
    })
}
fn mj11_udimm_max_frequency() -> Result<Table> {
    Ok(Table::UdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr2933"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2400"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr1866"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr1866"}]),
        )?,
    })
}
fn mj11_udimm_stretch_frequency() -> Result<Table> {
    Ok(Table::UdimmStretchFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr2400"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2133"}]),
        )?,
    })
}
fn mj11_rdimm_odt() -> Result<Table> {
    Ok(Table::RdimmOdt {
        rows: native(
            json!([{"ranks":[["empty"],["single"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["empty"],["dual"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["single"],["empty"],[]],"patterns":[[0,1],[0,0],[0,0],[0,0]]},{"ranks":[["dual"],["empty"],[]],"patterns":[[0,4],[0,1],[0,0],[0,0]]},{"ranks":[["single"],["single"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["single"],["dual"],[]],"patterns":[[2,2],[0,0],[1,1],[1,1]]},{"ranks":[["dual"],["single"],[]],"patterns":[[2,2],[2,2],[1,1],[0,0]]},{"ranks":[["dual"],["dual"],[]],"patterns":[[10,10],[10,10],[5,5],[5,5]]}]),
        )?,
    })
}
fn mj11_rdimm_cad() -> Result<Table> {
    Ok(Table::RdimmCad {
        rows: native(
            json!([{"slots":[1],"rates":[1333],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3815994,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[1600],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3750201,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[1866],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3618615,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2133],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3487029,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2400],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3355443,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2667],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3223857,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[2933],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3092271,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[1],"rates":[3200],"rank0":["single"],"rank1":["empty"],"addr-cmd":2960685,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1333],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3815994,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1333],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3815994,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1333],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":3684922,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1600],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3750201,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1600],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3750201,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1600],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":3488057,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3618615,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3618615,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["single"],"rank1":["single"],"addr-cmd":3356985,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["single"],"rank1":["dual"],"addr-cmd":3356471,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[1866],"rank0":["dual"],"rank1":["single","dual"],"addr-cmd":3356471,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2133],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3487029,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2133],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3487029,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2133],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":3224885,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2400],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3355443,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2400],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3355443,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2400],"rank0":["single"],"rank1":["single"],"addr-cmd":3093299,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2400],"rank0":["single","dual"],"rank1":["single","dual"],"addr-cmd":3093299,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2667],"rank0":["empty"],"rank1":["single","dual"],"addr-cmd":3223857,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2667],"rank0":["single","dual"],"rank1":["empty"],"addr-cmd":3223857,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2933],"rank0":["empty"],"rank1":["single"],"addr-cmd":3092271,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]},{"slots":[2],"rates":[2933],"rank0":["single"],"rank1":["empty"],"addr-cmd":3092271,"drive":["30 Ω","30 Ω","30 Ω","30 Ω"]}]),
        )?,
    })
}
fn mj11_legacy_rdimm_data() -> Result<Table> {
    Ok(Table::LegacyRdimmData {
        rows: native(
            json!([{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true,"ddr2933":true,"ddr3200":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"48 Ω","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true,"ddr2933":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"dq_drive_strength":30,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"240 Ω","rtt_park":"Off","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true,"ddr2933":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"48 Ω","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"dual_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"240 Ω","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true,"ddr2933":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"48 Ω","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":30,"dqs_drive_strength":30,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"34 Ω","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr2400":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":58,"dqs_drive_strength":30,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"34 Ω","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"dual_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"34 Ω","rtt_park":"Off","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"240 Ω","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"single_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"34 Ω","rtt_park":"Off","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"dual_rank":true},"dimm_slots_per_channel":2,"dq_drive_strength":58,"dqs_drive_strength":30,"odt_drive_strength":24,"rtt_nom":"60 Ω","rtt_park":"240 Ω","rtt_wr":"120 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}}]),
        )?,
    })
}
fn mj11_rdimm_max_frequency() -> Result<Table> {
    Ok(Table::RdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr2933"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2400"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr1866"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2133"}]),
        )?,
    })
}
fn mj11_rdimm_stretch_frequency() -> Result<Table> {
    Ok(Table::RdimmStretchFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr2933"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr2400"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2133"}]),
        )?,
    })
}
fn mj11_legacy_console() -> Result<Table> {
    Ok(Table::LegacyConsole {
        settings: native(
            json!({"abl_breakpoint_control":{"break_on_all_dies":false,"enable_breakpoint":false},"abl_console_out_control":{"abl_console_port":128,"enable_console_logging":false,"enable_mem_flow_logging":true,"enable_mem_getreg_logging":false,"enable_mem_pmu_logging":false,"enable_mem_pmu_sram_read_logging":false,"enable_mem_pmu_sram_write_logging":false,"enable_mem_setreg_logging":true,"enable_mem_status_logging":false,"enable_mem_test_verbose_logging":false}}),
        )?,
    })
}
fn mj11_legacy_error_output() -> Result<Table> {
    Ok(Table::LegacyErrorOutput {
        settings: native(
            json!({"beep_code_table":[{"custom_error_type":"General","peak_attr":{"peak_count":8,"pulse_width":0,"repeat_count":0},"peak_map":1},{"custom_error_type":"Memory","peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0},"peak_map":2},{"custom_error_type":"Df","peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0},"peak_map":3},{"custom_error_type":"Ccx","peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0},"peak_map":4},{"custom_error_type":"Gnb","peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0},"peak_map":5},{"custom_error_type":"Psp","peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0},"peak_map":6},{"custom_error_type":"Smu","peak_attr":{"peak_count":20,"pulse_width":0,"repeat_count":0},"peak_map":7},{"custom_error_type":"Unknown","peak_attr":{"peak_count":4,"pulse_width":0,"repeat_count":0},"peak_map":2}],"clear_acknowledgement":false,"enable_error_reporting":false,"enable_error_reporting_beep_codes":false,"enable_error_reporting_gpio":false,"enable_heart_beat":false,"enable_using_handshake":false,"error_reporting_gpio":{"bank_control":192,"iomux_control":1,"pin":85},"input_port":132,"input_port_size":"32 Bit","input_port_type":"FchHtIo","output_delay":15000,"output_port":128,"output_port_size":"32 Bit","output_port_type":"FchHtIo","stop_on_first_fatal_error":false}),
        )?,
    })
}
fn mj11_voltage_control() -> Result<Table> {
    Ok(Table::VoltageControl {
        settings: native(
            json!({"clear_acknowledgement":false,"enabled":true,"input_port":132,"input_port_size":"32 Bit","input_port_type":"FchHtIo","output_port":128,"output_port_size":"32 Bit","output_port_type":"FchHtIo"}),
        )?,
    })
}
fn mj11_lrdimm_odt() -> Result<Table> {
    Ok(Table::LrdimmOdt {
        rows: native(
            json!([{"ranks":[["empty"],["lr"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["lr"],["empty"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["lr"],["lr"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]}]),
        )?,
    })
}
fn mj11_lrdimm_cad() -> Result<Table> {
    Ok(Table::LrdimmCad {
        rows: native(json!([]))?,
    })
}
fn mj11_legacy_lrdimm_data() -> Result<Table> {
    Ok(Table::LegacyLrdimmData {
        rows: native(
            json!([{"ddr_rates":{"ddr1333":true},"dimm0_ranks":{"lr":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"48 Ω","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true,"ddr2933":true,"ddr3200":true},"dimm0_ranks":{"lr":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"48 Ω","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"lr":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"48 Ω","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1333":true},"dimm0_ranks":{"lr":true},"dimm1_ranks":{"lr":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"34 Ω","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true,"ddr2667":true,"ddr2933":true},"dimm0_ranks":{"unpopulated":true},"dimm1_ranks":{"lr":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"48 Ω","rtt_wr":"Off","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"lr":true},"dimm1_ranks":{"lr":true},"dimm_slots_per_channel":2,"dq_drive_strength":63,"dqs_drive_strength":63,"odt_drive_strength":24,"rtt_nom":"Off","rtt_park":"34 Ω","rtt_wr":"80 Ω","vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}}]),
        )?,
    })
}
fn mj11_lrdimm_max_frequency() -> Result<Table> {
    Ok(Table::LrdimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr2667"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[1],"conditions":[1,0,0,1],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,1,0,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,0,1,0],"speed":"Ddr2667"},{"slots":[2],"conditions":[1,0,0,1],"speed":"Ddr2667"},{"slots":[2],"conditions":[2,2,0,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,1,1,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,0,2,0],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,0,0,2],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,1,0,1],"speed":"Ddr2133"},{"slots":[2],"conditions":[2,0,1,1],"speed":"Ddr2133"}]),
        )?,
    })
}
fn mj11_empty_ppr() -> Result<Table> {
    Ok(Table::EmptyPpr { slots: 0 })
}
fn mj11_gnb_override() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Gnb,
        layer: ParameterLayer::Override,
        values: vec![
            Setting {
                token: amd_apcb::ParameterTokenConfig::GnbBmcSocketNumber,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::GnbBmcStartLane,
                width: Width::U8,
                value: 21,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::GnbBmcEndLane,
                width: Width::U8,
                value: 21,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::GnbBmcDevice,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::GnbBmcFunction,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::GnbPcieResetControl,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
fn mj11_fch_override() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Fch,
        layer: ParameterLayer::Override,
        values: vec![
            Setting {
                token: amd_apcb::ParameterTokenConfig::FchConsoleOutEnable,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::FchConsoleOutSerialPort,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
fn mj11_cbs_default() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Cbs,
        layer: ParameterLayer::Default,
        values: vec![
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs1c,
                width: Width::U32,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs5b,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs22,
                width: Width::U8,
                value: 57,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs26,
                width: Width::U8,
                value: 26,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs2a,
                width: Width::U8,
                value: 18,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs2c,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs2e,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs30,
                width: Width::U8,
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs32,
                width: Width::U16,
                value: 312,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs34,
                width: Width::U16,
                value: 192,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs36,
                width: Width::U16,
                value: 132,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs37,
                width: Width::U8,
                value: 5,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs51,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs52,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs53,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs67,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs1d,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs1e,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs1f,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs4a,
                width: Width::U32,
                value: 700,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs62,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs60,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
fn mj11_cbs_debug() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Cbs,
        layer: ParameterLayer::Debug,
        values: vec![
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs28,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs29,
                width: Width::U8,
                value: 255,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsaf,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsac,
                width: Width::U8,
                value: 11,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs17,
                width: Width::U8,
                value: 48,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs1e,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs1f,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs20,
                width: Width::U8,
                value: 6,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs21,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs22,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs37,
                width: Width::U8,
                value: 29,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs43,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs46,
                width: Width::U16,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs48,
                width: Width::U16,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs4a,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs4c,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs4e,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs2f,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs31,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs33,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs3b,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs3d,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs3f,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs41,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs54,
                width: Width::U8,
                value: 8,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs55,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs58,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs59,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs5c,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsc3,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsc5,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs63,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs66,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs67,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs6f,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs70,
                width: Width::U8,
                value: 8,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs71,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs72,
                width: Width::U8,
                value: 8,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs9f,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsa0,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsa1,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs7c,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs7e,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs80,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs84,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsc0,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsa7,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsa9,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsab,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsb1,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsb4,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsb5,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsb7,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsb8,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsba,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsbb,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsbd,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbsbe,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs95,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs96,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs8f,
                width: Width::U32,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
pub(super) fn mj11(ec0: bool) -> Result<Definition> {
    let tables = BTreeMap::from([
        ("psp-override".into(), mj11_psp_override()?),
        ("ccx-override".into(), mj11_ccx_override()?),
        ("df-override".into(), mj11_df_override()?),
        ("memory-override".into(), mj11_memory_override()?),
        ("spd-smbus".into(), mj11_spd_smbus()?),
        ("platform-override".into(), mj11_platform_override()?),
        ("udimm-odt".into(), mj11_udimm_odt()?),
        ("udimm-cad".into(), mj11_udimm_cad()?),
        ("legacy-udimm-data".into(), mj11_legacy_udimm_data()?),
        ("udimm-max-frequency".into(), mj11_udimm_max_frequency()?),
        (
            "udimm-stretch-frequency".into(),
            mj11_udimm_stretch_frequency()?,
        ),
        ("rdimm-odt".into(), mj11_rdimm_odt()?),
        ("rdimm-cad".into(), mj11_rdimm_cad()?),
        ("legacy-rdimm-data".into(), mj11_legacy_rdimm_data()?),
        ("rdimm-max-frequency".into(), mj11_rdimm_max_frequency()?),
        (
            "rdimm-stretch-frequency".into(),
            mj11_rdimm_stretch_frequency()?,
        ),
        ("legacy-console".into(), mj11_legacy_console()?),
        ("legacy-error-output".into(), mj11_legacy_error_output()?),
        ("voltage-control".into(), mj11_voltage_control()?),
        ("lrdimm-odt".into(), mj11_lrdimm_odt()?),
        ("lrdimm-cad".into(), mj11_lrdimm_cad()?),
        ("legacy-lrdimm-data".into(), mj11_legacy_lrdimm_data()?),
        ("lrdimm-max-frequency".into(), mj11_lrdimm_max_frequency()?),
        ("empty-ppr".into(), mj11_empty_ppr()?),
        ("gnb-override".into(), mj11_gnb_override()?),
        ("fch-override".into(), mj11_fch_override()?),
        ("cbs-default".into(), mj11_cbs_default()?),
        ("cbs-debug".into(), mj11_cbs_debug()?),
    ]);
    let mut blob = Blob {
        name: "board".into(),
        revision: 7853,
        donor_sha256: "fa1f884641edd40ba9bc8a6f327b03d71fd0d12edb80161cd284cffa28a22bc9".into(),
        bindings: vec![
            Binding {
                slot: Slot::Active,
                instance: 0,
            },
            Binding {
                slot: Slot::Backup,
                instance: 0,
            },
        ],
        groups: vec![
            Group::Psp,
            Group::Ccx,
            Group::Df,
            Group::Memory,
            Group::Gnb,
            Group::Fch,
            Group::Cbs,
        ],
        records: vec![
            Reference {
                name: "psp-override".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "ccx-override".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "df-override".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "memory-override".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "spd-smbus".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "platform-override".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "udimm-odt".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "udimm-cad".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "legacy-udimm-data".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "udimm-max-frequency".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "udimm-stretch-frequency".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "rdimm-odt".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "rdimm-cad".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "legacy-rdimm-data".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "rdimm-max-frequency".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "rdimm-stretch-frequency".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "legacy-console".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "legacy-error-output".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "voltage-control".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "lrdimm-odt".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "lrdimm-cad".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "legacy-lrdimm-data".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "lrdimm-max-frequency".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "empty-ppr".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "gnb-override".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "fch-override".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "cbs-default".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "cbs-default".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "cbs-debug".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "cbs-debug".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
        ],
        tokens: vec![],
    };
    if ec0 {
        blob.revision = 9205;
        blob.donor_sha256 =
            "aeb727e324f1d51c586e742ceedaae8d41e4ad64c410207d9e8d04f3f12492d0".into();
    }
    Ok(Definition {
        generation: Generation::Zeppelin,
        blobs: vec![blob],
        tables,
        token_sets: BTreeMap::from([]),
        spd_images: BTreeMap::from([]),
    })
}
