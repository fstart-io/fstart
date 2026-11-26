// SPDX-License-Identifier: MIT OR Apache-2.0
//! Register placements used by native nct6687d_hwm.c, from nct6687d_hwm.h.
//! Fan arguments are ZERO-based array indices (native macros take 1..=10).

pub const HWM_CONFIG: u16 = 0x0180;
pub const SMBUS_MASTER_CFG1: u16 = 0x0460;
pub const SMBUS_MASTER_CFG2: u16 = 0x0461;
pub const SMBUS_MASTER_BAUD_RATE_SEL: u16 = 0x0462;
pub const PCH_THERMAL_DATA_CFG: u16 = 0x0410;
pub const PCH_DEVICE_ADDR: u16 = 0x0411;
pub const PCH_THERMAL_CMD: u16 = 0x0412;
pub const PECI_CFG: u16 = 0x0560;
pub const PECI_AGENT_EN: u16 = 0x0561;
pub const CHIP_ID0: u16 = 0x0600;
pub const CHIP_ID1: u16 = 0x0601;
pub const CUSTOMER_ID0: u16 = 0x0602;
pub const CUSTOMER_ID1: u16 = 0x0603;
pub const FW_BUILD_YEAR: u16 = 0x0604;
pub const FW_BUILD_MONTH: u16 = 0x0605;
pub const FW_BUILD_DAY: u16 = 0x0606;
pub const FW_BUILD_SERIALNUM: u16 = 0x0607;
pub const FW_VER0: u16 = 0x0608;
pub const FW_VER1: u16 = 0x0609;
pub const PROFILE_VER: u16 = 0x060a;
pub const ROM_VER0: u16 = 0x060c;
pub const ISP_BUILD_YEAR: u16 = 0x0612;
pub const ISP_BUILD_MONTH: u16 = 0x0613;
pub const ISP_BUILD_DAY: u16 = 0x0614;
pub const ISP_BUILD_SERIALNUM: u16 = 0x0615;
pub const ISP_VER0: u16 = 0x0616;
pub const ISP_VER1: u16 = 0x0617;
pub const OEM_VER: u16 = 0x0618;
pub const FAN_CFG_CTRL: u16 = 0x0a01;
pub const FAN_LOW_RPM_SPEED_BOUNDARY_HI: u16 = 0x0a48;
pub const FAN_LOW_RPM_SPEED_BOUNDARY_LO: u16 = 0x0a49;
pub const FAN_HIGH_RPM_SPEED_BOUNDARY_HI: u16 = 0x0a4a;
pub const FAN_HIGH_RPM_SPEED_BOUNDARY_LO: u16 = 0x0a4b;
pub const FAN_LOW_RPM_TOLERANCE: u16 = 0x0a4c;
pub const FAN_TRACKING_STEP: u16 = 0x0a4f;
pub const FAN_FAST_TRACK_TEMP_BOUNDARY: u16 = 0x0a50;
pub const FAN_LOW_RPM_FAST_TRACK_WEIGHT: u16 = 0x0a51;
pub const FAN_LOW_RPM_FAST_TRACK_DUTY_STEP: u16 = 0x0a54;
pub const FAN_MARKUP_TRACK_AMB_TEMP_BOUNDARY: u16 = 0x0a57;
pub const FAN_MARKUP_TRACK_WEIGHT: u16 = 0x0a58;
pub const DTS2_TARGET_MARGIN: u16 = 0x0a59;
pub const DTS2_TCONTROL: u16 = 0x0a5a;
pub const DTS2_TCONTROL_OFFSET: u16 = 0x0a5b;
pub const DTS2_TARGET_TOLERANCE: u16 = 0x0a5c;
pub const DTS2_DIVISOR: u16 = 0x0a5d;
pub const DTS2_STEP_SPEED_HI: u16 = 0x0a5e;
pub const DTS2_STEP_SPEED_LO: u16 = 0x0a5f;
pub const DTS2_MIN_SPEED_HI: u16 = 0x0a60;
pub const DTS2_MIN_SPEED_LO: u16 = 0x0a61;
pub const DTS2_DELAY_TIME_COUNTER: u16 = 0x0a62;
pub const AMBIENT_FLOOR_TEMP_START_POINT: u16 = 0x0a77;
pub const AMBIENT_FLOOR_TEMP_END_POINT: u16 = 0x0a78;
pub const FAN_ENGINE_STS: u16 = 0x0cf8;
pub const FAN_LAST_ERROR_CODE: u16 = 0x0cf9;
pub const FAN_DEFAULT_VAL: u16 = 0x0cfd;

