//! Lenovo ThinkPad H8 embedded controller driver.
//!
//! This is the shared ThinkPad H8 policy from coreboot `ec/lenovo/h8`, built on
//! the reusable ACPI EC command transport in `fstart-ec-acpi`.

#![no_std]

use fstart_ec_acpi::AcpiEc;

pub use fstart_ec_acpi::EcPorts;
use fstart_services::device::{Device, DeviceError};
use fstart_services::ServiceError;
use serde::{Deserialize, Serialize};

/// H8 config register 0.
pub const H8_CONFIG0: u8 = 0x00;
/// H8 config0 bit: enable H8 SMM interface.
pub const H8_CONFIG0_SMM_H8_ENABLE: u8 = 0x20;
/// H8 config0 bit: enable thermal control.
pub const H8_CONFIG0_TC_ENABLE: u8 = 0x80;
/// H8 config register 1.
pub const H8_CONFIG1: u8 = 0x01;
/// H8 config register 2.
pub const H8_CONFIG2: u8 = 0x02;
/// H8 config register 3.
pub const H8_CONFIG3: u8 = 0x03;
/// H8 sound mask register 0.
pub const H8_SOUND_ENABLE0: u8 = 0x04;
/// H8 sound mask register 1.
pub const H8_SOUND_ENABLE1: u8 = 0x05;
/// H8 sound register.
pub const H8_SOUND_REG: u8 = 0x06;
/// H8 sound repeat register.
pub const H8_SOUND_REPEAT: u8 = 0x07;
/// H8 trackpoint control register.
pub const H8_TRACKPOINT_CTRL: u8 = 0x0b;
/// Trackpoint on command.
pub const H8_TRACKPOINT_ON: u8 = 0x03;
/// Trackpoint off command.
pub const H8_TRACKPOINT_OFF: u8 = 0x02;
/// H8 LED control register.
pub const H8_LED_CONTROL: u8 = 0x0c;
/// LED on command prefix.
pub const H8_LED_CONTROL_ON: u8 = 0x80;
/// Power LED selector.
pub const H8_LED_CONTROL_POWER_LED: u8 = 0x00;
/// ThinkPad lid logo LED selector.
pub const H8_LED_CONTROL_LOGO_LED: u8 = 0x0a;
/// USB always-on register.
pub const H8_USB_ALWAYS_ON: u8 = 0x0d;
/// USB always-on enable bit.
pub const H8_USB_ALWAYS_ON_ENABLE: u8 = 0x01;
/// USB always-on AC-only bits.
pub const H8_USB_ALWAYS_ON_AC_ONLY: u8 = 0x0c;
/// H8 fan control register.
pub const H8_FAN_CONTROL: u8 = 0x2f;
/// Fan automatic control value.
pub const H8_FAN_CONTROL_AUTO: u8 = 0x80;
/// H8 volume register.
pub const H8_VOLUME_CONTROL: u8 = 0x30;
/// H8 EC firmware version registers.
pub const H8_EC_FIRMWARE_MINOR_VER: u8 = 0xe8;
pub const H8_EC_FIRMWARE_MAJOR_VER: u8 = 0xe9;
pub const H8_EC_FUNC_MINOR_VER: u8 = 0xeb;
pub const H8_EC_FUNC_MAJOR_VER: u8 = 0xef;
pub const H8_EC_BUILD_ID: u8 = 0xf0;

/// USB always-on policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum UsbAlwaysOn {
    /// Disable always-on USB.
    #[default]
    Off,
    /// Enable on AC and battery.
    AcAndBattery,
    /// Enable on AC only.
    AcOnly,
}

