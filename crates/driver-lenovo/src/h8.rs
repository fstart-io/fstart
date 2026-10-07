//! Lenovo H8 embedded controller on board-supplied command/data ports.
//!
//! Runtime side is a faithful port of coreboot `ec/lenovo/h8`: event
//! enable bits at EC RAM 0x10..0x1f, radio/USB/LED registers, and the
//! hotkey/event configuration word. The ACPI side ports the complete DSDT
//! surface coreboot ships for this EC (`ec.asl`, `battery.asl`, `thermal.asl`,
//! `lid.asl`, `ac.asl`, `sleepbutton.asl`, `beep.asl`, `systemstatus.asl`,
//! `thinkpad.asl`) using the [`fstart_acpi_macros::acpi_dsl!`] macro.

use super::ec::{Ec, EcPorts, EcRegister};
use fstart_core::typed::{Io8, IoAddr};
use tock_registers::{LocalRegisterCopy, register_bitfields};

register_bitfields![u8,
    pub CONFIG0 [
        EVENTS_ENABLE OFFSET(1) NUMBITS(1) [],
        HOTKEY_ENABLE OFFSET(2) NUMBITS(1) [],
        STICKY_FN OFFSET(3) NUMBITS(1) [],
        SECONDARY_CHARGE_FIRST OFFSET(4) NUMBITS(1) [],
        SMM_ENABLE OFFSET(5) NUMBITS(1) [],
        THERMAL_CONTROL OFFSET(7) NUMBITS(1) []
    ],
    pub CONFIG1 [
        BACKLIGHT_LID_CONTROL OFFSET(0) NUMBITS(1) [],
        ILLUMINATION OFFSET(2) NUMBITS(2) [Both = 0, Keyboard = 1, Thinklight = 2, None = 3],
        ULTRABAY_POWER OFFSET(5) NUMBITS(1) []
    ],
    pub CONFIG2 [
        DOCK_USB_POWER OFFSET(0) NUMBITS(1) [],
        DOCK_SPEAKER_MUTE OFFSET(1) NUMBITS(1) [],
        DOCK_SPEAKER_MUTE_POLARITY OFFSET(2) NUMBITS(1) []
    ],
    pub CONFIG3 [
        DOCK_LATCH OFFSET(2) NUMBITS(1) [],
        STICKY_FNLOCK_LED OFFSET(4) NUMBITS(1) []
    ],
    pub LED_CONTROL [
        SELECTOR OFFSET(0) NUMBITS(4) [
            Power = 0, Battery0 = 1, Battery1 = 2, Ultrabay = 4, FnLock = 6,
            Suspend = 7, Dock1 = 8, Dock2 = 9, Logo = 10, AcDc = 12, Mute = 14
        ],
        MODE OFFSET(5) NUMBITS(3) [Off = 0, On = 4, Pulse = 5, Blink = 6]
    ],
    pub TRACKPOINT_CONTROL [MODE OFFSET(0) NUMBITS(2) [Auto = 1, Off = 2, On = 3]],
    // Speed levels and disengaged/automatic modes match Linux thinkpad_acpi.
    pub FAN_CONTROL [
        LEVEL OFFSET(0) NUMBITS(3) [
            Stop = 0, Level1 = 1, Level2 = 2, Level3 = 3,
            Level4 = 4, Level5 = 5, Level6 = 6, Level7 = 7
        ],
        DISENGAGED OFFSET(6) NUMBITS(1) [],
        AUTO OFFSET(7) NUMBITS(1) []
    ],
    pub VOLUME_CONTROL [LEVEL OFFSET(0) NUMBITS(4) [], MUTE OFFSET(6) NUMBITS(1) []],
    // Beep-enable bytes are board policy; only the low-battery event is
    // documented by coreboot. Do not invent names for the other enable bits.
    pub SOUND_ENABLE0 [EVENTS OFFSET(0) NUMBITS(8) [], LOW_BATTERY OFFSET(1) NUMBITS(1) []],
    pub SOUND_ENABLE1 [EVENTS OFFSET(0) NUMBITS(8) []],
    pub SOUND_COMMAND [TONE OFFSET(0) NUMBITS(8) [Silence = 0]],
    pub SOUND_REPEAT [COUNT OFFSET(0) NUMBITS(8) []],
    pub EVENT_ATTENTION [COMMAND OFFSET(0) NUMBITS(8) [DiscardAndEnable = 1]],
    pub RADIO_CONTROL [
        AUDIO_MUTE OFFSET(0) NUMBITS(1) [],
        BLUETOOTH_ENABLE OFFSET(4) NUMBITS(1) [],
        WLAN_ENABLE OFFSET(5) NUMBITS(1) [],
        WWAN_ENABLE OFFSET(6) NUMBITS(1) []
    ],
    pub USB_CONTROL [
        THINKLIGHT OFFSET(1) NUMBITS(1) [],
        POWER_ENABLE OFFSET(4) NUMBITS(1) []
    ],
    pub FN_CONTROL [SWAP_CTRL OFFSET(4) NUMBITS(1) []],
    pub ALWAYS_ON_CONTROL [
        ENABLE OFFSET(0) NUMBITS(1) [],
        AC_ONLY OFFSET(2) NUMBITS(2) [AnyPower = 0, AcOnly = 3],
        KEYBOARD_BACKLIGHT OFFSET(6) NUMBITS(2) [Off = 0, Low = 1, High = 2]
    ],
    pub STATUS1 [
        ULTRABAY_ABSENT OFFSET(0) NUMBITS(1) [],
        ULTRABAY_OFF OFFSET(2) NUMBITS(1) []
    ]
];

