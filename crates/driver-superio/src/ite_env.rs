//! Seven-bit SmartGuardian environmental control, verified for IT8718F C+.
//!
//! IT8718F V0.3 §9.6 defines the index/data ports at EC base +5/+6.
//! Related chips must verify this register profile before adopting the driver.
//! Fan full-speed temperature registers used by other ITE generations are
//! reserved here: a requested full-speed temperature is encoded as a slope.
use fstart_core::{pio::IndexedPioRegister, services::device::DeviceError};
use tock_registers::{
    RegisterLongName,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u8,
    BYTE [ VALUE OFFSET(0) NUMBITS(8) [] ],
    CONFIGURATION [
        INITIALIZE OFFSET(7) NUMBITS(1) [], UPDATE_VBAT OFFSET(6) NUMBITS(1) [],
        CLEAR_CASE_OPEN OFFSET(5) NUMBITS(1) [], INT_CLEAR OFFSET(3) NUMBITS(1) [],
        IRQ_ENABLE OFFSET(2) NUMBITS(1) [], SMI_ENABLE OFFSET(1) NUMBITS(1) [], START OFFSET(0) NUMBITS(1) [],
    ],
    INTERFACE [ PSEUDO_EOC OFFSET(7) NUMBITS(1) [], EXTERNAL_HOST OFFSET(4) NUMBITS(3) [Disabled = 0] ],
    SMOOTHING [ FREQUENCY OFFSET(6) NUMBITS(2) [Khz1 = 0] ],
    FAN_MAIN [ SMART OFFSET(0) NUMBITS(3) [], TACHOMETERS OFFSET(4) NUMBITS(3) [] ],
    FAN_COUNTER [ COUNTERS_16BIT OFFSET(0) NUMBITS(3) [] ],
    FAN_CONTROL [
        POLARITY_HIGH OFFSET(7) NUMBITS(1) [], CLOCK OFFSET(4) NUMBITS(3) [Mhz3 = 5],
        MINIMUM_20_PERCENT OFFSET(3) NUMBITS(1) [], ON OFFSET(0) NUMBITS(3) [],
    ],
    PWM [ AUTOMATIC OFFSET(7) NUMBITS(1) [], PAYLOAD OFFSET(0) NUMBITS(7) [] ],
    TEMPERATURE_INPUTS [ DIODE OFFSET(0) NUMBITS(3) [], RESISTOR OFFSET(3) NUMBITS(3) [] ],
    EXTRA_INPUTS [
        TEMPERATURE3_EXTERNAL OFFSET(7) NUMBITS(1) [], FAN2_CLOCK OFFSET(4) NUMBITS(3) [Mhz3 = 5],
        FAN2_MINIMUM_20_PERCENT OFFSET(3) NUMBITS(1) [], VIN_AS_TEMPERATURE OFFSET(0) NUMBITS(3) [],
    ],
    BEEP [ OFFSET_WRITE_ENABLE OFFSET(7) NUMBITS(1) [] ],
    PWM_START [ SLOPE_HIGH OFFSET(7) NUMBITS(1) [], DUTY OFFSET(0) NUMBITS(7) [] ],
    PWM_SLOPE [ SMOOTHING OFFSET(7) NUMBITS(1) [], SLOPE_LOW OFFSET(0) NUMBITS(6) [] ],
    PWM_HYSTERESIS [ DIRECT_DECREASE OFFSET(7) NUMBITS(1) [], CELSIUS OFFSET(0) NUMBITS(5) [] ],
    EXTRA_VECTOR [ TARGET OFFSET(5) NUMBITS(2) [None = 0] ],
];