/// Lenovo H8 configuration, mirroring coreboot `ec_lenovo_h8_config`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LenovoH8Config {
    /// EC command/data port pair.
    #[serde(default)]
    pub ports: EcPorts,
    pub config0: u8,
    pub config1: u8,
    pub config2: u8,
    pub config3: u8,
    pub beepmask0: u8,
    pub beepmask1: u8,
    /// Event enable registers 0x10..0x1f.
    #[serde(default)]
    pub event_enable: [u8; 16],
    #[serde(default)]
    pub has_thinklight: bool,
    #[serde(default)]
    pub has_keyboard_backlight: bool,
    #[serde(default)]
    pub has_power_management_beeps: bool,
    #[serde(default)]
    pub has_uwb: bool,
    /// Coreboot `H8_SUPPORT_BT_ON_WIFI`: assume Bluetooth can live on WLAN.
    #[serde(default)]
    pub support_bt_on_wifi: bool,
    /// Coreboot `H8_HAS_BDC_GPIO_DETECTION`: use board GPIO policy for BDC detection.
    #[serde(default)]
    pub has_bdc_gpio_detection: bool,
    /// Coreboot `H8_HAS_WWAN_GPIO_DETECTION`: use board GPIO policy for WWAN detection.
    #[serde(default)]
    pub has_wwan_gpio_detection: bool,
    /// Coreboot `H8_HAS_PRIMARY_FN_KEYS`: expose/apply F1..F12 primary-key policy.
    #[serde(default)]
    pub has_primary_fn_keys: bool,
    /// Coreboot `H8_HAS_LEDLOGO`: initialize ThinkPad lid logo LED.
    #[serde(default)]
    pub has_led_logo: bool,
    /// Coreboot `H8_HAS_BAT_THRESHOLDS_IMPL`: ACPI battery threshold methods are valid.
    #[serde(default)]
    pub has_battery_thresholds: bool,
    /// Coreboot `H8_FN_KEY_AS_VBOOT_RECOVERY_SW`: Fn key acts as recovery switch.
    #[serde(default)]
    pub fn_key_as_recovery_sw: bool,
    /// Coreboot `H8_HAS_2ND_THERMAL_ZONE`: board exposes TMP1 as a second thermal zone.
    #[serde(default)]
    pub has_second_thermal_zone: bool,
    #[serde(default)]
    pub bdc_gpio_num: u8,
    #[serde(default)]
    pub bdc_gpio_lvl: u8,
    #[serde(default)]
    pub wwan_gpio_num: u8,
    #[serde(default)]
    pub wwan_gpio_lvl: u8,
    #[serde(default)]
    pub usb_always_on: UsbAlwaysOn,
    #[serde(default = "default_true")]
    pub wlan_enable: bool,
    #[serde(default = "default_true")]
    pub trackpoint_enable: bool,
    #[serde(default = "default_true")]
    pub usb_power_enable: bool,
    #[serde(default = "default_true")]
    pub bluetooth_enable: bool,
    #[serde(default = "default_true")]
    pub wwan_enable: bool,
    #[serde(default = "default_true")]
    pub uwb_enable: bool,
    #[serde(default)]
    pub fn_ctrl_swap: bool,
    #[serde(default)]
    pub sticky_fn: bool,
    /// Apply F1..F12-as-primary if `has_primary_fn_keys` is set.
    #[serde(default = "default_true")]
    pub f1_to_f12_as_primary: bool,
    #[serde(default = "default_true")]
    pub primary_battery_first: bool,
    /// Optional startup volume; coreboot applies this only when not waking S3.
    #[serde(default)]
    pub volume: Option<u8>,
}

fn default_true() -> bool {
    true
}

impl Default for LenovoH8Config {
    fn default() -> Self {
        Self {
            ports: EcPorts::STANDARD,
            config0: 0,
            config1: 0,
            config2: 0,
            config3: 0,
            beepmask0: 0,
            beepmask1: 0,
            event_enable: [0; 16],
            has_thinklight: false,
            has_keyboard_backlight: false,
            has_power_management_beeps: false,
            has_uwb: false,
            support_bt_on_wifi: false,
            has_bdc_gpio_detection: false,
            has_wwan_gpio_detection: false,
            has_primary_fn_keys: false,
            has_led_logo: false,
            has_battery_thresholds: false,
            fn_key_as_recovery_sw: false,
            has_second_thermal_zone: false,
            bdc_gpio_num: 0,
            bdc_gpio_lvl: 0,
            wwan_gpio_num: 0,
            wwan_gpio_lvl: 0,
            usb_always_on: UsbAlwaysOn::Off,
            wlan_enable: true,
            trackpoint_enable: true,
            usb_power_enable: true,
            bluetooth_enable: true,
            wwan_enable: true,
            uwb_enable: true,
            fn_ctrl_swap: false,
            sticky_fn: false,
            f1_to_f12_as_primary: true,
            primary_battery_first: true,
            volume: None,
        }
    }
}

/// Lenovo H8 EC device.
pub struct LenovoH8 {
    config: Option<&'static LenovoH8Config>,
    ec: AcpiEc,
}

impl LenovoH8 {
    /// Construct a temporary H8 handle for the supplied ports.
    pub const fn from_ports(ports: EcPorts) -> Self {
        Self {
            config: None,
            ec: AcpiEc::new(ports),
        }
    }

    /// Standard-port temporary H8 handle.
    pub const fn standard() -> Self {
        Self::from_ports(EcPorts::STANDARD)
    }

    /// Alternate-port temporary H8 handle for SMM-style callers.
    pub const fn alternate() -> Self {
        Self::from_ports(EcPorts::ALT)
    }