// -----------------------------------------------------------------------
// Register map (coreboot h8.h)
// -----------------------------------------------------------------------

// Like PMH7, the namespace owns the index: callers cannot independently
// choose an address and a mismatched field value. The EC handshake stays in Ec.
macro_rules! ec_registers {
    ($($register:ident => $index:literal),* $(,)?) => {
        $(impl EcRegister for $register::Register {
            const INDEX: u8 = $index;
        })*
    };
}

ec_registers! {
    CONFIG0 => 0x00,
    CONFIG1 => 0x01,
    CONFIG2 => 0x02,
    CONFIG3 => 0x03,
    SOUND_ENABLE0 => 0x04,
    SOUND_ENABLE1 => 0x05,
    SOUND_COMMAND => 0x06,
    SOUND_REPEAT => 0x07,
    TRACKPOINT_CONTROL => 0x0b,
    LED_CONTROL => 0x0c,
    ALWAYS_ON_CONTROL => 0x0d,
    FAN_CONTROL => 0x2f,
    VOLUME_CONTROL => 0x30,
    RADIO_CONTROL => 0x3a,
    USB_CONTROL => 0x3b,
    STATUS1 => 0x47,
    EVENT_ATTENTION => 0x80,
    FN_CONTROL => 0xce,
}

pub const H8_CONFIG0_EVENTS_ENABLE: u8 = CONFIG0::EVENTS_ENABLE::SET.value;
pub const H8_CONFIG0_HOTKEY_ENABLE: u8 = CONFIG0::HOTKEY_ENABLE::SET.value;
pub const H8_CONFIG0_SMM_H8_ENABLE: u8 = CONFIG0::SMM_ENABLE::SET.value;
pub const H8_CONFIG0_TC_ENABLE: u8 = CONFIG0::THERMAL_CONTROL::SET.value;

pub use LED_CONTROL::MODE::Value as H8LedMode;
pub use LED_CONTROL::SELECTOR::Value as H8Led;

/// EC queries consumed by firmware dock policy, separate from OS notifications.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum H8DockEvent {
    FnF9 = 0x18,
    AcLost = 0x27,
    DockConnected = 0x37,
    DockDisconnected = 0x50,
    DockConnectedAlternate = 0x58,
}

