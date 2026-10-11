//! W83627EHG V1.3 §6 runtime hardware monitor.
//!
//! This profile is not assumed compatible with other Winbond/Nuvoton chips.
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
    DC_OUTPUT [ VALUE OFFSET(2) NUMBITS(6) [] ],
    SENSOR_TYPE [ BATTERY OFFSET(0) NUMBITS(1) [], DIODE OFFSET(1) NUMBITS(3) [] ],
    DIODE_TYPE [ PENTIUM_II OFFSET(4) NUMBITS(3) [] ],
    SENSOR_CONTROL [ STOP OFFSET(0) NUMBITS(1) [] ],
    SYSTEM_OVT [ DISABLE OFFSET(6) NUMBITS(1) [] ],
    CPU_AUX_OVT [ CPU_DISABLE OFFSET(3) NUMBITS(1) [], AUX_DISABLE OFFSET(4) NUMBITS(1) [] ],
    BEEP [ ENABLE OFFSET(7) NUMBITS(1) [] ],
];

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

fn reserved_bank_fields() -> FieldValue<u8, BANK_SELECT::Register> {
    BANK_SELECT::ZERO3::CLEAR + BANK_SELECT::ZERO6::CLEAR
}
fn monitoring_fields(start: bool) -> FieldValue<u8, CONFIGURATION::Register> {
    CONFIGURATION::INITIALIZE::CLEAR
        + CONFIGURATION::INT_CLEAR::CLEAR
        + CONFIGURATION::SMI_ENABLE::CLEAR
        + CONFIGURATION::START.val(u8::from(start))
}
fn primary_fan_fields(system: bool, mode: FanOutputMode) -> FieldValue<u8, FAN_CONFIG1::Register> {
    if system {
        FAN_CONFIG1::SYSTEM_MODE::Manual + FAN_CONFIG1::SYSTEM_DC.val(mode.dc())
    } else {
        FAN_CONFIG1::CPU_MODE::Manual + FAN_CONFIG1::CPU_DC.val(mode.dc())
    }
}

/// Caller must first identify/activate W83627EHG and decode its eight-byte HWM
/// window, then serialize the index/data pair against CPUs, interrupts and any
/// SMBus client. Construction performs no I/O. Runtime IDs are only a sanity
/// check, not a substitute for PnP model identification.
pub struct WinbondEhgMonitor {
    base: u16,
}
impl WinbondEhgMonitor {
    pub fn new(base: u16) -> Result<Self, DeviceError> {
        if !(0x100..0x1000).contains(&base) || base & 7 != 0 {
            return Err(DeviceError::ConfigError);
        }
        Ok(Self { base })
    }
    fn register<R: RegisterLongName>(&self, index: u8) -> IndexedPioRegister<R> {
        IndexedPioRegister::new(self.base + 5, self.base + 6, index)
    }
    // Bank selection affects 50h..5Fh only. Restore defined bank/vendor/beep
    // fields after every operation, including a failed probe; §6.8.46 requires
    // reserved bits 3 and 6 to be zero rather than replayed.
    fn with_bank<T>(&mut self, bank: u8, operation: impl FnOnce(&Self) -> T) -> T {
        let selector = self.register::<BANK_SELECT::Register>(0x4e);
        let saved = selector.get();
        selector.modify(BANK_SELECT::BANK.val(bank) + reserved_bank_fields());
        let result = operation(self);
        selector.set(reserved_bank_fields().modify(saved));
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
            if chip == 0xa1 && u16::from_be_bytes([high, low]) == 0x5ca3 {
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
            // DC values occupy bits 7:2; preserve reserved low bits.
            FanOutputMode::DcVoltage => self
                .register::<DC_OUTPUT::Register>(index)
                .modify(DC_OUTPUT::VALUE.val(63)),
        }
    }
    /// Select manual control before writing the maximum output value, as
    /// required by §6.5.3. Does not need a running conversion loop or sensors.
    /// A probe failure makes no fan-control writes.
    pub fn force_full_speed(&mut self, cooling: &FullSpeedCooling) -> Result<(), DeviceError> {
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
                    .modify(primary_fan_fields(system, mode));
                self.full_output(index, mode);
            }
        }
        if let Some(mode) = cooling.auxiliary {
            self.register::<FAN_CONFIG2::Register>(0x12)
                .modify(FAN_CONFIG2::MODE::Manual + FAN_CONFIG2::DC.val(mode.dc()));
            self.full_output(0x11, mode);
        }
        if let Some(mode) = cooling.cpu_secondary {
            self.register::<FAN_CONFIG3::Register>(0x62)
                .modify(FAN_CONFIG3::MODE::Manual + FAN_CONFIG3::DC.val(mode.dc()));
            self.full_output(0x61, mode);
        }
    }
    pub fn init(&mut self, config: &MonitorConfig) -> Result<(), DeviceError> {
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
        for base in [0x100, 0x290, 0xa00, 0xff8] {
            assert!(WinbondEhgMonitor::new(base).is_ok());
        }
        for base in [0, 0x008, 0x0f8, 0x291, 0x1000, 0xfff8] {
            assert!(matches!(
                WinbondEhgMonitor::new(base),
                Err(DeviceError::ConfigError)
            ));
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
            primary_fan_fields(true, FanOutputMode::DcVoltage).modify(0xff),
            0xf3
        );
        assert_eq!(
            primary_fan_fields(false, FanOutputMode::Pwm).modify(0xff),
            0xcd
        );
        assert_eq!(DC_OUTPUT::VALUE.val(63).modify(3), 0xff);
        assert_eq!(monitoring_fields(true).modify(0xff), 0x75);
        assert_eq!(monitoring_fields(false).modify(0xff), 0x74);
        // Bank changes must not disable beep sources or change vendor byte.
        assert_eq!(
            (BANK_SELECT::BANK.val(4) + reserved_bank_fields()).modify(0xff),
            0xb4
        );
        assert_eq!(reserved_bank_fields().modify(0xff), 0xb7);
    }
}
