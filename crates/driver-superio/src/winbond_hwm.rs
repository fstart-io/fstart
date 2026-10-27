//! Verified W83627EHG V1.3 §6 and W83627THF/THG V1.22 §§5/14 monitors.
//!
//! Profiles are explicit, not inferred from shared runtime identity registers.
//! It supports polling sensors and manual full-speed cooling, not cruise control.
use fstart_core::{pio::IndexedPioRegister, services::device::DeviceError};
use tock_registers::{
    RegisterLongName,
    fields::FieldValue,
    interfaces::{ReadWriteable, Readable, Writeable},
    register_bitfields,
};

register_bitfields![u8,
    BYTE [ VALUE OFFSET(0) NUMBITS(8) [] ],
    BANK_SELECT [
        BANK OFFSET(0) NUMBITS(3) [], ZERO3 OFFSET(3) NUMBITS(1) [],
        THG_ZERO45 OFFSET(4) NUMBITS(2) [],
        ZERO6 OFFSET(6) NUMBITS(1) [], VENDOR_HIGH OFFSET(7) NUMBITS(1) [],
    ],
    CONFIGURATION [
        START OFFSET(0) NUMBITS(1) [], SMI_ENABLE OFFSET(1) NUMBITS(1) [],
        INT_CLEAR OFFSET(3) NUMBITS(1) [], INITIALIZE OFFSET(7) NUMBITS(1) [],
    ],
    FAN_CONFIG1 [
        SYSTEM_DC OFFSET(0) NUMBITS(1) [], CPU_DC OFFSET(1) NUMBITS(1) [],
        SYSTEM_MODE OFFSET(2) NUMBITS(2) [Manual = 0], CPU_MODE OFFSET(4) NUMBITS(2) [Manual = 0],
    ],
    FAN_CONFIG2 [ DC OFFSET(0) NUMBITS(1) [], MODE OFFSET(1) NUMBITS(2) [Manual = 0] ],
    FAN_CONFIG3 [ MODE OFFSET(4) NUMBITS(2) [Manual = 0], DC OFFSET(6) NUMBITS(1) [] ],
    DC_OUTPUT [ EHG_VALUE OFFSET(2) NUMBITS(6) [], THG_VALUE OFFSET(4) NUMBITS(4) [] ],
    SENSOR_TYPE [ BATTERY OFFSET(0) NUMBITS(1) [], DIODE OFFSET(1) NUMBITS(3) [] ],
    DIODE_TYPE [ PENTIUM_II OFFSET(4) NUMBITS(3) [] ],
    SENSOR_CONTROL [ STOP OFFSET(0) NUMBITS(1) [] ],
    SYSTEM_OVT [ DISABLE OFFSET(6) NUMBITS(1) [] ],
    CPU_AUX_OVT [ CPU_DISABLE OFFSET(3) NUMBITS(1) [], AUX_DISABLE OFFSET(4) NUMBITS(1) [] ],
    BEEP [ ENABLE OFFSET(7) NUMBITS(1) [] ],
];

