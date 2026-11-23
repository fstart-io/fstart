// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native NCT6687D HWM configuration, translated from coreboot's
//! nct6687d_hwm.c/chip.h.
//!
//! Caller exclusively owns firmware port zero, serializes SMM/other agents,
//! and has already enabled the EC decode and board multifunction pins. This
//! API configures the existing EC firmware; it does not upload it, prove PECI
//! readiness, measure temperatures, or establish physical thermal safety.
//! Errors stop initialization without manual/fan-off fallback. After a failed
//! handshake, hardware state is uncertain and the caller must not proceed.

use super::ec::Ec;
use embedded_hal::delay::DelayNs;
use tock_registers::{
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

mod config;
mod registers;
pub use config::*;
use registers::*;

register_bitfields![u8,
    HwmConfig [
        ENABLE OFFSET(7) NUMBITS(1) [],
        LOCK_SENSORS OFFSET(5) NUMBITS(1) []
    ],
    EngineStatus [
        PHASE OFFSET(3) NUMBITS(1) [],
        INVALID OFFSET(4) NUMBITS(1) [],
        CHECK_DONE OFFSET(5) NUMBITS(1) [],
        LOCK OFFSET(6) NUMBITS(1) []
    ],
    ConfigControl [
        DONE OFFSET(6) NUMBITS(1) [],
        REQUEST OFFSET(7) NUMBITS(1) []
    ],
    PeciConfig [
        SPEED OFFSET(0) NUMBITS(2) [],
        INIT OFFSET(6) NUMBITS(1) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    PeciAgents [ ENABLE OFFSET(0) NUMBITS(4) [] ],
    PchConfig [
        BAUD OFFSET(0) NUMBITS(3) [],
        PORT OFFSET(4) NUMBITS(2) [],
        ONE_BYTE OFFSET(7) NUMBITS(1) []
    ],
    SmbusConfig [ ENABLE OFFSET(7) NUMBITS(1) [] ],
    FanMode [ MODE OFFSET(0) NUMBITS(4) [] ],
    FanPin [
        SELECT OFFSET(0) NUMBITS(5) [],
        ENABLE OFFSET(7) NUMBITS(1) []
    ],
    FanFunction [
        MIN_DUTY OFFSET(0) NUMBITS(1) [],
        MANUAL_OFFSET OFFSET(1) NUMBITS(1) [],
        STARTUP OFFSET(2) NUMBITS(1) [],
        AMBIENT OFFSET(3) NUMBITS(1) [],
        MARKUP OFFSET(4) NUMBITS(1) [],
        FAST OFFSET(5) NUMBITS(1) [],
        TRACKING OFFSET(6) NUMBITS(1) [],
        RPM OFFSET(7) NUMBITS(1) []
    ],
    FanAlgorithm [
        DTS_UB OFFSET(2) NUMBITS(1) [],
        DTS2 OFFSET(3) NUMBITS(1) [],
        TEMP_ERROR OFFSET(5) NUMBITS(1) [],
        CRITICAL OFFSET(6) NUMBITS(1) [],
        WEIGHT OFFSET(7) NUMBITS(1) []
    ],
    FanWeight [
        LOW OFFSET(0) NUMBITS(4) [],
        HIGH OFFSET(4) NUMBITS(4) []
    ],
    ChannelBits [ BITS OFFSET(0) NUMBITS(8) [] ],
    ThermalTarget [ KEEP_MIN OFFSET(7) NUMBITS(1) [] ]
];

const POLL_COUNT: usize = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitPhase {
    ExitConfiguration,
    Unlock,
    ConfigurationCheck,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidSensor { index: u8 },
    InvalidPeciSpeed,
    InvalidSmbusSensor,
    InvalidFan { channel: u8 },
    InvalidTracking,
    UnsupportedManual,
    UnsupportedDts1,
    UnsupportedPid,
    Timeout(WaitPhase),
    ConfigurationRejected { status: u8, code: u8 },
    ConfigurationNotLocked { status: u8 },
    ConfigurationNotOpen { status: u8 },
}

/// Raw source identification/firmware information; no expected-ID or readiness
/// assertion is made. Source words at CHIP_ID0/CUSTOMER_ID0 are little endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EcInfo {
    pub chip_id: u16,
    pub customer_id: u16,
    pub firmware: [u8; 2],
    pub profile: u8,
    pub rom: [u8; 4],
    pub build: [u8; 4],
    pub isp_build: [u8; 4],
    pub isp: [u8; 2],
    pub oem: [u8; 8],
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InitReport {
    pub info: EcInfo,
    pub engine_status: u8,
}

/// Validate the ENTIRE deterministic configuration before any EC transaction.
/// Zero/ignored entries retain native conditional-write semantics, not a reset
/// of inherited settings. A caller needing a known clean state must establish
/// that prerequisite separately; applying this twice is not a reset operation.
pub fn validate(config: &Config) -> Result<(), Error> {
    if config.peci_speed > 3 {
        return Err(Error::InvalidPeciSpeed);
    }
    for (i, s) in config.sensors.iter().enumerate() {
        if !matches!(s.raw(), 0x00..=0x18 | 0x20..=0x2b | 0x30..=0x49 | 0x50..=0x57 | 0x60..=0x76) {
            return Err(Error::InvalidSensor { index: i as u8 });
        }
    }
    if let Some(s) = config.smbus_sensor
        && (s.index >= 32
            || !(0x30..=0x37).contains(&config.sensors[s.index as usize].raw())
            || s.address == 0
            || s.command == 0
            || s.port > 3
            || s.baud > 7)
    {
        return Err(Error::InvalidSmbusSensor);
    }
    if config.fan_default_percent > 100 {
        return Err(Error::InvalidFan { channel: 0 });
    }
    for (i, f) in config.fans.iter().enumerate() {
        let invalid = || Error::InvalidFan {
            channel: i as u8 + 1,
        };
        match f.mode {
            Mode::Ignore => continue,
            Mode::Manual => return Err(Error::UnsupportedManual),
            Mode::Pid => return Err(Error::UnsupportedPid),
            _ => {}
        }
        if f.dts1 {
            return Err(Error::UnsupportedDts1);
        }
        if f.input > 23
            || f.output > 23
            || f.weight > 15
            || f.critical_temp > 127
            || f.critical_tolerance > 15
            || (f.critical_temp != 0 && f.critical_temp < f.critical_tolerance)
            || f.startup_percent > 100
            || f.min_percent > 100
        {
            return Err(invalid());
        }
        if i >= 8
            && (f.ambient_floor
                || f.startup_percent != 0
                || f.manual_offset != 0
                || f.min_percent != 0)
        {
            return Err(invalid());
        }
        if (f.fast_tracking || f.markup_tracking) && !f.smart_tracking {
            return Err(invalid());
        }
        if f.ambient_floor && f.ambient_rpm.iter().any(|rpm| *rpm > 25599) {
            return Err(invalid());
        }
        match f.mode {
            Mode::SmartFan(s) => {
                for n in 0..7 {
                    if s.temperatures[n] > 127
                        || s.speeds[n] > if f.unit == Unit::Pwm { 100 } else { 0x3fff }
                        || (n != 0
                            && (s.temperatures[n] < s.temperatures[n - 1]
                                || s.speeds[n] < s.speeds[n - 1]))
                    {
                        return Err(invalid());
                    }
                }
                if u16::from(s.temperatures[0]) < u16::from(s.cutoff) + u16::from(s.hysteresis) {
                    return Err(invalid());
                }
            }
            Mode::ThermalCruise(t) => {
                if t.step_down == 0
                    || t.step_up == 0
                    || t.initial <= t.stop
                    || t.target > 127
                    || t.tolerance > 15
                    || t.target < t.tolerance
                    || (t.keep_min && t.stop == 0)
                    || (!t.keep_min && t.stop_time == 0)
                {
                    return Err(invalid());
                }
            }
            Mode::SpeedCruise(s)
                if (s.target > 0x3fff
                    || s.target < s.tolerance
                    || s.step_up == 0
                    || s.step_down == 0) =>
            {
                return Err(invalid());
            }
            _ => {}
        }
    }
    if config
        .fans
        .iter()
        .any(|f| f.mode != Mode::Ignore && f.smart_tracking)
    {
        let t = &config.tracking;
        if t.step_up > 15
            || t.step_down > 15
            || t.weight_up
                .iter()
                .chain(t.weight_down.iter())
                .any(|v| *v > 6)
            || t.duty_step.iter().any(|v| *v > 15)
            || t.markup_weight > 7
        {
            return Err(Error::InvalidTracking);
        }
    }
    Ok(())
}

fn unlock<P: Writeable<T = u8>, I: Writeable<T = u8>, D: Readable<T = u8> + Writeable<T = u8>>(
    ec: &mut Ec<P, I, D>,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    let status = ec.register::<EngineStatus::Register>(FAN_ENGINE_STS).get();
    if status & 0x40 == 0 && status & 8 != 0 {
        return Ok(());
    }
    let mut ready = false;
    for _ in 0..POLL_COUNT {
        if !ec
            .register::<EngineStatus::Register>(FAN_ENGINE_STS)
            .is_set(EngineStatus::PHASE)
            && !ec
                .register::<ConfigControl::Register>(FAN_CFG_CTRL)
                .is_set(ConfigControl::REQUEST)
        {
            ready = true;
            break;
        }
        delay.delay_ms(1);
    }
    if !ready {
        return Err(Error::Timeout(WaitPhase::ExitConfiguration));
    }
    ec.register::<ConfigControl::Register>(FAN_CFG_CTRL)
        .write(ConfigControl::REQUEST::SET);
    for _ in 0..POLL_COUNT {
        let status = ec.register::<EngineStatus::Register>(FAN_ENGINE_STS).get();
        if status & 0x40 == 0 {
            return if status & 8 != 0 {
                Ok(())
            } else {
                Err(Error::ConfigurationNotOpen { status })
            };
        }
        delay.delay_ms(1);
    }
    Err(Error::Timeout(WaitPhase::Unlock))
}

fn lock_and_check<
    P: Writeable<T = u8>,
    I: Writeable<T = u8>,
    D: Readable<T = u8> + Writeable<T = u8>,
>(
    ec: &mut Ec<P, I, D>,
    delay: &mut impl DelayNs,
) -> Result<(), Error> {
    let status = ec.register::<EngineStatus::Register>(FAN_ENGINE_STS).get();
    // Native silently skips this case. A successful Rust result must actually
    // attest the requested configuration check, not just a pre-existing lock.
    if status & 0x40 != 0 || status & 8 == 0 {
        return Err(Error::ConfigurationNotOpen { status });
    }
    ec.register::<ConfigControl::Register>(FAN_CFG_CTRL)
        .write(ConfigControl::DONE::SET);
    let mut done = false;
    for _ in 0..POLL_COUNT {
        if ec
            .register::<EngineStatus::Register>(FAN_ENGINE_STS)
            .is_set(EngineStatus::CHECK_DONE)
        {
            done = true;
            break;
        }
        delay.delay_ms(1);
    }
    if !done {
        return Err(Error::Timeout(WaitPhase::ConfigurationCheck));
    }
    let status = ec.register::<EngineStatus::Register>(FAN_ENGINE_STS).get();
    if status & 0x10 != 0 {
        return Err(Error::ConfigurationRejected {
            status,
            code: ec.register::<()>(FAN_LAST_ERROR_CODE).get(),
        });
    }
    if status & 0x40 == 0 {
        return Err(Error::ConfigurationNotLocked { status });
    }
    Ok(())
}

pub fn read_info<P: Writeable<T = u8>, I: Writeable<T = u8>, D: Readable<T = u8>>(
    ec: &mut Ec<P, I, D>,
) -> EcInfo {
    let chip_id = u16::from_le_bytes([{ ec.register::<()>(CHIP_ID0).get() }, {
        ec.register::<()>(CHIP_ID1).get()
    }]);
    let customer_id = u16::from_le_bytes([{ ec.register::<()>(CUSTOMER_ID0).get() }, {
        ec.register::<()>(CUSTOMER_ID1).get()
    }]);
    let build = [
        { ec.register::<()>(FW_BUILD_DAY).get() },
        { ec.register::<()>(FW_BUILD_MONTH).get() },
        { ec.register::<()>(FW_BUILD_YEAR).get() },
        { ec.register::<()>(FW_BUILD_SERIALNUM).get() },
    ];
    let firmware = [{ ec.register::<()>(FW_VER0).get() }, {
        ec.register::<()>(FW_VER1).get()
    }];
    let profile = { ec.register::<()>(PROFILE_VER).get() };
    let mut rom = [0; 4];
    for (i, v) in rom.iter_mut().enumerate() {
        *v = ec.register::<()>(ROM_VER0 + i as u16).get();
    }
    let isp_build = [
        { ec.register::<()>(ISP_BUILD_DAY).get() },
        { ec.register::<()>(ISP_BUILD_MONTH).get() },
        { ec.register::<()>(ISP_BUILD_YEAR).get() },
        { ec.register::<()>(ISP_BUILD_SERIALNUM).get() },
    ];
    let isp = [{ ec.register::<()>(ISP_VER0).get() }, {
        ec.register::<()>(ISP_VER1).get()
    }];
    let mut oem = [0; 8];
    for (i, v) in oem.iter_mut().enumerate() {
        *v = ec.register::<()>(OEM_VER + i as u16).get();
    }
    EcInfo {
        chip_id,
        customer_id,
        build,
        firmware,
        profile,
        rom,
        isp_build,
        isp,
        oem,
    }
}

/// Complete supported native order: information, sensors, automatic fans,
/// misc algorithms, sensor lock, final status. Uses ordinary transactions only:
/// no HWM source call uses the transport's `*_page_ff` variant.
pub fn initialize<
    P: Writeable<T = u8>,
    I: Writeable<T = u8>,
    D: Readable<T = u8> + Writeable<T = u8>,
>(
    ec: &mut Ec<P, I, D>,
    delay: &mut impl DelayNs,
    config: &Config,
) -> Result<InitReport, Error> {
    validate(config)?;
    let info = read_info(ec);
    unlock(ec, delay)?;
    ec.register::<HwmConfig::Register>(HWM_CONFIG)
        .modify(HwmConfig::ENABLE::SET);
    let mut peci = false;
    for (i, s) in config.sensors.iter().enumerate() {
        ec.register::<()>(sensor_cfg(i as u16)).set(s.raw());
        if (0x20..=0x27).contains(&s.raw()) {
            // Preserve native per-selector replacement (not accumulated OR).
            ec.register::<PeciAgents::Register>(PECI_AGENT_EN)
                .modify(PeciAgents::ENABLE.val(1 << (s.raw() & 3)));
            peci = true;
        }
        if let Some(smb) = config.smbus_sensor.filter(|smb| smb.index as usize == i) {
            ec.register::<()>(SMBUS_MASTER_CFG2).set(4);
            ec.register::<()>(SMBUS_MASTER_BAUD_RATE_SEL).set(3);
            ec.register::<SmbusConfig::Register>(SMBUS_MASTER_CFG1)
                .modify(SmbusConfig::ENABLE::SET);
            ec.register::<PchConfig::Register>(PCH_THERMAL_DATA_CFG)
                .modify(PchConfig::BAUD.val(smb.baud));
            ec.register::<PchConfig::Register>(PCH_THERMAL_DATA_CFG)
                .modify(PchConfig::PORT.val(smb.port));
            ec.register::<PchConfig::Register>(PCH_THERMAL_DATA_CFG)
                .modify(PchConfig::ONE_BYTE.val(smb.one_byte as u8));
            ec.register::<()>(PCH_DEVICE_ADDR).set(smb.address);
            ec.register::<()>(PCH_THERMAL_CMD).set(smb.command);
        }
    }
    if peci {
        // Correct native AND-mask typo: replace speed, preserve all other bits.
        ec.register::<PeciConfig::Register>(PECI_CFG)
            .modify(PeciConfig::SPEED.val(config.peci_speed));
        ec.register::<PeciConfig::Register>(PECI_CFG)
            .modify(PeciConfig::INIT::SET + PeciConfig::ENABLE::SET);
    }
    lock_and_check(ec, delay)?;
    let mut once = Once::default();
    for (i, fan) in config.fans.iter().enumerate() {
        if fan.mode == Mode::Ignore {
            continue;
        }
        init_fan(ec, delay, config, fan, i as u16, &mut once)?;
    }
    unlock(ec, delay)?;
    ec.register::<HwmConfig::Register>(HWM_CONFIG)
        .modify(HwmConfig::LOCK_SENSORS::SET);
    lock_and_check(ec, delay)?;
    Ok(InitReport {
        info,
        engine_status: ec.register::<()>(FAN_ENGINE_STS).get(),
    })
}

#[derive(Default)]
struct Once {
    tracking: bool,
    dts2: bool,
    ambient: bool,
}

fn pwm(percent: u16) -> u8 {
    (percent * 255 / 100) as u8
}

fn init_fan<P: Writeable<T = u8>, I: Writeable<T = u8>, D: Readable<T = u8> + Writeable<T = u8>>(
    ec: &mut Ec<P, I, D>,
    delay: &mut impl DelayNs,
    config: &Config,
    fan: &Fan,
    n: u16,
    once: &mut Once,
) -> Result<(), Error> {
    unlock(ec, delay)?;
    let mode = match fan.mode {
        Mode::ThermalCruise(_) => 1,
        Mode::SpeedCruise(_) => 2,
        Mode::SmartFan(_) => 3,
        _ => unreachable!(),
    };
    ec.register::<FanMode::Register>(fan_mode_sel(n))
        .modify(FanMode::MODE.val(mode));
    ec.register::<FanPin::Register>(fanin_cfg(n))
        .modify(FanPin::SELECT.val(fan.input));
    ec.register::<FanPin::Register>(fanin_cfg(n))
        .modify(FanPin::ENABLE::SET);
    ec.register::<FanPin::Register>(fanout_cfg(n))
        .modify(FanPin::SELECT.val(fan.output));
    ec.register::<FanPin::Register>(fanout_cfg(n))
        .modify(FanPin::ENABLE::SET);
    match fan.mode {
        Mode::SmartFan(s) => {
            ec.register::<FanFunction::Register>(fan_function_ctrl(n))
                .modify(FanFunction::RPM.val((fan.unit == Unit::Rpm) as u8));
            for (i, v) in s.sources.iter().enumerate() {
                ec.register::<()>(fan_mtz_dcs_data(3 - i as u16, n)).set(*v);
            }
            for i in 0..7 {
                if s.temperatures[i] != 0 {
                    ec.register::<()>(fan_sf4_temp_lvl(n, i as u16))
                        .set(s.temperatures[i]);
                }
                if s.speeds[i] != 0 {
                    let value = if fan.unit == Unit::Pwm {
                        u16::from(pwm(s.speeds[i]))
                    } else {
                        s.speeds[i]
                    };
                    ec.register::<()>(fan_sf4_pwm_rpm_lvl_hi(n, i as u16))
                        .set((value >> 8) as u8);
                    ec.register::<()>(fan_sf4_pwm_rpm_lvl_lo(n, i as u16))
                        .set(value as u8);
                }
            }
            ec.register::<()>(fan_sf4_temp_off_hysteresis(n))
                .set(s.hysteresis);
            ec.register::<()>(fan_sf4_temp_cut_off(n)).set(s.cutoff);
            ec.register::<()>(fan_sf4_temp_off_delay(n))
                .set(s.cutoff_delay);
            ec.register::<()>(fan_step_up_time(n)).set(s.step_up);
            ec.register::<()>(fan_step_down_time(n)).set(s.step_down);
        }
        Mode::ThermalCruise(t) => {
            ec.register::<()>(fan_tc_target_temp(n)).set(t.target);
            if t.keep_min {
                ec.register::<ThermalTarget::Register>(fan_tc_target_temp(n))
                    .modify(ThermalTarget::KEEP_MIN::SET);
                ec.register::<()>(fan_tc_stop_value(n)).set(t.stop);
            }
            ec.register::<()>(fan_tc_target_temp_tolerance(n))
                .set(t.tolerance);
            ec.register::<()>(fan_initial_value(n)).set(t.initial);
            ec.register::<()>(fan_tc_stop_time(n)).set(t.stop_time);
            // Native intentionally maps the config's UP to DOWN register and
            // DOWN to UP register; retained, not silently 'corrected'.
            ec.register::<()>(fan_tc_step_down_time(n)).set(t.step_up);
            ec.register::<()>(fan_tc_step_up_time(n)).set(t.step_down);
        }
        Mode::SpeedCruise(s) => {
            ec.register::<FanFunction::Register>(fan_function_ctrl(n))
                .modify(FanFunction::RPM::SET);
            ec.register::<()>(fan_sc_target_rpm_hi(n))
                .set((s.target >> 8) as u8);
            ec.register::<()>(fan_sc_target_rpm_lo(n))
                .set(s.target as u8);
            ec.register::<()>(fan_sc_target_rpm_tolerance_hi(n))
                .set((s.tolerance >> 8) as u8);
            ec.register::<()>(fan_sc_target_rpm_tolerance_lo(n))
                .set(s.tolerance as u8);
            ec.register::<()>(fan_sc_step_down_time(n)).set(s.step_up);
            ec.register::<()>(fan_sc_step_up_time(n)).set(s.step_down);
        }
        _ => unreachable!(),
    }
    let bit = 1 << (n % 8);
    ec.register::<ChannelBits::Register>(fan_manual_en(n))
        .modify(tock_registers::fields::FieldValue::<
            u8,
            ChannelBits::Register,
        >::new(bit, 0, 0));
    ec.register::<ChannelBits::Register>(fan_channel_en(n))
        .modify(tock_registers::fields::FieldValue::<
            u8,
            ChannelBits::Register,
        >::new(bit, 0, bit));
    lock_and_check(ec, delay)?;
    misc_fan(ec, delay, config, fan, n, once)
}

fn misc_fan<P: Writeable<T = u8>, I: Writeable<T = u8>, D: Readable<T = u8> + Writeable<T = u8>>(
    ec: &mut Ec<P, I, D>,
    delay: &mut impl DelayNs,
    config: &Config,
    fan: &Fan,
    n: u16,
    once: &mut Once,
) -> Result<(), Error> {
    unlock(ec, delay)?;
    if fan.weight != 0 {
        let addr = fan_alg_engine_weight_val(n);
        let field = if n.is_multiple_of(2) {
            FanWeight::LOW.val(fan.weight)
        } else {
            FanWeight::HIGH.val(fan.weight)
        };
        ec.register::<FanWeight::Register>(addr).modify(field);
        let addr = fan_alg_engine_weight_en(n);
        let bit = 1 << (n % 8);
        ec.register::<ChannelBits::Register>(addr)
            .modify(tock_registers::fields::FieldValue::<
                u8,
                ChannelBits::Register,
            >::new(bit, 0, bit));
        ec.register::<FanAlgorithm::Register>(fan_alg_functrl(n))
            .modify(FanAlgorithm::WEIGHT::SET);
    }
    if fan.critical_temp != 0 {
        ec.register::<()>(fan_crit_temp_cfg(n))
            .set(fan.critical_temp);
        if fan.critical_tolerance != 0 {
            ec.register::<()>(fan_crit_temp_tolerance(n))
                .set(fan.critical_tolerance);
        }
        ec.register::<FanAlgorithm::Register>(fan_alg_functrl(n))
            .modify(FanAlgorithm::CRITICAL::SET);
    }
    if fan.temp_error_duty != 0 {
        ec.register::<()>(fan_temp_err_duty_val(n))
            .set(fan.temp_error_duty);
        ec.register::<FanAlgorithm::Register>(fan_alg_functrl(n))
            .modify(FanAlgorithm::TEMP_ERROR::SET);
    }
    if fan.dts2 {
        if !once.dts2 {
            let d = config.dts2;
            ec.register::<()>(DTS2_TARGET_MARGIN)
                .set(d.target_margin as u8);
            ec.register::<()>(DTS2_TCONTROL).set(d.tcontrol as u8);
            ec.register::<()>(DTS2_TCONTROL_OFFSET)
                .set(d.tcontrol_offset as u8);
            ec.register::<()>(DTS2_TARGET_TOLERANCE).set(d.tolerance);
            ec.register::<()>(DTS2_STEP_SPEED_HI)
                .set((d.step_speed >> 8) as u8);
            ec.register::<()>(DTS2_STEP_SPEED_LO)
                .set(d.step_speed as u8);
            ec.register::<()>(DTS2_MIN_SPEED_HI)
                .set((d.min_speed >> 8) as u8);
            ec.register::<()>(DTS2_MIN_SPEED_LO).set(d.min_speed as u8);
            ec.register::<()>(DTS2_DELAY_TIME_COUNTER)
                .set(d.delay_seconds);
            ec.register::<()>(DTS2_DIVISOR).set(d.divisor);
            once.dts2 = true;
        }
        ec.register::<FanAlgorithm::Register>(fan_alg_functrl(n))
            .modify(FanAlgorithm::DTS2::SET);
    }
    if fan.dts_upper_bound {
        ec.register::<FanAlgorithm::Register>(fan_alg_functrl(n))
            .modify(FanAlgorithm::DTS_UB::SET);
    }
    if fan.smart_tracking {
        if !once.tracking {
            let t = config.tracking;
            ec.register::<()>(FAN_LOW_RPM_SPEED_BOUNDARY_HI)
                .set((t.boundary_low >> 8) as u8);
            ec.register::<()>(FAN_LOW_RPM_SPEED_BOUNDARY_LO)
                .set(t.boundary_low as u8);
            ec.register::<()>(FAN_HIGH_RPM_SPEED_BOUNDARY_HI)
                .set((t.boundary_high >> 8) as u8);
            ec.register::<()>(FAN_HIGH_RPM_SPEED_BOUNDARY_LO)
                .set(t.boundary_high as u8);
            for (i, v) in t.tolerance.iter().enumerate() {
                ec.register::<()>(FAN_LOW_RPM_TOLERANCE + i as u16).set(*v);
            }
            ec.register::<()>(FAN_TRACKING_STEP)
                .set(t.step_up << 4 | t.step_down);
            ec.register::<()>(FAN_FAST_TRACK_TEMP_BOUNDARY)
                .set(t.temp_boundary);
            for i in 0..3 {
                ec.register::<()>(FAN_LOW_RPM_FAST_TRACK_WEIGHT + i as u16)
                    .set(t.weight_up[i] << 4 | t.weight_down[i]);
            }
            for (i, v) in t.duty_step.iter().enumerate() {
                ec.register::<()>(FAN_LOW_RPM_FAST_TRACK_DUTY_STEP + i as u16)
                    .set(*v);
            }
            ec.register::<()>(FAN_MARKUP_TRACK_AMB_TEMP_BOUNDARY)
                .set(t.ambient_boundary);
            ec.register::<()>(FAN_MARKUP_TRACK_WEIGHT)
                .set(t.markup_weight);
            once.tracking = true;
        }
        if fan.fast_tracking {
            ec.register::<FanFunction::Register>(fan_function_ctrl(n))
                .modify(FanFunction::FAST::SET);
        }
        if fan.markup_tracking {
            ec.register::<FanFunction::Register>(fan_function_ctrl(n))
                .modify(FanFunction::MARKUP::SET);
        }
        ec.register::<FanFunction::Register>(fan_function_ctrl(n))
            .modify(FanFunction::TRACKING::SET);
    }
    if config.fan_default_percent != 0 {
        ec.register::<()>(FAN_DEFAULT_VAL)
            .set(pwm(config.fan_default_percent.into()));
    }
    if n < 8 {
        if fan.ambient_floor {
            if !once.ambient {
                ec.register::<()>(AMBIENT_FLOOR_TEMP_START_POINT)
                    .set(config.ambient_temperatures[0]);
                ec.register::<()>(AMBIENT_FLOOR_TEMP_END_POINT)
                    .set(config.ambient_temperatures[1]);
                once.ambient = true;
            }
            for (addr, rpm) in [
                fan_amb_floor_min_out_start(n),
                fan_amb_floor_min_out_end(n),
                fan_amb_floor_max_out(n),
            ]
            .into_iter()
            .zip(fan.ambient_rpm)
            {
                ec.register::<()>(addr)
                    .set(if rpm == 0 { 1 } else { (rpm / 100) as u8 });
            }
            ec.register::<FanFunction::Register>(fan_function_ctrl(n))
                .modify(FanFunction::AMBIENT::SET);
        }
        if fan.startup_percent != 0 {
            ec.register::<()>(fan_startup_duty(n))
                .set(pwm(fan.startup_percent.into()));
            ec.register::<FanFunction::Register>(fan_function_ctrl(n))
                .modify(FanFunction::STARTUP::SET);
        }
        if fan.manual_offset != 0 {
            ec.register::<()>(fan_manual_offset(n))
                .set(fan.manual_offset);
            ec.register::<FanFunction::Register>(fan_function_ctrl(n))
                .modify(FanFunction::MANUAL_OFFSET::SET);
        }
        if fan.min_percent != 0 {
            ec.register::<()>(fan_min_duty(n))
                .set(pwm(fan.min_percent.into()));
            ec.register::<FanFunction::Register>(fan_function_ctrl(n))
                .modify(FanFunction::MIN_DUTY::SET);
        }
    }
    lock_and_check(ec, delay)
}

#[cfg(test)]
mod tests;