    /// Read one H8 EC register.
    pub fn read(&self, reg: u8) -> Result<u8, ServiceError> {
        self.ec.read(reg)
    }

    /// Write one H8 EC register.
    pub fn write(&self, reg: u8, value: u8) -> Result<(), ServiceError> {
        self.ec.write(reg, value)
    }

    /// Set one H8 EC register bit.
    pub fn set_bit(&self, reg: u8, bit: u8) -> Result<(), ServiceError> {
        self.ec.set_bit(reg, bit)
    }

    /// Clear one H8 EC register bit.
    pub fn clear_bit(&self, reg: u8, bit: u8) -> Result<(), ServiceError> {
        self.ec.clear_bit(reg, bit)
    }

    /// Enable or disable dock USB/speaker path bit used by X200 dock connect.
    pub fn dock_connect(&self, on: bool) -> Result<(), ServiceError> {
        if on {
            self.set_bit(H8_CONFIG2, 0)
        } else {
            self.clear_bit(H8_CONFIG2, 0)
        }
    }

    /// Enable or disable trackpoint.
    pub fn trackpoint_enable(&self, on: bool) -> Result<(), ServiceError> {
        self.write(
            H8_TRACKPOINT_CTRL,
            if on {
                H8_TRACKPOINT_ON
            } else {
                H8_TRACKPOINT_OFF
            },
        )
    }

    /// Enable or disable WLAN radio-off pin.
    pub fn wlan_enable(&self, on: bool) -> Result<(), ServiceError> {
        self.set_clear_bit(0x3a, 5, on)
    }

    /// Enable or disable Bluetooth daughter-card power.
    pub fn bluetooth_enable(&self, on: bool) -> Result<(), ServiceError> {
        self.set_clear_bit(0x3a, 4, on)
    }

    /// Enable or disable WWAN radio-off pin.
    pub fn wwan_enable(&self, on: bool) -> Result<(), ServiceError> {
        self.set_clear_bit(0x3a, 6, on)
    }

    /// Enable or disable UWB radio-off pin.
    pub fn uwb_enable(&self, on: bool) -> Result<(), ServiceError> {
        self.set_clear_bit(0x31, 2, on)
    }

    /// Enable or disable audio mute.
    pub fn audio_mute(&self, mute: bool) -> Result<(), ServiceError> {
        self.set_clear_bit(0x3a, 0, mute)
    }

    /// Enable or disable always-powered USB port.
    pub fn usb_always_on_enable(&self, policy: UsbAlwaysOn) -> Result<(), ServiceError> {
        let mut val = self.read(H8_USB_ALWAYS_ON)?;
        match policy {
            UsbAlwaysOn::Off => val &= !(H8_USB_ALWAYS_ON_ENABLE | H8_USB_ALWAYS_ON_AC_ONLY),
            UsbAlwaysOn::AcAndBattery => {
                val |= H8_USB_ALWAYS_ON_ENABLE;
                val &= !H8_USB_ALWAYS_ON_AC_ONLY;
            }
            UsbAlwaysOn::AcOnly => val |= H8_USB_ALWAYS_ON_ENABLE | H8_USB_ALWAYS_ON_AC_ONLY,
        }
        self.write(H8_USB_ALWAYS_ON, val)
    }

    /// Enable or disable main USB power.
    pub fn usb_power_enable(&self, on: bool) -> Result<(), ServiceError> {
        self.set_clear_bit(0x3b, 4, on)
    }

    /// Set Fn/Ctrl swap.
    pub fn fn_ctrl_swap(&self, on: bool) -> Result<(), ServiceError> {
        self.set_clear_bit(0xce, 4, on)
    }

    /// Set sticky Fn.
    pub fn sticky_fn(&self, on: bool) -> Result<(), ServiceError> {
        self.set_clear_bit(0x00, 3, on)
    }

    /// Select whether F1..F12 are primary instead of special hotkeys.
    pub fn f1_to_f12_as_primary(&self, on: bool) -> Result<(), ServiceError> {
        self.set_clear_bit(0x3b, 3, on)
    }

    /// Set battery charge priority.
    pub fn charge_primary_first(&self, primary: bool) -> Result<(), ServiceError> {
        if primary {
            self.clear_bit(0x00, 4)
        } else {
            self.set_bit(0x00, 4)
        }
    }

    fn set_clear_bit(&self, reg: u8, bit: u8, on: bool) -> Result<(), ServiceError> {
        if on {
            self.set_bit(reg, bit)
        } else {
            self.clear_bit(reg, bit)
        }
    }

