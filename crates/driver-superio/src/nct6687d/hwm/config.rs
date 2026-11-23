// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit policy models from native chip.h, not chip defaults or telemetry.

/// Source sensor selector. Unlisted encodings are rejected before I/O.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Sensor(pub u8);
impl Sensor {
    pub const fn raw(self) -> u8 {
        self.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Unit {
    #[default]
    Pwm,
    Rpm,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Ignore,
    ThermalCruise(ThermalCruise),
    SpeedCruise(SpeedCruise),
    SmartFan(SmartFan),
    /// Native PID is unimplemented and rejects before I/O.
    Pid,
    /// Native manual validation only permits zero duty, apparently a typo.
    /// Unsupported rather than introducing a fan-off fallback.
    Manual,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SmartFan {
    /// Source byte masks, written most-significant source first; not sensor IDs.
    pub sources: [u8; 4],
    pub temperatures: [u8; 7],
    /// PWM percent or RPM, according to Fan::unit. Zero retains source's skip.
    pub speeds: [u16; 7],
    pub hysteresis: u8,
    pub cutoff: u8,
    pub cutoff_delay: u8,
    /// Native units: 100 milliseconds.
    pub step_up: u8,
    pub step_down: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct ThermalCruise {
    pub keep_min: bool,
    pub target: u8,
    pub tolerance: u8,
    pub initial: u8,
    pub stop: u8,
    /// Native time fields; source writes UP to DOWN and DOWN to UP registers.
    pub stop_time: u8,
    pub step_up: u8,
    pub step_down: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SpeedCruise {
    pub target: u16,
    pub tolerance: u16,
    pub step_up: u8,
    pub step_down: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Fan {
    pub mode: Mode,
    pub unit: Unit,
    pub input: u8,
    pub output: u8,
    pub weight: u8,
    pub critical_temp: u8,
    pub critical_tolerance: u8,
    /// Raw 8-bit native error-output duty (not percent).
    pub temp_error_duty: u8,
    /// DTS1 fails closed: source RPM_END high/low addresses alias.
    pub dts1: bool,
    pub dts2: bool,
    pub dts_upper_bound: bool,
    pub smart_tracking: bool,
    pub fast_tracking: bool,
    pub markup_tracking: bool,
    pub ambient_floor: bool,
    pub ambient_rpm: [u16; 3],
    pub startup_percent: u8,
    pub manual_offset: u8,
    pub min_percent: u8,
}
impl Fan {
    /// Exact all-zero source inventory; ignored, not an active thermal policy.
    pub const IGNORED: Self = Self {
        mode: Mode::Ignore,
        unit: Unit::Pwm,
        input: 0,
        output: 0,
        weight: 0,
        critical_temp: 0,
        critical_tolerance: 0,
        temp_error_duty: 0,
        dts1: false,
        dts2: false,
        dts_upper_bound: false,
        smart_tracking: false,
        fast_tracking: false,
        markup_tracking: false,
        ambient_floor: false,
        ambient_rpm: [0; 3],
        startup_percent: 0,
        manual_offset: 0,
        min_percent: 0,
    };
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SmbusSensor {
    pub index: u8,
    pub one_byte: bool,
    pub port: u8,
    pub baud: u8,
    pub address: u8,
    pub command: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Dts2 {
    pub target_margin: i8,
    pub tolerance: u8,
    pub tcontrol: i8,
    pub tcontrol_offset: i8,
    pub step_speed: u16,
    pub min_speed: u16,
    pub delay_seconds: u8,
    pub divisor: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Tracking {
    pub tolerance: [u8; 3],
    pub boundary_low: u16,
    pub boundary_high: u16,
    pub step_up: u8,
    pub step_down: u8,
    pub temp_boundary: u8,
    /// Source enum encodings 0..6 (dividers 1,2,4,8,16,32,64).
    pub weight_up: [u8; 3],
    pub weight_down: [u8; 3],
    pub duty_step: [u8; 3],
    pub ambient_boundary: u8,
    pub markup_weight: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Config {
    pub fans: [Fan; 10],
    pub sensors: [Sensor; 32],
    /// Native PECI encodings: 0=2MHz, 1=1.2MHz, 2=800kHz, 3=400kHz.
    pub peci_speed: u8,
    pub smbus_sensor: Option<SmbusSensor>,
    pub dts2: Dts2,
    pub ambient_temperatures: [u8; 2],
    pub tracking: Tracking,
    /// Source automatic default-output policy, not a manual-mode fallback.
    pub fan_default_percent: u8,
}
impl Config {
    pub const SOURCE_ZERO: Self = Self {
        fans: [Fan::IGNORED; 10],
        sensors: [Sensor(0); 32],
        peci_speed: 0,
        smbus_sensor: None,
        dts2: Dts2 {
            target_margin: 0,
            tolerance: 0,
            tcontrol: 0,
            tcontrol_offset: 0,
            step_speed: 0,
            min_speed: 0,
            delay_seconds: 0,
            divisor: 0,
        },
        ambient_temperatures: [0; 2],
        tracking: Tracking {
            tolerance: [0; 3],
            boundary_low: 0,
            boundary_high: 0,
            step_up: 0,
            step_down: 0,
            temp_boundary: 0,
            weight_up: [0; 3],
            weight_down: [0; 3],
            duty_step: [0; 3],
            ambient_boundary: 0,
            markup_weight: 0,
        },
        fan_default_percent: 0,
    };
}