impl H8DockEvent {
    pub const fn from_query(value: u8) -> Option<Self> {
        if value == Self::FnF9 as u8 {
            Some(Self::FnF9)
        } else if value == Self::AcLost as u8 {
            Some(Self::AcLost)
        } else if value == Self::DockConnected as u8 {
            Some(Self::DockConnected)
        } else if value == Self::DockDisconnected as u8 {
            Some(Self::DockDisconnected)
        } else if value == Self::DockConnectedAlternate as u8 {
            Some(Self::DockConnectedAlternate)
        } else {
            None
        }
    }
}

/// Event-enable masks occupy EC RAM 0x10..0x1f.
const EVENT_ENABLE_BASE: u8 = 0x10;
const EVENT_ENABLE_REGISTERS: usize = 16;

// -----------------------------------------------------------------------
// Runtime driver
// -----------------------------------------------------------------------

/// H8 runtime operations over the caller-selected EC channel.
#[derive(Debug, Clone, Copy)]
pub struct H8 {
    channel: Ec,
}

impl H8 {
    pub const fn new(channel: Ec) -> Self {
        Self { channel }
    }

    /// Clear any stale EC output queue bytes.
    pub fn clear_out_queue(&self) {
        self.channel.clear_out_queue();
    }

    /// Enable one EC event (1..=127) in the event mask registers.
    pub fn enable_event(&self, event: u8) -> bool {
        if event > 127 {
            return false;
        }
        self.channel
            .set_bit(EVENT_ENABLE_BASE + (event >> 3), event & 7)
    }

    /// Disable one EC event.
    pub fn disable_event(&self, event: u8) -> bool {
        if event > 127 {
            return false;
        }
        self.channel
            .clear_bit(EVENT_ENABLE_BASE + (event >> 3), event & 7)
    }

    /// Program all event-enable mask registers from board policy.
    pub fn program_event_masks(&self, masks: &[u8; EVENT_ENABLE_REGISTERS]) -> bool {
        masks
            .iter()
            .enumerate()
            .map(|(index, mask)| self.channel.write(EVENT_ENABLE_BASE + index as u8, *mask))
            .fold(true, |ok, written| written && ok)
    }

    /// Turn the hotkey/event reporting on and enable thermal management,
    /// mirroring coreboot's `h8_enable()` CONFIG0 programming.
    pub fn init_config0(&self, board_config0: u8) -> bool {
        let reg8 = board_config0 | H8_CONFIG0_SMM_H8_ENABLE | H8_CONFIG0_TC_ENABLE;
        self.channel.write_register::<CONFIG0::Register>(reg8) && self.enable_hotkey(true)
    }

    /// Program CONFIG1 illumination, CONFIG2/3, reset power LED and beeper,
    /// and return the fan to EC automatic control (coreboot `h8_enable`).
    pub fn init_controls(
        &self,
        config: [u8; 3],
        illumination: Option<H8Illumination>,
        beep_masks: [u8; 2],
    ) -> bool {
        let config1 = illumination.map_or(config[0], |mode| mode.config1(config[0]));
        [
            self.channel.write_register::<CONFIG1::Register>(config1),
            self.channel.write_register::<CONFIG2::Register>(config[1]),
            self.channel.write_register::<CONFIG3::Register>(config[2]),
            self.set_led(H8Led::Power, H8LedMode::On),
            self.channel
                .write_register::<SOUND_ENABLE0::Register>(beep_masks[0]),
            self.channel
                .write_register::<SOUND_ENABLE1::Register>(beep_masks[1]),
            self.channel
                .write_register::<SOUND_REPEAT::Register>(SOUND_REPEAT::COUNT.val(0).value),
            self.channel
                .write_register::<SOUND_COMMAND::Register>(SOUND_COMMAND::TONE::Silence.value),
            self.channel
                .write_register::<FAN_CONTROL::Register>(FAN_CONTROL::AUTO::SET.value),
        ]
        .into_iter()
        .all(core::convert::identity)
    }

    pub fn enable_hotkey(&self, on: bool) -> bool {
        self.channel
            .modify_register(CONFIG0::HOTKEY_ENABLE.val(on.into()))
    }