    fn init_h8(&self) -> Result<(), ServiceError> {
        let config = self.config.ok_or(ServiceError::InvalidParam)?;
        self.ec.clear_out_queue();
        let mut config0 = config.config0 | H8_CONFIG0_SMM_H8_ENABLE | H8_CONFIG0_TC_ENABLE;
        // Preserve coreboot config semantics but avoid silently losing hotkey/events bits.
        config0 |= config.config0;
        self.write(H8_CONFIG0, config0)?;

        let mut config1 = config.config1;
        if config.has_thinklight || config.has_keyboard_backlight {
            // Coreboot defaults option "backlight" to 0, which selects both backlights.
            config1 &= 0xf3;
        }
        self.write(H8_CONFIG1, config1)?;
        // X200 may connect a present dock before full H8 init. Coreboot writes
        // CONFIG2 and then calls the board h8_mb_init() hook; preserving the
        // pre-existing dock bit gives the same final state without duplicating
        // board-specific dock-present GPIO logic in this shared driver.
        let config2 = config.config2 | (self.read(H8_CONFIG2).unwrap_or(0) & 0x01);
        self.write(H8_CONFIG2, config2)?;
        self.write(H8_CONFIG3, config.config3)?;

        self.write(H8_LED_CONTROL, H8_LED_CONTROL_ON | H8_LED_CONTROL_POWER_LED)?;
        if config.has_led_logo {
            self.write(H8_LED_CONTROL, H8_LED_CONTROL_ON | H8_LED_CONTROL_LOGO_LED)?;
        }
        self.write(H8_SOUND_ENABLE0, config.beepmask0)?;
        self.write(H8_SOUND_ENABLE1, config.beepmask1)?;
        self.write(H8_SOUND_REPEAT, 0x00)?;
        self.write(H8_SOUND_REG, 0x00)?;

        for (idx, value) in config.event_enable.iter().copied().enumerate() {
            self.write(0x10 + idx as u8, value)?;
        }
        self.write(H8_FAN_CONTROL, H8_FAN_CONTROL_AUTO)?;
        self.usb_always_on_enable(config.usb_always_on)?;
        self.wlan_enable(config.wlan_enable)?;
        self.trackpoint_enable(config.trackpoint_enable)?;
        self.usb_power_enable(config.usb_power_enable)?;
        // Coreboot uses Kconfig to decide whether GPIO detection exists and CMOS
        // options for user policy. fstart captures both in RON; GPIO detection
        // is board-specific and may be applied by a mainboard hook after the
        // shared EC init, so this shared init keeps the configured policy value.
        let bluetooth_enable = config.bluetooth_enable
            && (config.support_bt_on_wifi
                || !config.has_bdc_gpio_detection
                || config.bdc_gpio_num != 0);
        self.bluetooth_enable(bluetooth_enable)?;
        let wwan_enable =
            config.wwan_enable && (!config.has_wwan_gpio_detection || config.wwan_gpio_num != 0);
        self.wwan_enable(wwan_enable)?;
        if config.has_uwb {
            self.uwb_enable(config.uwb_enable)?;
        }
        self.fn_ctrl_swap(config.fn_ctrl_swap)?;
        self.sticky_fn(config.sticky_fn)?;
        if config.has_primary_fn_keys {
            self.f1_to_f12_as_primary(config.f1_to_f12_as_primary)?;
        }
        self.charge_primary_first(config.primary_battery_first)?;
        self.audio_mute(false)?;
        if let Some(volume) = config.volume {
            self.write(H8_VOLUME_CONTROL, volume)?;
        }
        Ok(())
    }
}

impl Device for LenovoH8 {
    const NAME: &'static str = "lenovo-h8";
    const COMPATIBLE: &'static [&'static str] = &["lenovo,h8-ec"];
    type Config = LenovoH8Config;

    fn new(config: &'static Self::Config) -> Result<Self, DeviceError> {
        Ok(Self {
            config: Some(config),
            ec: AcpiEc::new(config.ports),
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        self.init_h8().map_err(|_| DeviceError::InitFailed)?;
        let fw_major = self.read(H8_EC_FIRMWARE_MAJOR_VER).unwrap_or(0);
        let fw_minor = self.read(H8_EC_FIRMWARE_MINOR_VER).unwrap_or(0);
        let fn_major = self.read(H8_EC_FUNC_MAJOR_VER).unwrap_or(0);
        let fn_minor = self.read(H8_EC_FUNC_MINOR_VER).unwrap_or(0);
        fstart_log::info!(
            "H8: firmware={:#x}.{:#x} func={:#x}.{:#x}",
            fw_major as u32,
            fw_minor as u32,
            fn_major as u32,
            fn_minor as u32
        );
        Ok(())
    }
}
