// SPDX-License-Identifier: MIT OR Apache-2.0
// Reviewed firmware defaults. Rust owns the ABI; these are not user templates.
use super::*;
fn vilboz_board_id_gpio() -> Result<Table> {
    Ok(Table::BoardIdGpio {
        pins: native(
            json!([{"bank_control":0,"iomux_control":1,"pin":132},{"bank_control":0,"iomux_control":1,"pin":90},{"bank_control":0,"iomux_control":3,"pin":86},{"bank_control":0,"iomux_control":1,"pin":69}]),
        )?,
        mapping: native(
            json!([{"board_instance_index":0,"id_and_feature_mask":15,"id_and_feature_value":0},{"board_instance_index":1,"id_and_feature_mask":15,"id_and_feature_value":1},{"board_instance_index":2,"id_and_feature_mask":15,"id_and_feature_value":2},{"board_instance_index":3,"id_and_feature_mask":15,"id_and_feature_value":3},{"board_instance_index":4,"id_and_feature_mask":15,"id_and_feature_value":4},{"board_instance_index":5,"id_and_feature_mask":15,"id_and_feature_value":5},{"board_instance_index":6,"id_and_feature_mask":15,"id_and_feature_value":6},{"board_instance_index":7,"id_and_feature_mask":15,"id_and_feature_value":7},{"board_instance_index":8,"id_and_feature_mask":15,"id_and_feature_value":8},{"board_instance_index":9,"id_and_feature_mask":15,"id_and_feature_value":9},{"board_instance_index":10,"id_and_feature_mask":15,"id_and_feature_value":10},{"board_instance_index":11,"id_and_feature_mask":15,"id_and_feature_value":11},{"board_instance_index":12,"id_and_feature_mask":15,"id_and_feature_value":12},{"board_instance_index":13,"id_and_feature_mask":15,"id_and_feature_value":13},{"board_instance_index":14,"id_and_feature_mask":15,"id_and_feature_value":14},{"board_instance_index":15,"id_and_feature_mask":15,"id_and_feature_value":15}]),
        )?,
    })
}
fn vilboz_df_default() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Df,
        layer: ParameterLayer::Default,
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
                value: 208,
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
fn vilboz_df_override() -> Result<Table> {
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
                value: 208,
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
fn vilboz_memory_default() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Memory,
        layer: ParameterLayer::Default,
        values: vec![
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemBottomIo,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemHoleRemapping,
                width: Width::U32,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemLimitToBelow1TiB,
                width: Width::U32,
                value: 1200,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUserTimingMode,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemClockValue,
                width: Width::U8,
                value: 0,
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableParity,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableBankSwizzle,
                width: Width::U32,
                value: 134217728,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableClearing,
                width: Width::U8,
                value: 1,
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
                width: Width::U8,
                value: 3,
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
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemIsCapsuleMode,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemForceTraining,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmTypeMixedConfig,
                width: Width::U16,
                value: 0,
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
                width: Width::U32,
                value: 16777152,
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
                width: Width::U32,
                value: 1600,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubL2Rate,
                width: Width::U32,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubL3Rate,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubInstructionCacheRate,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubDataCacheRate,
                width: Width::U8,
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
                width: Width::U8,
                value: 0,
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUmaAlignment,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableAllClocks,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemBusFrequencyLimit,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemPowerDownMode,
                width: Width::U8,
                value: 2,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemIgnoreSpdChecksum,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemModeUnganged,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemQuadRankCapable,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemRdimmCapable,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemLrdimmCapable,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUdimmCapable,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemSodimmCapable,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableDoubleRefreshRate,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmTypeDdr4Capable,
                width: Width::U16,
                value: 0,
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorUpper,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorLower,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorCritical,
                width: Width::U32,
                value: 30,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorResolution,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorConf,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemAutoRefreshFineGranMode,
                width: Width::U8,
                value: 0,
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
                token: amd_apcb::ParameterTokenConfig::MemSwCmdThrottleCycles,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemOdtsCmdThrottleCycles,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDdrRouteBalancedTee,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnablePState,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemSolderedDown,
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
        ],
    })
}
fn vilboz_memory_override() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Memory,
        layer: ParameterLayer::Override,
        values: vec![
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemBottomIo,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemHoleRemapping,
                width: Width::U32,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemLimitToBelow1TiB,
                width: Width::U32,
                value: 1200,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUserTimingMode,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemClockValue,
                width: Width::U8,
                value: 0,
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableParity,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableBankSwizzle,
                width: Width::U32,
                value: 134217728,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableClearing,
                width: Width::U8,
                value: 1,
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
                width: Width::U8,
                value: 3,
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
                value: 3,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemIsCapsuleMode,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemForceTraining,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmTypeMixedConfig,
                width: Width::U16,
                value: 0,
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
                width: Width::U32,
                value: 16777152,
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
                width: Width::U32,
                value: 1600,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubL2Rate,
                width: Width::U32,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubL3Rate,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubInstructionCacheRate,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemScrubDataCacheRate,
                width: Width::U8,
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
                width: Width::U8,
                value: 0,
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUmaAlignment,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableAllClocks,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemBusFrequencyLimit,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemPowerDownMode,
                width: Width::U8,
                value: 2,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemIgnoreSpdChecksum,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemModeUnganged,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemQuadRankCapable,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemRdimmCapable,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemLrdimmCapable,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemUdimmCapable,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemSodimmCapable,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnableDoubleRefreshRate,
                width: Width::U16,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmTypeDdr4Capable,
                width: Width::U16,
                value: 0,
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorUpper,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorLower,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorCritical,
                width: Width::U32,
                value: 30,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorResolution,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDimmSensorConf,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemAutoRefreshFineGranMode,
                width: Width::U8,
                value: 0,
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
                token: amd_apcb::ParameterTokenConfig::MemSwCmdThrottleCycles,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemOdtsCmdThrottleCycles,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemDdrRouteBalancedTee,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemEnablePState,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::MemSolderedDown,
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
        ],
    })
}
fn vilboz_spd_info() -> Result<Table> {
    Ok(Table::SpdInfo {
        slots: native(
            json!([{"descriptor":{"address":160,"channel":0,"dimm":0,"dimm_present":true,"dram_down_spd_valid":true,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":164,"channel":0,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":162,"channel":1,"dimm":0,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":166,"channel":1,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}}]),
        )?,
    })
}
fn vilboz_spd_smbus() -> Result<Table> {
    Ok(Table::SpdSmbus {
        slots: native(
            json!([{"socket":0,"channel":0,"dimm":0,"address":160,"mux":255,"mux-channel":255,"mux-control":255},{"socket":0,"channel":0,"dimm":1,"address":164,"mux":255,"mux-channel":255,"mux-control":255},{"socket":0,"channel":1,"dimm":0,"address":162,"mux":255,"mux-channel":255,"mux-control":255},{"socket":0,"channel":1,"dimm":1,"address":166,"mux":255,"mux-channel":255,"mux-control":255}]),
        )?,
    })
}
fn vilboz_platform_override() -> Result<Table> {
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
            Override::MaxDimmsPerChannel { value: 1 },
            Override::MaxChannelsPerSocket { value: 2 },
        ],
    })
}
fn vilboz_legacy_console() -> Result<Table> {
    Ok(Table::LegacyConsole {
        settings: native(
            json!({"abl_breakpoint_control":{"break_on_all_dies":false,"enable_breakpoint":false},"abl_console_out_control":{"abl_console_port":128,"enable_console_logging":false,"enable_mem_flow_logging":true,"enable_mem_getreg_logging":false,"enable_mem_pmu_logging":false,"enable_mem_pmu_sram_read_logging":false,"enable_mem_pmu_sram_write_logging":false,"enable_mem_setreg_logging":true,"enable_mem_status_logging":false,"enable_mem_test_verbose_logging":false}}),
        )?,
    })
}
fn vilboz_legacy_error_output() -> Result<Table> {
    Ok(Table::ErrorOutput {
        settings: native(
            json!({"beep_code_table":[{"custom_error_type":"General","peak_attr":{"peak_count":3,"pulse_width":3,"repeat_count":0},"peak_map":21845},{"custom_error_type":"Memory","peak_attr":{"peak_count":5,"pulse_width":3,"repeat_count":0},"peak_map":21845},{"custom_error_type":"Df","peak_attr":{"peak_count":7,"pulse_width":3,"repeat_count":0},"peak_map":21845},{"custom_error_type":"Ccx","peak_attr":{"peak_count":9,"pulse_width":3,"repeat_count":0},"peak_map":21845},{"custom_error_type":"Gnb","peak_attr":{"peak_count":11,"pulse_width":3,"repeat_count":0},"peak_map":21845},{"custom_error_type":"Psp","peak_attr":{"peak_count":13,"pulse_width":3,"repeat_count":0},"peak_map":21845},{"custom_error_type":"Smu","peak_attr":{"peak_count":15,"pulse_width":3,"repeat_count":0},"peak_map":21845},{"custom_error_type":"Unknown","peak_attr":{"peak_count":3,"pulse_width":3,"repeat_count":0},"peak_map":21845}],"clear_acknowledgement":false,"enable_error_reporting":false,"enable_error_reporting_beep_codes":false,"enable_error_reporting_gpio":false,"enable_heart_beat":false,"enable_power_good_gpio":false,"enable_using_handshake":false,"error_reporting_gpio":{"bank_control":192,"iomux_control":1,"pin":85},"input_port":132,"input_port_size":"32 Bit","input_port_type":"FchHtIo","output_delay":15000,"output_port":128,"output_port_size":"32 Bit","output_port_type":"FchHtIo","power_good_gpio":{"bank_control":0,"iomux_control":0,"pin":0},"stop_on_first_fatal_error":true}),
        )?,
    })
}
fn vilboz_voltage_control() -> Result<Table> {
    Ok(Table::VoltageControl {
        settings: native(
            json!({"clear_acknowledgement":false,"enabled":true,"input_port":132,"input_port_size":"32 Bit","input_port_type":"FchHtIo","output_port":128,"output_port_size":"32 Bit","output_port_type":"FchHtIo"}),
        )?,
    })
}
fn vilboz_sodimm_odt() -> Result<Table> {
    Ok(Table::SodimmOdt {
        rows: native(
            json!([{"ranks":[["empty"],["single"],[]],"patterns":[[0,0],[0,0],[0,2],[0,0]]},{"ranks":[["empty"],["dual"],[]],"patterns":[[0,0],[0,0],[0,2],[0,8]]},{"ranks":[["single"],["empty"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["dual"],["empty"],[]],"patterns":[[0,0],[0,0],[0,0],[0,0]]},{"ranks":[["single"],["single"],[]],"patterns":[[2,2],[0,0],[1,1],[0,0]]},{"ranks":[["single"],["dual"],[]],"patterns":[[2,2],[0,0],[1,1],[1,1]]},{"ranks":[["dual"],["single"],[]],"patterns":[[2,2],[2,2],[1,1],[0,0]]},{"ranks":[["dual"],["dual"],[]],"patterns":[[2,2],[2,2],[1,1],[1,1]]}]),
        )?,
    })
}
fn vilboz_sodimm_cad() -> Result<Table> {
    Ok(Table::SodimmCad {
        rows: native(
            json!([{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1333":true,"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":0,"slow_mode":0,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":3538944,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1333":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":0,"slow_mode":0,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr1600":true,"ddr1866":true,"ddr2133":true,"ddr2400":true},"dimm0_ranks":{"dual_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":0,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr2667":true},"dimm0_ranks":{"dual_rank":true,"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":1,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}},{"address_command_control":0,"address_command_drive_strength":"24 Ω","cke_drive_strength":"24 Ω","clk_drive_strength":"24 Ω","cs_odt_drive_strength":"24 Ω","ddr_rates":{"ddr2933":true,"ddr3200":true},"dimm0_ranks":{"single_rank":true},"dimm1_ranks":{"unpopulated":true},"dimm_slots_per_channel":1,"gear_down_mode":1,"slow_mode":1,"vdd_io":{"1.25 V":false,"1.35 V":false,"1.5 V":true}}]),
        )?,
    })
}
fn vilboz_sodimm_data() -> Result<Table> {
    Ok(Table::SodimmData {
        rows: native(
            json!([{"slots":[1],"rates":[1333],"rank0":["single"],"rank1":["empty"],"rtt-nom":"Off","rtt-wr":"Off","rtt-park":"80 Ω","drive":[30,30,10],"pmu-vref":90,"dram-vref":{"Range1":"75.60%"}},{"slots":[1],"rates":[1333],"rank0":["dual"],"rank1":["empty"],"rtt-nom":"Off","rtt-wr":"80 Ω","rtt-park":"240 Ω","drive":[30,30,10],"pmu-vref":90,"dram-vref":{"Range1":"75.60%"}},{"slots":[1],"rates":[1600,1866,2133,2400,2667],"rank0":["single"],"rank1":["empty"],"rtt-nom":"Off","rtt-wr":"Off","rtt-park":"48 Ω","drive":[63,63,24],"pmu-vref":90,"dram-vref":{"Range1":"75.60%"}},{"slots":[1],"rates":[1600,1866,2133,2400,2667],"rank0":["dual"],"rank1":["empty"],"rtt-nom":"Off","rtt-wr":"80 Ω","rtt-park":"240 Ω","drive":[63,63,24],"pmu-vref":90,"dram-vref":{"Range1":"75.60%"}},{"slots":[1],"rates":[2933,3200],"rank0":["single"],"rank1":["empty"],"rtt-nom":"Off","rtt-wr":"Off","rtt-park":"48 Ω","drive":[63,63,24],"pmu-vref":90,"dram-vref":{"Range1":"75.60%"}}]),
        )?,
    })
}
fn vilboz_sodimm_max_frequency() -> Result<Table> {
    Ok(Table::SodimmMaxFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"}]),
        )?,
    })
}
fn vilboz_sodimm_stretch_frequency() -> Result<Table> {
    Ok(Table::SodimmStretchFrequency {
        rows: native(
            json!([{"slots":[1],"conditions":[1,1,0,0],"speed":"Ddr3200"},{"slots":[1],"conditions":[1,0,1,0],"speed":"Ddr3200"}]),
        )?,
    })
}
fn vilboz_fch_default() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Fch,
        layer: ParameterLayer::Default,
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::FchSmbusSpeed,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::FchEnd,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Fch1c05,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Fch1c06,
                width: Width::U32,
                value: 115200,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Fch1c07,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