pub const fn sensor_cfg(n: u16) -> u16 {
    0x01a0 + n
}
pub const fn fanin_cfg(n: u16) -> u16 {
    0x01c0 + n
}
pub const fn fanout_cfg(n: u16) -> u16 {
    if n >= 8 { 0x08f4 + n - 8 } else { 0x01d0 + n }
}
pub const fn fan_step_up_time(n: u16) -> u16 {
    0x081a + n
}
pub const fn fan_step_down_time(n: u16) -> u16 {
    0x0824 + n
}
pub const fn fan_alg_functrl(n: u16) -> u16 {
    0x0900 + n
}
pub const fn fan_mtz_dcs_data(x: u16, n: u16) -> u16 {
    0x0910 + x + 4 * n
}
pub const fn fan_initial_value(n: u16) -> u16 {
    0x0990 + n
}
pub const fn fan_crit_temp_cfg(n: u16) -> u16 {
    0x09a0 + n
}
pub const fn fan_crit_temp_tolerance(n: u16) -> u16 {
    0x09b0 + n
}
pub const fn fan_temp_err_duty_val(n: u16) -> u16 {
    0x09c0 + n
}
pub const fn fan_mode_sel(n: u16) -> u16 {
    0x09d0 + n
}
pub const fn fan_manual_en(n: u16) -> u16 {
    if n >= 8 { 0x080f } else { 0x0a00 }
}
pub const fn fan_function_ctrl(n: u16) -> u16 {
    if n >= 8 { 0x08e0 + n - 8 } else { 0x0a08 + n }
}
pub const fn fan_amb_floor_min_out_start(n: u16) -> u16 {
    0x0a10 + n
}
pub const fn fan_amb_floor_min_out_end(n: u16) -> u16 {
    0x0a18 + n
}
pub const fn fan_amb_floor_max_out(n: u16) -> u16 {
    0x0a20 + n
}
pub const fn fan_manual_offset(n: u16) -> u16 {
    0x0a30 + n
}
pub const fn fan_min_duty(n: u16) -> u16 {
    0x0a38 + n
}
pub const fn fan_startup_duty(n: u16) -> u16 {
    0x0a40 + n
}
pub const fn fan_alg_engine_weight_en(n: u16) -> u16 {
    if n >= 8 {
        0x0ac1 + 8 * (n - 8)
    } else {
        0x0a80 + 8 * n
    }
}
pub const fn fan_alg_engine_weight_val(n: u16) -> u16 {
    if n >= 8 {
        0x0ac3 + (n - 8) / 2 + 8 * (n - 8)
    } else {
        0x0a84 + n / 2 + 8 * n
    }
}
pub const fn fan_channel_en(n: u16) -> u16 {
    if n >= 8 { 0x0cfb } else { 0x0cfc }
}
// Source TC/SC/SF4 register blocks use base 0x0b00 + 24*n. Fields below
// deliberately share placements according to the selected algorithm.
pub const fn fan_tc_target_temp_tolerance(n: u16) -> u16 {
    0x0b00 + 24 * n
}
pub const fn fan_tc_target_temp(n: u16) -> u16 {
    0x0b01 + 24 * n
}
pub const fn fan_tc_stop_value(n: u16) -> u16 {
    0x0b02 + 24 * n
}
pub const fn fan_tc_stop_time(n: u16) -> u16 {
    0x0b03 + 24 * n
}
pub const fn fan_tc_step_down_time(n: u16) -> u16 {
    0x0b04 + 24 * n
}
pub const fn fan_tc_step_up_time(n: u16) -> u16 {
    0x0b05 + 24 * n
}
pub const fn fan_sc_target_rpm_hi(n: u16) -> u16 {
    0x0b00 + 24 * n
}
pub const fn fan_sc_target_rpm_lo(n: u16) -> u16 {
    0x0b01 + 24 * n
}
pub const fn fan_sc_target_rpm_tolerance_hi(n: u16) -> u16 {
    0x0b02 + 24 * n
}
pub const fn fan_sc_target_rpm_tolerance_lo(n: u16) -> u16 {
    0x0b03 + 24 * n
}
pub const fn fan_sc_step_down_time(n: u16) -> u16 {
    0x0b04 + 24 * n
}
pub const fn fan_sc_step_up_time(n: u16) -> u16 {
    0x0b05 + 24 * n
}
pub const fn fan_sf4_temp_lvl(n: u16, level: u16) -> u16 {
    0x0b00 + 24 * n + level
}
pub const fn fan_sf4_pwm_rpm_lvl_hi(n: u16, level: u16) -> u16 {
    0x0b07 + 24 * n + 2 * level
}
pub const fn fan_sf4_pwm_rpm_lvl_lo(n: u16, level: u16) -> u16 {
    0x0b08 + 24 * n + 2 * level
}
pub const fn fan_sf4_temp_off_hysteresis(n: u16) -> u16 {
    0x0b15 + 24 * n
}
pub const fn fan_sf4_temp_cut_off(n: u16) -> u16 {
    0x0b16 + 24 * n
}
pub const fn fan_sf4_temp_off_delay(n: u16) -> u16 {
    0x0b17 + 24 * n
}