fn monitoring_fields() -> tock_registers::fields::FieldValue<u8, CONFIGURATION::Register> {
    CONFIGURATION::INITIALIZE::CLEAR
        + CONFIGURATION::CLEAR_CASE_OPEN::CLEAR
        + CONFIGURATION::UPDATE_VBAT::SET
        + CONFIGURATION::INT_CLEAR::CLEAR
        + CONFIGURATION::IRQ_ENABLE::CLEAR
        + CONFIGURATION::SMI_ENABLE::CLEAR
        + CONFIGURATION::START::SET
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemperatureInput {
    One,
    Two,
    Three,
}
impl TemperatureInput {
    const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemperatureMode {
    Disabled,
    Resistor,
    Diode,
}

#[derive(Debug, Clone, Copy)]
pub struct TemperatureConfig {
    pub mode: TemperatureMode,
    pub offset_c: i8,
    pub low_c: i8,
    pub high_c: i8,
}
impl TemperatureConfig {
    #[must_use]
    pub const fn new(mode: TemperatureMode) -> Self {
        Self {
            mode,
            offset_c: 0,
            low_c: 0,
            high_c: 127,
        }
    }
}

/// Nominal ramp with a hardware-quantized slope, calculated to reach full duty
/// at or below `full_c`. Sampling, hysteresis and smoothing affect the actual
/// response; this is not an unconditional thermal deadline. Start duty is below
/// 100%; use `FullSpeed` for an unconditional full-speed output.
#[derive(Debug, Clone, Copy)]
pub struct FanCurve {
    off_c: i8,
    start_c: i8,
    full_c: i8,
    start_percent: u8,
    hysteresis_c: u8,
    smoothing: bool,
}
impl FanCurve {
    #[must_use]
    pub const fn new(off_c: i8, start_c: i8, full_c: i8, start_percent: u8) -> Self {
        assert!(off_c <= start_c && start_c < full_c && start_percent < 100);
        let curve = Self {
            off_c,
            start_c,
            full_c,
            start_percent,
            hysteresis_c: 0,
            smoothing: false,
        };
        assert!(curve.slope_eighths() <= 127);
        curve
    }
    #[must_use]
    pub const fn hysteresis(mut self, celsius: u8) -> Self {
        assert!(celsius <= 31);
        self.hysteresis_c = celsius;
        self
    }
    #[must_use]
    pub const fn smoothing(mut self, enabled: bool) -> Self {
        self.smoothing = enabled;
        self
    }
    const fn start_pwm(self) -> u8 {
        (self.start_percent as u16 * 128 / 100) as u8
    }
    const fn slope_eighths(self) -> u16 {
        let span = (self.full_c as i16 - self.start_c as i16) as u16;
        ((128 - self.start_pwm() as u16) * 8).div_ceil(span)
    }
    fn start_fields(self) -> tock_registers::fields::FieldValue<u8, PWM_START::Register> {
        PWM_START::DUTY.val(self.start_pwm())
            + PWM_START::SLOPE_HIGH.val((self.slope_eighths() >> 6) as u8)
    }
    fn slope_fields(self) -> tock_registers::fields::FieldValue<u8, PWM_SLOPE::Register> {
        PWM_SLOPE::SLOPE_LOW.val((self.slope_eighths() & 63) as u8)
            + PWM_SLOPE::SMOOTHING.val(self.smoothing as u8)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum FanPolicy {
    FullSpeed,
    Automatic {
        input: TemperatureInput,
        curve: FanCurve,
    },
}
#[derive(Debug, Clone, Copy)]
pub enum FanPolarity {
    ActiveLow,
    ActiveHigh,
}

/// Owns all eight voltage and three temperature inputs. Absent fan policies
/// preserve those fans' individual controls, but polarity/PWM clock settings
/// are shared and affect all outputs. Extra-vector targets are disconnected so
/// inherited vector curves cannot contribute to the standard fan policy.
/// PWM is linear with a zero minimum duty.
#[derive(Debug, Clone, Copy)]
pub struct EnvironmentConfig {
    pub temperatures: [TemperatureConfig; 3],
    pub voltage_inputs: [bool; 8],
    pub fans: [Option<FanPolicy>; 3],
    pub fan_polarity: FanPolarity,
}
impl EnvironmentConfig {
    fn valid(&self) -> bool {
        self.temperatures
            .iter()
            .all(|t| t.low_c <= t.high_c && (t.mode == TemperatureMode::Diode || t.offset_c == 0))
            && self.fans.iter().flatten().all(|fan| match fan {
                FanPolicy::FullSpeed => true,
                FanPolicy::Automatic { input, .. } => {
                    self.temperatures[input.index()].mode != TemperatureMode::Disabled
                }
            })
    }
    fn fan_mask(&self) -> u8 {
        self.fans.iter().enumerate().fold(0, |mask, (i, fan)| {
            mask | if fan.is_some() { 1 << i } else { 0 }
        })
    }
    fn temperature_fields(
        &self,
    ) -> tock_registers::fields::FieldValue<u8, TEMPERATURE_INPUTS::Register> {
        let mask = |mode| {
            self.temperatures
                .iter()
                .enumerate()
                .fold(0, |mask, (i, t)| {
                    mask | if t.mode == mode { 1 << i } else { 0 }
                })
        };
        TEMPERATURE_INPUTS::DIODE.val(mask(TemperatureMode::Diode))
            + TEMPERATURE_INPUTS::RESISTOR.val(mask(TemperatureMode::Resistor))
    }
}

/// Caller owns the decoded EC window and serializes its index/data pair.
/// Construction performs no I/O. The Super I/O must already have been
/// identified as a chip implementing this profile and activated at this base.
pub struct IteEnvironmentController {
    base: u16,
}
impl IteEnvironmentController {
    pub fn new(base: u16) -> Result<Self, DeviceError> {
        if base == 0 || base >= 0x1000 || base & 7 != 0 {
            return Err(DeviceError::ConfigError);
        }
        Ok(Self { base })
    }
    fn register<R: RegisterLongName>(&self, index: u8) -> IndexedPioRegister<R> {
        IndexedPioRegister::new(self.base + 5, self.base + 6, index)
    }
    fn probe(&self) -> Result<(), DeviceError> {
        if self.register::<BYTE::Register>(0x58).get() != 0x90
            || self.register::<BYTE::Register>(0x5b).get() != 0x12
        {
            return Err(DeviceError::InitFailed);
        }
        Ok(())
    }
    fn configure_pwm(&mut self, polarity: FanPolarity) {
        self.register::<FAN_CONTROL::Register>(0x14).modify(
            FAN_CONTROL::POLARITY_HIGH.val(matches!(polarity, FanPolarity::ActiveHigh) as u8)
                + FAN_CONTROL::CLOCK::Mhz3
                + FAN_CONTROL::MINIMUM_20_PERCENT::CLEAR,
        );
        self.register::<EXTRA_INPUTS::Register>(0x55)
            .modify(EXTRA_INPUTS::FAN2_CLOCK::Mhz3 + EXTRA_INPUTS::FAN2_MINIMUM_20_PERCENT::CLEAR);
    }
    fn force_fans_full(&mut self, mask: u8) {
        // Establish ON before leaving SmartGuardian mode; do not touch latches.
        let on =
            tock_registers::fields::FieldValue::<u8, FAN_CONTROL::Register>::new(mask, 0, mask);
        let smart = tock_registers::fields::FieldValue::<u8, FAN_MAIN::Register>::new(mask, 0, 0);
        self.register::<FAN_CONTROL::Register>(0x14).modify(on);
        self.register::<FAN_MAIN::Register>(0x13).modify(smart);
    }
    /// Early cooling policy, usable before DRAM training once LPC decoding and
    /// Super I/O EC activation are ready. Identity is checked before updates.
    pub fn full_speed(
        &mut self,
        fans: [bool; 3],
        polarity: FanPolarity,
    ) -> Result<(), DeviceError> {
        self.probe()?;
        self.configure_pwm(polarity);
        let mask = fans
            .into_iter()
            .enumerate()
            .fold(0, |mask, (i, on)| mask | if on { 1 << i } else { 0 });
        self.force_fans_full(mask);
        Ok(())
    }

    pub fn init(&mut self, config: &EnvironmentConfig) -> Result<(), DeviceError> {
        if !config.valid() {
            return Err(DeviceError::ConfigError);
        }
        self.probe()?;
        self.configure_pwm(config.fan_polarity);
        self.force_fans_full(config.fan_mask());
        // §9.6.2.2.61: target None disconnects each extra vector, preserving
        // its temperature selector and hysteresis. No global reset is needed.
        for index in [0x92, 0x96] {
            self.register::<EXTRA_VECTOR::Register>(index)
                .modify(EXTRA_VECTOR::TARGET::None);
        }
        // This profile is analog-only; do not inherit an external-sensor host.
        self.register::<INTERFACE::Register>(0x0a)
            .modify(INTERFACE::PSEUDO_EOC::CLEAR + INTERFACE::EXTERNAL_HOST::Disabled);
        self.register::<EXTRA_INPUTS::Register>(0x55).modify(
            EXTRA_INPUTS::TEMPERATURE3_EXTERNAL::CLEAR + EXTRA_INPUTS::VIN_AS_TEMPERATURE.val(0),
        );
        self.register::<TEMPERATURE_INPUTS::Register>(0x51)
            .modify(config.temperature_fields());
        for (i, temperature) in config.temperatures.iter().enumerate() {
            if temperature.mode == TemperatureMode::Disabled {
                continue;
            }
            if temperature.mode == TemperatureMode::Diode {
                let gate = self.register::<BEEP::Register>(0x5c);
                let enabled = gate.is_set(BEEP::OFFSET_WRITE_ENABLE);
                gate.modify(BEEP::OFFSET_WRITE_ENABLE::SET);
                self.register::<BYTE::Register>([0x56, 0x57, 0x59][i])
                    .write(BYTE::VALUE.val(temperature.offset_c as u8));
                gate.modify(BEEP::OFFSET_WRITE_ENABLE.val(enabled as u8));
            }
            self.register::<BYTE::Register>(0x40 + i as u8 * 2)
                .write(BYTE::VALUE.val(temperature.high_c as u8));
            self.register::<BYTE::Register>(0x41 + i as u8 * 2)
                .write(BYTE::VALUE.val(temperature.low_c as u8));
        }
        let voltage_mask = config
            .voltage_inputs
            .iter()
            .enumerate()
            .fold(0, |mask, (i, enabled)| {
                mask | if *enabled { 1 << i } else { 0 }
            });
        self.register::<BYTE::Register>(0x50)
            .write(BYTE::VALUE.val(voltage_mask));
        // INT_Clear must be zero for conversion (§9.6.3.2, §9.6.3.6).
        // Keep IRQ/SMI disabled without stopping the monitoring loop or
        // replaying reset/COPEN strobes.
        self.register::<CONFIGURATION::Register>(0x00)
            .modify(monitoring_fields());
        self.register::<SMOOTHING::Register>(0x0b)
            .modify(SMOOTHING::FREQUENCY::Khz1);
        for (i, fan) in config.fans.iter().enumerate() {
            let Some(fan) = fan else {
                continue;
            };
            let field = tock_registers::fields::Field::<u8, FAN_MAIN::Register>::new(1, i);
            let tach = tock_registers::fields::Field::<u8, FAN_MAIN::Register>::new(1, i + 4);
            let counter = tock_registers::fields::Field::<u8, FAN_COUNTER::Register>::new(1, i);
            self.register::<FAN_COUNTER::Register>(0x0c)
                .modify(counter.val(1));
            self.register::<FAN_MAIN::Register>(0x13)
                .modify(tach.val(1));
            if let FanPolicy::Automatic { input, curve } = fan {
                let base = 0x60 + i as u8 * 8;
                self.register::<BYTE::Register>(base)
                    .write(BYTE::VALUE.val(curve.off_c as u8));
                self.register::<BYTE::Register>(base + 1)
                    .write(BYTE::VALUE.val(curve.start_c as u8));
                // base+2 is RESERVED on this controller generation.
                self.register::<PWM_START::Register>(base + 3)
                    .write(curve.start_fields());
                self.register::<PWM_SLOPE::Register>(base + 4)
                    .modify(curve.slope_fields());
                self.register::<PWM_HYSTERESIS::Register>(base + 5).modify(
                    PWM_HYSTERESIS::DIRECT_DECREASE::CLEAR
                        + PWM_HYSTERESIS::CELSIUS.val(curve.hysteresis_c),
                );
                self.register::<PWM::Register>(0x15 + i as u8)
                    .write(PWM::AUTOMATIC::SET + PWM::PAYLOAD.val(input.index() as u8));
                self.register::<FAN_MAIN::Register>(0x13)
                    .modify(field.val(1));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoded_ramp_reaches_nominal_full_by_endpoint() {
        for (start, full, duty) in [(35, 75, 0), (40, 80, 20), (-10, 70, 30), (70, 75, 80)] {
            let curve = FanCurve::new(30.min(start), start, full, duty);
            let span = (full as i16 - start as i16) as u16;
            assert!(curve.start_pwm() as u16 * 8 + curve.slope_eighths() * span >= 128 * 8);
            assert!(curve.slope_eighths() <= 127);
        }
        let curve = FanCurve::new(30, 35, 75, 0).hysteresis(3).smoothing(true);
        assert_eq!(curve.start_fields().value, 0);
        assert_eq!(curve.slope_fields().value, 0x9a);
        // Higher slope bit and reserved control bit 6 are independent.
        let curve = FanCurve::new(30, 35, 45, 0);
        assert_eq!(curve.start_fields().value, 0x80);
        assert_eq!(curve.slope_fields().modify(0x40), 0x67);
    }
    #[test]
    fn modes_clear_conflicting_temperature_bits_and_keep_reserved_bits() {
        let config = EnvironmentConfig {
            temperatures: [
                TemperatureConfig::new(TemperatureMode::Resistor),
                TemperatureConfig::new(TemperatureMode::Resistor),
                TemperatureConfig::new(TemperatureMode::Diode),
            ],
            voltage_inputs: [true; 8],
            fans: [None; 3],
            fan_polarity: FanPolarity::ActiveHigh,
        };
        assert_eq!(config.temperature_fields().modify(0xff), 0xdc);
        assert!(config.valid());
        let invalid = EnvironmentConfig {
            fans: [
                Some(FanPolicy::Automatic {
                    input: TemperatureInput::One,
                    curve: FanCurve::new(30, 35, 75, 0),
                }),
                None,
                None,
            ],
            temperatures: [TemperatureConfig::new(TemperatureMode::Disabled); 3],
            ..config
        };
        assert!(!invalid.valid());
        assert!(IteEnvironmentController::new(0x290).is_ok());
        assert!(IteEnvironmentController::new(0x291).is_err());
    }
    #[test]
    fn monitoring_runs_without_interrupts_or_strobes() {
        let value = monitoring_fields().modify(0xff);
        assert_eq!(value, 0x51);
        assert_eq!(
            value
                & (CONFIGURATION::INT_CLEAR::SET.value
                    | CONFIGURATION::IRQ_ENABLE::SET.value
                    | CONFIGURATION::SMI_ENABLE::SET.value
                    | CONFIGURATION::INITIALIZE::SET.value
                    | CONFIGURATION::CLEAR_CASE_OPEN::SET.value),
            0
        );
        // Target None leaves the extra vector's source and hysteresis intact.
        assert_eq!(EXTRA_VECTOR::TARGET::None.modify(0xff), 0x9f);
    }

    #[test]
    #[should_panic]
    fn unrepresentable_ramp_is_rejected() {
        let _ = FanCurve::new(30, 35, 36, 0);
    }
}