/// Caller-verified PnP model/revision, not runtime-ID auto-detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WinbondMonitorProfile {
    Ehg,
    /// E-version (PnP ID 8285h) only. Runtime ID 90h also occurs on older
    /// silicon and does not establish the revised PWM/DC controls.
    ThgE,
}
impl WinbondMonitorProfile {
    fn accepts_identity(self, chip: u8, vendor: u16) -> bool {
        vendor == 0x5ca3
            && chip
                == match self {
                    Self::Ehg => 0xa1,
                    Self::ThgE => 0x90,
                }
    }
    fn validate_cooling(self, cooling: &FullSpeedCooling) -> Result<(), DeviceError> {
        if self == Self::ThgE && cooling.cpu_secondary.is_some() {
            Err(DeviceError::ConfigError)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemperatureSensor {
    Thermistor,
    /// Diode with the Pentium-II-compatible selector clear.
    Diode,
    PentiumIIDiode,
}

#[derive(Debug, Clone, Copy)]
pub struct TemperatureConfig {
    pub sensor: TemperatureSensor,
    pub offset_c: i8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanOutputMode {
    Pwm,
    DcVoltage,
}
impl FanOutputMode {
    fn dc(self) -> u8 {
        u8::from(self == Self::DcVoltage)
    }
}

/// Physical outputs, not inferred from tachometer availability. `None` preserves
/// the output's individual controls. Full-speed programming is not glitch-free
/// takeover from inherited automatic control; the mode must precede its value.
#[derive(Debug, Clone, Copy, Default)]
pub struct FullSpeedCooling {
    pub system: Option<FanOutputMode>,
    pub cpu_primary: Option<FanOutputMode>,
    pub auxiliary: Option<FanOutputMode>,
    pub cpu_secondary: Option<FanOutputMode>,
}

/// Owns all three analog temperature sources, offsets and battery sampling.
/// Initialization disables SMI, OVT and global beep outputs: this is polling-only
/// monitoring, with no autonomous alarm/shutdown policy. Fan electrical buffers,
/// pin muxes, tachometer divisors and PWM frequencies remain separate policy.
#[derive(Debug, Clone, Copy)]
pub struct MonitorConfig {
    pub system: TemperatureConfig,
    pub cpu: TemperatureConfig,
    pub auxiliary: TemperatureConfig,
    pub battery_voltage: bool,
    pub cooling: FullSpeedCooling,
}
impl MonitorConfig {
    fn temperatures(&self) -> [TemperatureConfig; 3] {
        [self.system, self.cpu, self.auxiliary]
    }
    fn sensor_fields(&self) -> FieldValue<u8, SENSOR_TYPE::Register> {
        let diode = self
            .temperatures()
            .iter()
            .enumerate()
            .fold(0, |bits, (i, t)| {
                bits | (u8::from(t.sensor != TemperatureSensor::Thermistor) << i)
            });
        SENSOR_TYPE::DIODE.val(diode) + SENSOR_TYPE::BATTERY.val(u8::from(self.battery_voltage))
    }
    fn diode_fields(&self) -> FieldValue<u8, DIODE_TYPE::Register> {
        let pentium_ii = self
            .temperatures()
            .iter()
            .enumerate()
            .fold(0, |bits, (i, t)| {
                bits | (u8::from(t.sensor == TemperatureSensor::PentiumIIDiode) << i)
            });
        DIODE_TYPE::PENTIUM_II.val(pentium_ii)
    }
}

#[derive(Debug, Clone, Copy)]
pub enum TemperatureInput {
    System,
    Cpu,
    Auxiliary,
}

fn reserved_bank_fields(profile: WinbondMonitorProfile) -> FieldValue<u8, BANK_SELECT::Register> {
    let fields = BANK_SELECT::ZERO3::CLEAR + BANK_SELECT::ZERO6::CLEAR;
    match profile {
        WinbondMonitorProfile::Ehg => fields,
        WinbondMonitorProfile::ThgE => fields + BANK_SELECT::THG_ZERO45.val(0),
    }
}
fn full_dc_fields(profile: WinbondMonitorProfile) -> FieldValue<u8, DC_OUTPUT::Register> {
    match profile {
        WinbondMonitorProfile::Ehg => DC_OUTPUT::EHG_VALUE.val(63),
        WinbondMonitorProfile::ThgE => DC_OUTPUT::THG_VALUE.val(15),
    }
}
fn monitoring_fields(start: bool) -> FieldValue<u8, CONFIGURATION::Register> {
    CONFIGURATION::INITIALIZE::CLEAR
        + CONFIGURATION::INT_CLEAR::CLEAR
        + CONFIGURATION::SMI_ENABLE::CLEAR
        + CONFIGURATION::START.val(u8::from(start))
}
fn primary_fan_fields(
    profile: WinbondMonitorProfile,
    system: bool,
    mode: FanOutputMode,
) -> FieldValue<u8, FAN_CONFIG1::Register> {
    let manual = if system {
        FAN_CONFIG1::SYSTEM_MODE::Manual
    } else {
        FAN_CONFIG1::CPU_MODE::Manual
    };
    if profile == WinbondMonitorProfile::ThgE {
        manual // Bits 1:0 are reserved; pin mode is Super I/O LDNB CRF0.
    } else if system {
        manual + FAN_CONFIG1::SYSTEM_DC.val(mode.dc())
    } else {
        manual + FAN_CONFIG1::CPU_DC.val(mode.dc())
    }
}
fn auxiliary_fan_fields(
    profile: WinbondMonitorProfile,
    mode: FanOutputMode,
) -> FieldValue<u8, FAN_CONFIG2::Register> {
    let manual = FAN_CONFIG2::MODE::Manual;
    match profile {
        WinbondMonitorProfile::Ehg => manual + FAN_CONFIG2::DC.val(mode.dc()),
        WinbondMonitorProfile::ThgE => manual, // Bit 0 is reserved.
    }
}

/// Caller must first identify the selected PnP model/revision, activate and
/// decode its eight-byte HWM window, then serialize against CPUs, interrupts
/// and any SMBus client. Construction performs no I/O; runtime IDs are only
/// sanity checks. THG-E callers must first use `configure_fan_output_modes`
/// with the same cooling policy: runtime accesses cannot select its PWM/DC pads.
/// Fan buffers/muxes/frequencies and settling remain separate caller policy.
pub struct WinbondMonitor {
    base: u16,
    profile: WinbondMonitorProfile,
}
impl WinbondMonitor {
    pub fn new(base: u16, profile: WinbondMonitorProfile) -> Result<Self, DeviceError> {
        if !(0x100..0x1000).contains(&base) || base & 7 != 0 {
            return Err(DeviceError::ConfigError);
        }
        Ok(Self { base, profile })
    }
    fn register<R: RegisterLongName>(&self, index: u8) -> IndexedPioRegister<R> {
        IndexedPioRegister::ports(self.base + 5, self.base + 6, index)
    }
    // Bank selection affects 50h..5Fh only. Restore defined fields even on a
    // failed probe. EHG §6.8.46 clears reserved bits 3/6 but retains beep fields;
    // THG §5.8.16 requires all bits 6:3 zero.
    fn with_bank<T>(&mut self, bank: u8, operation: impl FnOnce(&Self) -> T) -> T {
        let selector = self.register::<BANK_SELECT::Register>(0x4e);
        let saved = selector.get();
        selector.modify(BANK_SELECT::BANK.val(bank) + reserved_bank_fields(self.profile));
        let result = operation(self);
        selector.set(reserved_bank_fields(self.profile).modify(saved));
        result
    }
    pub fn probe(&mut self) -> Result<(), DeviceError> {
        self.with_bank(0, |this| {
            let chip = this.register::<BYTE::Register>(0x58).get();
            let selector = this.register::<BANK_SELECT::Register>(0x4e);
            selector.modify(BANK_SELECT::VENDOR_HIGH::CLEAR);
            let low = this.register::<BYTE::Register>(0x4f).get();
            selector.modify(BANK_SELECT::VENDOR_HIGH::SET);
            let high = this.register::<BYTE::Register>(0x4f).get();
            if this
                .profile
                .accepts_identity(chip, u16::from_be_bytes([high, low]))
            {
                Ok(())
            } else {
                Err(DeviceError::InitFailed)
            }
        })
    }
    fn full_output(&self, index: u8, mode: FanOutputMode) {
        match mode {
            FanOutputMode::Pwm => self
                .register::<BYTE::Register>(index)
                .write(BYTE::VALUE.val(255)),
            // EHG DC values occupy 7:2, THG-E 7:4. Preserve reserved low bits.
            FanOutputMode::DcVoltage => self
                .register::<DC_OUTPUT::Register>(index)
                .modify(full_dc_fields(self.profile)),
        }
    }
    /// Select manual control before writing the documented maximum output.
    /// Does not need a running conversion loop or sensors. THG-E DC maximum
    /// is 15/16 of AVCC, not full rail voltage; no actual fan-speed guarantee.
    /// Invalid output policy is rejected before I/O; a probe failure makes no
    /// fan-control writes. Takeover from inherited control is not glitch-free.
    pub fn force_full_speed(&mut self, cooling: &FullSpeedCooling) -> Result<(), DeviceError> {
        self.profile.validate_cooling(cooling)?;
        self.probe()?;
        self.program_cooling(cooling);
        Ok(())
    }
    fn program_cooling(&self, cooling: &FullSpeedCooling) {
        for (system, index, mode) in [
            (true, 0x01, cooling.system),
            (false, 0x03, cooling.cpu_primary),
        ] {
            if let Some(mode) = mode {
                self.register::<FAN_CONFIG1::Register>(0x04)
                    .modify(primary_fan_fields(self.profile, system, mode));
                self.full_output(index, mode);
            }
        }
        if let Some(mode) = cooling.auxiliary {
            self.register::<FAN_CONFIG2::Register>(0x12)
                .modify(auxiliary_fan_fields(self.profile, mode));
            self.full_output(0x11, mode);
        }
        if let Some(mode) = cooling.cpu_secondary {
            self.register::<FAN_CONFIG3::Register>(0x62)
                .modify(FAN_CONFIG3::MODE::Manual + FAN_CONFIG3::DC.val(mode.dc()));
            self.full_output(0x61, mode);
        }
    }
    pub fn init(&mut self, config: &MonitorConfig) -> Result<(), DeviceError> {
        self.profile.validate_cooling(&config.cooling)?;
        self.probe()?;
        // Cooling does not depend on sensor configuration/conversion startup.
        self.program_cooling(&config.cooling);
        self.with_bank(0, |this| {
            this.register::<CONFIGURATION::Register>(0x40)
                .modify(monitoring_fields(false));
            this.register::<SYSTEM_OVT::Register>(0x18)
                .modify(SYSTEM_OVT::DISABLE::SET);
            this.register::<CPU_AUX_OVT::Register>(0x4c)
                .modify(CPU_AUX_OVT::CPU_DISABLE::SET + CPU_AUX_OVT::AUX_DISABLE::SET);
            this.register::<BEEP::Register>(0x57)
                .modify(BEEP::ENABLE::CLEAR);
            this.register::<DIODE_TYPE::Register>(0x59)
                .modify(config.diode_fields());
            this.register::<SENSOR_TYPE::Register>(0x5d)
                .modify(config.sensor_fields());
        });
        self.with_bank(4, |this| {
            for (i, temperature) in config.temperatures().iter().enumerate() {
                this.register::<BYTE::Register>(0x54 + i as u8)
                    .write(BYTE::VALUE.val(temperature.offset_c as u8));
            }
        });
        for bank in [1, 2] {
            self.with_bank(bank, |this| {
                this.register::<SENSOR_CONTROL::Register>(0x52)
                    .modify(SENSOR_CONTROL::STOP::CLEAR)
            });
        }
        self.register::<CONFIGURATION::Register>(0x40)
            .modify(monitoring_fields(true));
        Ok(())
    }
    /// Read whole degrees Celsius, dropping the remote inputs' half-degree bit.
    /// One-byte readings avoid a torn high/low sample across a conversion.
    /// The caller must allow conversions to settle after init; a plausible value
    /// does not prove freshness, correct wiring, or a functioning sensor.
    pub fn temperature_celsius(&mut self, input: TemperatureInput) -> i8 {
        match input {
            TemperatureInput::System => self.register::<BYTE::Register>(0x27).get() as i8,
            TemperatureInput::Cpu => {
                self.with_bank(1, |this| this.register::<BYTE::Register>(0x50).get() as i8)
            }
            TemperatureInput::Auxiliary => {
                self.with_bank(2, |this| this.register::<BYTE::Register>(0x50).get() as i8)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn valid_windows_and_no_io_on_construction() {
        for profile in [WinbondMonitorProfile::Ehg, WinbondMonitorProfile::ThgE] {
            for base in [0x100, 0x290, 0xa00, 0xff8] {
                assert!(WinbondMonitor::new(base, profile).is_ok());
            }
            for base in [0, 0x008, 0x0f8, 0x291, 0x1000, 0xfff8] {
                assert!(matches!(
                    WinbondMonitor::new(base, profile),
                    Err(DeviceError::ConfigError)
                ));
            }
        }
    }
    #[test]
    fn sensor_fields_preserve_divisors_and_reference_policies_encode_semantically() {
        let config = MonitorConfig {
            system: TemperatureConfig {
                sensor: TemperatureSensor::Diode,
                offset_c: -15,
            },
            cpu: TemperatureConfig {
                sensor: TemperatureSensor::PentiumIIDiode,
                offset_c: 25,
            },
            auxiliary: TemperatureConfig {
                sensor: TemperatureSensor::Diode,
                offset_c: -4,
            },
            battery_voltage: true,
            cooling: FullSpeedCooling::default(),
        };
        assert_eq!(config.sensor_fields().modify(0xf0), 0xff);
        assert_eq!(config.diode_fields().modify(0x8f), 0xaf);
        assert_eq!(
            config.temperatures().map(|t| t.offset_c as u8),
            [0xf1, 0x19, 0xfc]
        );
        let thermistors = MonitorConfig {
            system: TemperatureConfig {
                sensor: TemperatureSensor::Thermistor,
                offset_c: 0,
            },
            cpu: TemperatureConfig {
                sensor: TemperatureSensor::Thermistor,
                offset_c: 0,
            },
            auxiliary: TemperatureConfig {
                sensor: TemperatureSensor::Thermistor,
                offset_c: 0,
            },
            battery_voltage: false,
            ..config
        };
        assert_eq!(thermistors.sensor_fields().modify(0xff), 0xf0);
        assert_eq!(thermistors.diode_fields().modify(0xff), 0x8f);
    }
    #[test]
    fn full_speed_and_monitoring_preserve_unrelated_fields() {
        assert_eq!(
            primary_fan_fields(WinbondMonitorProfile::Ehg, true, FanOutputMode::DcVoltage)
                .modify(0xff),
            0xf3
        );
        assert_eq!(
            primary_fan_fields(WinbondMonitorProfile::Ehg, false, FanOutputMode::Pwm).modify(0xff),
            0xcd
        );
        assert_eq!(full_dc_fields(WinbondMonitorProfile::Ehg).modify(3), 0xff);
        assert_eq!(monitoring_fields(true).modify(0xff), 0x75);
        assert_eq!(monitoring_fields(false).modify(0xff), 0x74);
        // Bank changes must not disable beep sources or change vendor byte.
        assert_eq!(
            (BANK_SELECT::BANK.val(4) + reserved_bank_fields(WinbondMonitorProfile::Ehg))
                .modify(0xff),
            0xb4
        );
        assert_eq!(
            reserved_bank_fields(WinbondMonitorProfile::Ehg).modify(0xff),
            0xb7
        );
    }

    #[test]
    fn thg_e_preserves_reserved_fan_fields_and_uses_four_bit_dc_values() {
        let profile = WinbondMonitorProfile::ThgE;
        assert_eq!(
            primary_fan_fields(profile, true, FanOutputMode::Pwm).modify(0xff),
            0xf3
        );
        assert_eq!(
            primary_fan_fields(profile, false, FanOutputMode::DcVoltage).modify(0xff),
            0xcf
        );
        assert_eq!(
            auxiliary_fan_fields(profile, FanOutputMode::Pwm).modify(0xff),
            0xf9
        );
        assert_eq!(full_dc_fields(profile).modify(0x05), 0xf5);
        assert_eq!(
            full_dc_fields(WinbondMonitorProfile::Ehg).modify(0x05),
            0xfd
        );
        assert_eq!(reserved_bank_fields(profile).modify(0xff), 0x87);
        assert_eq!(
            (BANK_SELECT::BANK.val(4) + reserved_bank_fields(profile)).modify(0xff),
            0x84
        );
        assert!(profile.accepts_identity(0x90, 0x5ca3));
        assert!(!profile.accepts_identity(0xa1, 0x5ca3));
        assert!(!profile.accepts_identity(0x90, 0xa35c));
        assert!(WinbondMonitorProfile::Ehg.accepts_identity(0xa1, 0x5ca3));
        assert!(!WinbondMonitorProfile::Ehg.accepts_identity(0x90, 0x5ca3));
    }

    #[test]
    fn thg_secondary_cpu_fan_policy_fails_before_io() {
        let cooling = FullSpeedCooling {
            cpu_secondary: Some(FanOutputMode::Pwm),
            ..FullSpeedCooling::default()
        };
        let mut monitor = WinbondMonitor::new(0xa00, WinbondMonitorProfile::ThgE).unwrap();
        // Running this in an unprivileged host test would fault if probe/port
        // access preceded policy validation.
        assert_eq!(
            monitor.force_full_speed(&cooling),
            Err(DeviceError::ConfigError)
        );
        assert!(
            WinbondMonitorProfile::Ehg
                .validate_cooling(&cooling)
                .is_ok()
        );
        let temperature = TemperatureConfig {
            sensor: TemperatureSensor::Thermistor,
            offset_c: 0,
        };
        let config = MonitorConfig {
            system: temperature,
            cpu: temperature,
            auxiliary: temperature,
            battery_voltage: false,
            cooling,
        };
        assert_eq!(monitor.init(&config), Err(DeviceError::ConfigError));
    }
}