    pub fn trackpoint_enable(&self, on: bool) -> bool {
        self.channel.write_register::<TRACKPOINT_CONTROL::Register>(
            if on {
                TRACKPOINT_CONTROL::MODE::On
            } else {
                TRACKPOINT_CONTROL::MODE::Off
            }
            .value,
        )
    }

    /// Controls the radio-off pin in the WLAN MiniPCIe slot.
    pub fn wlan_enable(&self, on: bool) -> bool {
        self.channel
            .modify_register(RADIO_CONTROL::WLAN_ENABLE.val(on.into()))
    }

    /// Controls the radio-off pin of the Bluetooth module.
    pub fn bluetooth_enable(&self, on: bool) -> bool {
        self.channel
            .modify_register(RADIO_CONTROL::BLUETOOTH_ENABLE.val(on.into()))
    }

    /// Controls the radio-off pin of the WWAN MiniPCIe slot.
    pub fn wwan_enable(&self, on: bool) -> bool {
        self.channel
            .modify_register(RADIO_CONTROL::WWAN_ENABLE.val(on.into()))
    }

    pub fn audio_mute(&self, mute: bool) -> bool {
        self.channel
            .modify_register(RADIO_CONTROL::AUDIO_MUTE.val(mute.into()))
    }

    pub fn usb_power_enable(&self, on: bool) -> bool {
        self.channel
            .modify_register(USB_CONTROL::POWER_ENABLE.val(on.into()))
    }

    pub fn fn_ctrl_swap(&self, on: bool) -> bool {
        self.channel
            .modify_register(FN_CONTROL::SWAP_CTRL.val(on.into()))
    }

    /// Sticky Fn without a Fn-lock LED (older ThinkPads, including X61).
    pub fn sticky_fn(&self, on: bool) -> bool {
        self.channel
            .modify_register(CONFIG0::STICKY_FN.val(on.into()))
    }

    pub fn charge_primary_first(&self, primary: bool) -> bool {
        self.channel
            .modify_register(CONFIG0::SECONDARY_CHARGE_FIRST.val((!primary).into()))
    }

    /// Disable USB-always-on, preserving the unrelated EC policy bits.
    pub fn usb_always_on_disable(&self) -> bool {
        self.channel
            .modify_register(ALWAYS_ON_CONTROL::ENABLE::CLEAR + ALWAYS_ON_CONTROL::AC_ONLY.val(0))
    }

    /// Set the volume level (0..=14) and optional mute bit in an exact write.
    /// Unsupported level/flag encodings fail before touching the EC channel.
    pub fn volume(&self, volume: u8) -> bool {
        let value = LocalRegisterCopy::<u8, VOLUME_CONTROL::Register>::new(volume);
        let allowed = (VOLUME_CONTROL::LEVEL.val(u8::MAX) + VOLUME_CONTROL::MUTE::SET).value;
        if volume & !allowed != 0 || value.read(VOLUME_CONTROL::LEVEL) > 14 {
            return false;
        }
        self.channel
            .write_register::<VOLUME_CONTROL::Register>(volume)
    }

    /// Program an exact LED command without reading a write-only control.
    pub fn set_led(&self, led: H8Led, mode: H8LedMode) -> bool {
        self.channel.write_register::<LED_CONTROL::Register>(
            (LED_CONTROL::SELECTOR.val(led as u8) + LED_CONTROL::MODE.val(mode as u8)).value,
        )
    }

    pub fn dock_latch(&self, connected: bool) -> bool {
        self.channel
            .modify_register(CONFIG3::DOCK_LATCH.val(connected.into()))
    }

    /// H8's exact command discards queued events and enables channel attention.
    pub fn reset_event_attention(&self) -> bool {
        self.channel.write_register::<EVENT_ATTENTION::Register>(
            EVENT_ATTENTION::COMMAND::DiscardAndEnable.value,
        )
    }

    /// Issue a beeper command (0..=17, as exposed by thinkpad_acpi).
    pub fn beep(&self, tone: u8) -> bool {
        tone <= 17 && self.channel.write_register::<SOUND_COMMAND::Register>(tone)
    }

