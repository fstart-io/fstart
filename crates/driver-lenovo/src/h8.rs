//! Lenovo H8 embedded controller (IO 0x62 data / 0x66 command).
//!
//! Runtime side is a faithful port of coreboot `ec/lenovo/h8`: event
//! enable bits at EC RAM 0x10..0x1f, radio/USB/LED registers, and the
//! hotkey/event configuration word. The ACPI side ports the complete DSDT
//! surface coreboot ships for this EC (`ec.asl`, `battery.asl`, `thermal.asl`,
//! `lid.asl`, `ac.asl`, `sleepbutton.asl`, `beep.asl`, `systemstatus.asl`,
//! `thinkpad.asl`) using the [`fstart_acpi_macros::acpi_dsl!`] macro.

use super::ec::{ec_clr_bit, ec_read, ec_set_bit, ec_write};

// -----------------------------------------------------------------------
// Register map (coreboot h8.h)
// -----------------------------------------------------------------------

pub const H8_CONFIG0: u8 = 0x00;
pub const H8_CONFIG0_EVENTS_ENABLE: u8 = 0x02;
pub const H8_CONFIG0_HOTKEY_ENABLE: u8 = 0x04;
pub const H8_CONFIG0_SMM_H8_ENABLE: u8 = 0x20;
pub const H8_CONFIG0_TC_ENABLE: u8 = 0x80;

pub const H8_CONFIG1: u8 = 0x01;
pub const H8_CONFIG2: u8 = 0x02;
pub const H8_CONFIG3: u8 = 0x03;

pub const H8_SOUND_REG: u8 = 0x06;

pub const H8_TRACKPOINT_CTRL: u8 = 0x0b;
pub const H8_TRACKPOINT_AUTO: u8 = 0x01;
pub const H8_TRACKPOINT_OFF: u8 = 0x02;
pub const H8_TRACKPOINT_ON: u8 = 0x03;

/// LED control register; high bit = on, low nibble selects the LED.
pub const H8_LED_CONTROL: u8 = 0x0c;
pub const H8_LED_CONTROL_OFF: u8 = 0x00;
pub const H8_LED_CONTROL_ON: u8 = 0x80;
pub const H8_LED_CONTROL_PULSE: u8 = 0xa0;
pub const H8_LED_CONTROL_BLINK: u8 = 0xc0;

pub const H8_LED_CONTROL_POWER_LED: u8 = 0x00;
pub const H8_LED_CONTROL_BAT0_LED: u8 = 0x01;
pub const H8_LED_CONTROL_BAT1_LED: u8 = 0x02;
pub const H8_LED_CONTROL_UBAY_LED: u8 = 0x04;
pub const H8_LED_CONTROL_SUSPEND_LED: u8 = 0x07;
pub const H8_LED_CONTROL_MUTE_LED: u8 = 0x0e;

/// Event-enable masks occupy EC RAM 0x10..0x1f.
const EVENT_ENABLE_BASE: u8 = 0x10;
const EVENT_ENABLE_REGISTERS: usize = 16;

// -----------------------------------------------------------------------
// Runtime driver
// -----------------------------------------------------------------------

/// Handle for the fixed-address H8 EC.
#[derive(Debug, Clone, Copy)]
pub struct H8;

impl Default for H8 {
    fn default() -> Self {
        Self
    }
}

impl H8 {
    /// Clear any stale EC output queue bytes.
    pub fn clear_out_queue(&self) {
        let mut timeout_us = RECV_TIMEOUT_US;
        while timeout_us > 0 {
            // SAFETY: fixed EC status port.
            let sc = unsafe { fstart_core::pio::inb(0x66) };
            if sc & EC_OBF_FLAG == 0 {
                return;
            }
            // SAFETY: fixed EC data port.
            let _garbage = unsafe { fstart_core::pio::inb(0x62) };
            fstart_arch::udelay(1);
            timeout_us = timeout_us.saturating_sub(1);
        }
        fstart_log::info!("lenovo-h8: timeout clearing EC output queue");
    }

    /// Enable one EC event (1..=127) in the event mask registers.
    pub fn enable_event(&self, event: u8) -> bool {
        if event > 127 {
            return false;
        }
        ec_set_bit(EVENT_ENABLE_BASE + (event >> 3), event & 7)
    }

    /// Disable one EC event.
    pub fn disable_event(&self, event: u8) -> bool {
        if event > 127 {
            return false;
        }
        ec_clr_bit(EVENT_ENABLE_BASE + (event >> 3), event & 7)
    }

    /// Program all event-enable mask registers from board policy.
    pub fn program_event_masks(&self, masks: &[u8; EVENT_ENABLE_REGISTERS]) -> bool {
        masks
            .iter()
            .enumerate()
            .map(|(index, mask)| ec_write(EVENT_ENABLE_BASE + index as u8, *mask))
            .fold(true, |ok, written| written && ok)
    }

    /// Turn the hotkey/event reporting on and enable thermal management,
    /// mirroring coreboot's `h8_enable()` CONFIG0 programming.
    pub fn init_config0(&self, board_config0: u8) -> bool {
        let reg8 = board_config0 | H8_CONFIG0_SMM_H8_ENABLE | H8_CONFIG0_TC_ENABLE;
        ec_write(H8_CONFIG0, reg8) && self.enable_hotkey(true)
    }