fn vilboz_fch_override() -> Result<Table> {
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
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::FchSmbusSpeed,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::FchEnd,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Fch1c05,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Fch1c06,
                width: Width::U32,
                value: 115200,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Fch1c07,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
        ],
    })
}
fn vilboz_cbs_default() -> Result<Table> {
    Ok(Table::Parameters {
        group: Group::Cbs,
        layer: ParameterLayer::Default,
        values: vec![
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs18,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs19,
                width: Width::U8,
                value: 1,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs1a,
                width: Width::U8,
                value: 0,
                time: amd_apcb::ParameterTimePoint::Any,
            },
            Setting {
                token: amd_apcb::ParameterTokenConfig::Cbs1b,
                width: Width::U32,
                value: 128,
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
        ],
    })
}
fn vilboz_cbs_debug() -> Result<Table> {
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
        ],
    })
}
fn vilboz_spd_info_2() -> Result<Table> {
    Ok(Table::SpdInfo {
        slots: native(
            json!([{"descriptor":{"address":160,"channel":0,"dimm":0,"dimm_present":true,"dram_down_spd_valid":true,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3},"image":"ram-id-2"},{"descriptor":{"address":164,"channel":0,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":162,"channel":1,"dimm":0,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":166,"channel":1,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}}]),
        )?,
    })
}
fn vilboz_spd_info_3() -> Result<Table> {
    Ok(Table::SpdInfo {
        slots: native(
            json!([{"descriptor":{"address":160,"channel":0,"dimm":0,"dimm_present":true,"dram_down_spd_valid":true,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3},"image":"ram-id-5"},{"descriptor":{"address":164,"channel":0,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":162,"channel":1,"dimm":0,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":166,"channel":1,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}}]),
        )?,
    })
}
fn vilboz_spd_info_4() -> Result<Table> {
    Ok(Table::SpdInfo {
        slots: native(
            json!([{"descriptor":{"address":160,"channel":0,"dimm":0,"dimm_present":true,"dram_down_spd_valid":true,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3},"image":"ram-id-7"},{"descriptor":{"address":164,"channel":0,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":162,"channel":1,"dimm":0,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":166,"channel":1,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}}]),
        )?,
    })
}
fn vilboz_spd_info_5() -> Result<Table> {
    Ok(Table::SpdInfo {
        slots: native(
            json!([{"descriptor":{"address":160,"channel":0,"dimm":0,"dimm_present":true,"dram_down_spd_valid":true,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3},"image":"ram-id-8"},{"descriptor":{"address":164,"channel":0,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":162,"channel":1,"dimm":0,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}},{"descriptor":{"address":166,"channel":1,"dimm":1,"dimm_present":false,"dram_down_spd_valid":false,"dram_manufacturer_id":0,"module_manufacturer_id":0,"mux_address":0,"mux_channel":0,"mux_present":false,"nvdimm_present":false,"package":1,"page_address":0,"shadow_spd_valid":false,"socket":0,"technology":3}}]),
        )?,
    })
}
pub(super) fn vilboz() -> Result<Definition> {
    let tables = BTreeMap::from([
        ("board-id-gpio".into(), vilboz_board_id_gpio()?),
        ("df-default".into(), vilboz_df_default()?),
        ("df-override".into(), vilboz_df_override()?),
        ("memory-default".into(), vilboz_memory_default()?),
        ("memory-override".into(), vilboz_memory_override()?),
        ("spd-info".into(), vilboz_spd_info()?),
        ("spd-smbus".into(), vilboz_spd_smbus()?),
        ("platform-override".into(), vilboz_platform_override()?),
        ("legacy-console".into(), vilboz_legacy_console()?),
        ("legacy-error-output".into(), vilboz_legacy_error_output()?),
        ("voltage-control".into(), vilboz_voltage_control()?),
        ("sodimm-odt".into(), vilboz_sodimm_odt()?),
        ("sodimm-cad".into(), vilboz_sodimm_cad()?),
        ("sodimm-data".into(), vilboz_sodimm_data()?),
        (
            "sodimm-max-frequency".into(),
            vilboz_sodimm_max_frequency()?,
        ),
        (
            "sodimm-stretch-frequency".into(),
            vilboz_sodimm_stretch_frequency()?,
        ),
        ("fch-default".into(), vilboz_fch_default()?),
        ("fch-override".into(), vilboz_fch_override()?),
        ("cbs-default".into(), vilboz_cbs_default()?),
        ("cbs-debug".into(), vilboz_cbs_debug()?),
        ("spd-info-2".into(), vilboz_spd_info_2()?),
        ("spd-info-3".into(), vilboz_spd_info_3()?),
        ("spd-info-4".into(), vilboz_spd_info_4()?),
        ("spd-info-5".into(), vilboz_spd_info_5()?),
    ]);
    let base = Blob {
        name: "ram-id-0".into(),
        revision: 717223047,
        donor_sha256: "142412100b3d54382a3dbee2e4eed65eccb6aba55ff9780d357298e824d6fba5".into(),
        bindings: vec![
            Binding {
                slot: Slot::Backup,
                instance: 0,
            },
            Binding {
                slot: Slot::Active,
                instance: 0,
            },
        ],
        groups: vec![Group::Psp, Group::Df, Group::Memory, Group::Fch, Group::Cbs],
        records: vec![
            Reference {
                name: "board-id-gpio".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "df-default".into(),
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
                name: "memory-default".into(),
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
                name: "spd-info".into(),
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
                name: "sodimm-odt".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "sodimm-cad".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "sodimm-data".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "sodimm-max-frequency".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "sodimm-stretch-frequency".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
            Reference {
                name: "fch-default".into(),
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
                name: "cbs-debug".into(),
                instance: 0,
                board_mask: None,
                priority: None,
            },
        ],
        tokens: vec![],
    };
    let variants = [
        (
            0u8,
            "spd-info",
            "142412100b3d54382a3dbee2e4eed65eccb6aba55ff9780d357298e824d6fba5",
        ),
        (
            1u8,
            "spd-info",
            "142412100b3d54382a3dbee2e4eed65eccb6aba55ff9780d357298e824d6fba5",
        ),
        (
            2u8,
            "spd-info-2",
            "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        ),
        (
            3u8,
            "spd-info-2",
            "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        ),
        (
            4u8,
            "spd-info-2",
            "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        ),
        (
            5u8,
            "spd-info-3",
            "90c47cd6f665e004988ece4dd620586ff66948719b9f896913080f59e1131663",
        ),
        (
            6u8,
            "spd-info-3",
            "90c47cd6f665e004988ece4dd620586ff66948719b9f896913080f59e1131663",
        ),
        (
            7u8,
            "spd-info-4",
            "5ccda7dcba6668369b6b8e0b99dab7990682a62380f3d606b03f5e150119c309",
        ),
        (
            8u8,
            "spd-info-5",
            "a70492cf9c71464e13ffc9332f4e08c078bdaaca6b6756a415c3765659947d3b",
        ),
        (
            9u8,
            "spd-info-5",
            "a70492cf9c71464e13ffc9332f4e08c078bdaaca6b6756a415c3765659947d3b",
        ),
        (
            10u8,
            "spd-info-2",
            "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        ),
        (
            11u8,
            "spd-info-2",
            "98f61312c86f37c167e8e19eec96a791872d86daa986c326ec9ba75764992ae1",
        ),
        (
            12u8,
            "spd-info-5",
            "a70492cf9c71464e13ffc9332f4e08c078bdaaca6b6756a415c3765659947d3b",
        ),
    ];
    let blobs = variants
        .into_iter()
        .map(|(id, spd, pin)| {
            let mut b = base.clone();
            b.name = format!("ram-id-{id}");
            b.donor_sha256 = pin.into();
            for binding in &mut b.bindings {
                binding.instance = id;
            }
            for record in &mut b.records {
                if record.name == "spd-info" {
                    record.name = spd.into();
                }
            }
            b
        })
        .collect();
    Ok(Definition {
        generation: Generation::Picasso,
        blobs,
        tables,
        token_sets: BTreeMap::from([]),
        spd_images: BTreeMap::from([
            (
                "ram-id-2".into(),
                r#"23 11 0c 03 45 21 00 08 00 00 00 00 02 03 00 00
00 00 05 06 f8 ff 02 00 6e 6e 6e 11 00 6e f0 0a
20 08 00 05 00 f0 2b 34 28 00 78 00 14 3c 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 9c b5 00 00 00 00 bc 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
"#
                .into(),
            ),
            (
                "ram-id-5".into(),
                r#"23 11 0c 03 46 29 00 08 00 00 00 00 02 03 00 00
00 00 05 06 f8 ff 02 00 6e 6e 6e 11 00 6e f0 0a
20 08 00 05 00 f0 2b 34 28 00 78 00 14 3c 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 9c b5 00 00 00 00 bc 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
"#
                .into(),
            ),
            (
                "ram-id-7".into(),
                r#"23 11 0c 03 85 21 91 08 00 00 00 00 01 03 00 00
00 00 05 06 f8 ff 02 00 6e 6e 6e 11 00 6e f0 0a
20 08 00 05 00 f0 2b 34 28 00 78 00 14 3c 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 9c b5 00 00 00 00 bc 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
"#
                .into(),
            ),
            (
                "ram-id-8".into(),
                r#"23 11 0c 03 46 29 00 08 00 00 00 00 02 03 00 00
00 00 05 06 f8 ff 02 00 6e 6e 6e 11 00 6e 30 11
f0 0a 20 08 00 f0 2b 34 28 00 78 00 14 3c 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 9c b5 00 00 00 00 bc 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00
"#
                .into(),
            ),
        ]),
    })
}