    /// True when an ultrabay device is present (active-low STATUS1 flags).
    pub fn ultrabay_device_present(&self) -> Option<bool> {
        let status = self.channel.read_register::<STATUS1::Register>()?;
        Some(status.matches_all(STATUS1::ULTRABAY_ABSENT::CLEAR + STATUS1::ULTRABAY_OFF::CLEAR))
    }
}

/// H8 CONFIG1 bits 2..3 select the active illumination hardware.
#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum H8Illumination {
    Both = 0,
    Keyboard = 1,
    Thinklight = 2,
    None = 3,
}

impl H8Illumination {
    pub fn config1(self, config1: u8) -> u8 {
        let mut value = LocalRegisterCopy::<u8, CONFIG1::Register>::new(config1);
        value.modify(CONFIG1::ILLUMINATION.val(self as u8));
        value.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn illumination_preserves_unrelated_config1_bits() {
        assert_eq!(H8Illumination::Thinklight.config1(0x05), 0x09);
        assert_eq!(H8Illumination::Keyboard.config1(0xf3), 0xf7);
        assert_eq!(H8Illumination::None.config1(0x05), 0x0d);
    }

    #[test]
    fn invalid_volume_and_sound_commands_do_not_touch_the_channel() {
        // Undecoded ports must never be accessed for an invalid command.
        let h8 = H8::new(Ec::new(EcPorts::new(0, 0)));
        for volume in [0x0f, 0x10, 0x4f, 0x80, 0xff] {
            assert!(!h8.volume(volume));
        }
        assert!(!h8.beep(18));
        assert!(!h8.beep(u8::MAX));
    }
}

// -----------------------------------------------------------------------
// Board configuration
// -----------------------------------------------------------------------

/// Board-selected bases; the H8 driver owns the auxiliary register layout.
#[derive(Debug, Clone, Copy)]
pub struct H8Resources {
    pub os: EcPorts,
    pub auxiliary_base: IoAddr<Io8>,
    pub pmh7_base: IoAddr<Io8>,
}

impl H8Resources {
    pub const AUXILIARY_SIZE: u16 = 0x80;
    pub const BATTERY_SIZE: u8 = 0x10;
    pub const PMH7_SIZE: u8 = 0x10;

    pub const fn new(os: EcPorts, auxiliary_base: u16, pmh7_base: u16) -> Self {
        assert!(auxiliary_base <= u16::MAX - (Self::AUXILIARY_SIZE - 1));
        assert!(pmh7_base <= u16::MAX - (Self::PMH7_SIZE as u16 - 1));
        Self {
            os,
            auxiliary_base: IoAddr::new(auxiliary_base),
            pmh7_base: IoAddr::new(pmh7_base),
        }
    }

    pub const fn smm(self) -> EcPorts {
        EcPorts::new(self.auxiliary_base.raw(), self.auxiliary_base.raw() + 4)
    }

    pub const fn gravity(self) -> EcPorts {
        EcPorts::new(self.auxiliary_base.raw() + 2, self.auxiliary_base.raw() + 6)
    }

    pub const fn battery_base(self) -> u16 {
        self.auxiliary_base.raw() + 0x10
    }
}

/// Board-declared H8 capabilities and ACPI knobs (coreboot `chip.h` +
/// Kconfig selections).
#[derive(Debug, Clone, Copy)]
pub struct H8Config {
    pub resources: H8Resources,
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
    /// Minimal capabilities; the board enables its attached devices explicitly.
    #[must_use]
    pub const fn new(resources: H8Resources, ec_gpe: u8, hkey_eisaid: &'static str) -> Self {
        Self {
            resources,
            ec_gpe,
            has_bluetooth: false,
            has_wwan: false,
            has_uwb: false,
            has_thinklight: false,
            has_keyboard_backlight: false,
            has_led_logo: false,
            second_thermal_zone: false,
            bat_info_extended: false,
            bat_charge_behaviour: false,
            bat_thresholds: false,
            alt_fn_f2f3_layout: false,
            hkey_eisaid,
            critical_temp_celsius: 0,
            passive_temp_celsius: 0,
        }
    }
}