    pub fn enable_hotkey(&self, on: bool) -> bool {
        let Some(val) = ec_read(H8_CONFIG0) else {
            return false;
        };
        if on {
            ec_write(H8_CONFIG0, val | H8_CONFIG0_HOTKEY_ENABLE)
        } else {
            ec_write(H8_CONFIG0, val & !H8_CONFIG0_HOTKEY_ENABLE)
        }
    }

    pub fn trackpoint_enable(&self, on: bool) -> bool {
        ec_write(
            H8_TRACKPOINT_CTRL,
            if on {
                H8_TRACKPOINT_ON
            } else {
                H8_TRACKPOINT_OFF
            },
        )
    }

    /// Controls the radio-off pin in the WLAN MiniPCIe slot.
    pub fn wlan_enable(&self, on: bool) -> bool {
        if on {
            ec_set_bit(0x3a, 5)
        } else {
            ec_clr_bit(0x3a, 5)
        }
    }

    /// Controls the radio-off pin of the Bluetooth module.
    pub fn bluetooth_enable(&self, on: bool) -> bool {
        if on {
            ec_set_bit(0x3a, 4)
        } else {
            ec_clr_bit(0x3a, 4)
        }
    }

    /// Controls the radio-off pin of the WWAN MiniPCIe slot.
    pub fn wwan_enable(&self, on: bool) -> bool {
        if on {
            ec_set_bit(0x3a, 6)
        } else {
            ec_clr_bit(0x3a, 6)
        }
    }

    pub fn audio_mute(&self, mute: bool) -> bool {
        if mute {
            ec_set_bit(0x3a, 0)
        } else {
            ec_clr_bit(0x3a, 0)
        }
    }

    pub fn usb_power_enable(&self, on: bool) -> bool {
        if on {
            ec_set_bit(0x3b, 4)
        } else {
            ec_clr_bit(0x3b, 4)
        }
    }

    /// Program the LED control register (`mode` = ON/PULSE/BLINK plus LED id).
    pub fn led_control(&self, mode: u8) -> bool {
        ec_write(H8_LED_CONTROL, mode)
    }

    /// Write a byte to the beeper sound register.
    pub fn beep(&self, tone: u8) -> bool {
        ec_write(H8_SOUND_REG, tone)
    }

    /// True when an ultrabay device is present (H8_STATUS1 polarity).
    pub fn ultrabay_device_present(&self) -> Option<bool> {
        let status1 = ec_read(0x30)?;
        Some(status1 & 0x5 == 0)
    }
}

const EC_OBF_FLAG: u8 = 0x01;
const RECV_TIMEOUT_US: u32 = 10_000;

// -----------------------------------------------------------------------
// Board configuration
// -----------------------------------------------------------------------

/// Board-declared H8 capabilities and ACPI knobs (coreboot `chip.h` +
/// Kconfig selections).
#[derive(Debug, Clone, Copy)]
pub struct H8Config {
    /// EC query GPE used for `Name(_GPE)` (X61: 0x12).
    pub ec_gpe: u8,
    pub has_bluetooth: bool,
    pub has_wwan: bool,
    pub has_uwb: bool,
    pub has_thinklight: bool,
    pub has_keyboard_backlight: bool,
    /// Red "i-dot" logo LED on the display lid.
    pub has_led_logo: bool,
    /// Second thermal zone (TMP1), e.g. X61 with discrete dGPU-less models
    /// still select it via `H8_HAS_2ND_THERMAL_ZONE`.
    pub second_thermal_zone: bool,
    /// `_BIX`/cycle-count support (`H8_HAS_BAT_INFO_EXTENDED`).
    pub bat_info_extended: bool,
    /// Charge-behaviour methods (`H8_HAS_BAT_CHARGE_BEHAVIOUR`).
    pub bat_charge_behaviour: bool,
    /// Charge-threshold methods (`H8_HAS_BAT_THRESHOLDS_IMPL`).
    pub bat_thresholds: bool,
    /// Alternative Fn-F2/Fn-F3 hotkey layout.
    pub alt_fn_f2f3_layout: bool,
    /// Hotkey hub EISAID ("IBM0068" on all models up to *61; "LEN0068" later).
    pub hkey_eisaid: &'static str,
    /// Critical temperature in degrees Celsius reported by `\TCRT`
    /// (0 lets the ASL fallback to 127 apply).
    pub critical_temp_celsius: u32,
    /// Passive trip temperature reported by `\TPSV`
    /// (0 lets the ASL fallback to 95 apply).
    pub passive_temp_celsius: u32,
}

impl H8Config {
    /// ThinkPad X61 defaults (matches the board's coreboot Kconfig picks).
    #[must_use]
    pub const fn x61() -> Self {
        Self {
            ec_gpe: 0x12,
            has_bluetooth: true,
            has_wwan: false,
            has_uwb: false,
            has_thinklight: true,
            has_keyboard_backlight: false,
            has_led_logo: false,
            second_thermal_zone: true,
            bat_info_extended: false,
            bat_charge_behaviour: false,
            bat_thresholds: false,
            alt_fn_f2f3_layout: false,
            hkey_eisaid: "IBM0068",
            critical_temp_celsius: 0,
            passive_temp_celsius: 0,
        }
    }
}
