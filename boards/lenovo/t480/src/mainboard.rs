// SPDX-License-Identifier: MIT OR Apache-2.0
//! ThinkPad T480 board hooks: MEC1653 bootblock init, PMH7 + H8 EC ramstage
//! setup, discrete GPU presence logging, and LPC TIS TPM probing/S3 resume.

use crate::gpio;
use fstart_driver_intel::sunrise_point::driver::IntelSunrisePoint;
use fstart_driver_lenovo::{
    ec::{Ec, EcPorts},
    h8::{EcOemString, H8, H8BatteryThresholdBackend, H8Config, H8Resources},
    mec1653,
    options::Options,
    pmh7::Pmh7,
};

pub const PMH7_BASE: u16 = 0x15e0;

/// T480 H8 EC channel and ACPI configuration from
/// `sklkbl_thinkpad/{Kconfig,devicetree.cb,acpi/ec.asl}`.
pub const H8_CONFIG: H8Config = H8Config {
    resources: H8Resources::new(EcPorts::new(0x0062, 0x0066), 0x1600, PMH7_BASE),
    config: [0xa6, 0x0d, 0xa8, 0xc4],
    beep_masks: [0x00, 0x86],
    event_masks: [
        0x00, 0x00, 0xff, 0xff, 0xd0, 0x3c, 0x00, 0x01, 0x7b, 0xff, 0x00, 0x00, 0xff, 0xff, 0x9d,
        0x00,
    ],
    has_power_management_beeps: false,
    bdc_gpio_level: None,
    ec_gpe: gpio::EC_SCI_GPE,
    wake_gpe: gpio::EC_WAKE_GPE,
    has_bluetooth: true,
    has_wwan: true,
    has_uwb: false,
    has_radio_switch: false,
    has_thinklight: false,
    has_keyboard_backlight: true,
    has_led_logo: true,
    has_fnlock_led: true,
    has_primary_fn_keys: true,
    second_thermal_zone: false,
    bat_info_extended: true,
    bat_charge_behaviour: false,
    bat_thresholds: true,
    bat_threshold_backend: H8BatteryThresholdBackend::RegistersB0,
    alt_fn_f2f3_layout: false,
    extended_fn_layout: true,
    hkey_eisaid: "LEN0268",
    critical_temp_celsius: 100,
    passive_temp_celsius: 90,
};

/// EC options exposed by ThinkPad T480.
pub static EC: fstart_options::OptionSet<Options> = {
    use fstart_driver_lenovo::options::*;
    fstart_options::OptionSet {
        knobs: &[
            FIRST_BATTERY,
            BLUETOOTH,
            WWAN,
            TOUCHPAD,
            WLAN,
            TRACKPOINT,
            FN_CTRL_SWAP,
            STICKY_FN,
            F1_TO_F12_AS_PRIMARY,
            USB_ALWAYS_ON,
        ],
        board: Options::DEFAULT,
    }
};

/// Board identity read from hardware for SMBIOS tables.
#[derive(Default)]
pub struct Identity {
    pub ec_oem_string: Option<EcOemString>,
}

/// Board ACPI state discovered during ramstage.
pub struct Acpi {
    pub keyboard_backlight: bool,
    pub tpm: Option<fstart_driver_tpm::Family>,
}

/// Bootblock early EC initialization (`bootblock_mainboard_early_init`):
/// signals BIOS-alive on port 0x86, enables the EC1 interface, and resets LEDs.
pub fn early() {
    let _ = mec1653::early_init(mec1653::EC0_PORTS);
}

/// Initialize PMH7, H8 EC, and dGPU presence reporting.
pub fn init_devices(
    southbridge: &IntelSunrisePoint,
    options: &Options,
    tpm: Option<fstart_driver_tpm::Family>,
    resume: bool,
) -> (Identity, Acpi) {
    let pmh7 = Pmh7::new(PMH7_BASE);
    pmh7.backlight_enable(true);
    pmh7.dock_event_enable(true);
    pmh7.touchpad_enable(options.touchpad);
    pmh7.trackpoint_enable(options.trackpoint);

    let h8 = H8::new(Ec::new(H8_CONFIG.resources.os));
    let (keyboard_backlight, acknowledged) = h8.init_modern_thinkpad(&H8_CONFIG, options);
    if !acknowledged {
        fstart_log::error!("lenovo-t480: H8 EC initialization incomplete");
    }

    let mut identity = Identity::default();
    if !resume {
        identity.ec_oem_string = h8.smbios_oem_string();
        if identity.ec_oem_string.is_none() {
            fstart_log::error!("lenovo-t480: H8 EC firmware id unavailable");
        }
        detect_dgpu(southbridge);
    }

    (
        identity,
        Acpi {
            keyboard_backlight,
            tpm,
        },
    )
}

/// Log optional discrete GPU presence and VRAM strap size, keeping the dGPU
/// powered off by default (matching coreboot `sklkbl_thinkpad/dgpu.c`).
fn detect_dgpu(southbridge: &IntelSunrisePoint) {
    const VRAM_STR: [&str; 4] = ["1GB", "2GB", "4GB", "N/A"];
    let Ok(absent) = southbridge.read_gpio(gpio::GPIO_DISCRETE_PRESENCE) else {
        return;
    };
    if !absent {
        let id0 = usize::from(
            southbridge
                .read_gpio(gpio::GPIO_DGFX_VRAM_ID0)
                .unwrap_or(false),
        );
        let id1 = usize::from(
            southbridge
                .read_gpio(gpio::GPIO_DGFX_VRAM_ID1)
                .unwrap_or(false),
        );
        let vram = VRAM_STR[id0 | (id1 << 1)];
        fstart_log::info!(
            "lenovo-t480: discrete GPU present ({} VRAM), keeping disabled",
            vram
        );
    }
}
