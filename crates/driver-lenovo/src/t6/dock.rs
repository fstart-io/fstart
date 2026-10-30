//! T6 Advanced Dock / Ultrabay legacy-I/O module wiring and sequencing.
//! No PC87392 protocol or X6 reset delays are reused for the PC87384.
//! Parallel/IR are deliberately disabled: the reference's LPT 3bch conflicts
//! with its PnP 8-byte resource mask and is not established by a full manual.

use crate::{
    ec::Ec,
    h8::{H8, H8Config, H8Led, H8LedMode},
    pmh7::Pmh7,
};
use fstart_core::{services::ServiceError, services::device::DeviceError};
use fstart_driver_superio::{
    ComPortConfig, GpioConfig, SuperIoConfig,
    pc87382::{self, GpioPort0, Pc87382},
    pc87384::{self, Pc87384},
};

pub const LEGACY_MODULE_GPIO: u8 = 6;
pub const SMI_DOCK_CONNECT: u8 = 1;
pub const SMI_DOCK_DISCONNECT: u8 = 2;
/// Firmware-owned status query; distinct from legacy brightness commands 3/4.
pub const SMI_DOCK_STATUS: u8 = 0x10;

const CONNECTED_LEDS: [(H8Led, H8LedMode); 2] = [
    (H8Led::Dock1, H8LedMode::Off),
    (H8Led::Dock2, H8LedMode::On),
];
const FAILED_LEDS: [(H8Led, H8LedMode); 2] = [
    (H8Led::Dock1, H8LedMode::Blink),
    (H8Led::Dock2, H8LedMode::Off),
];
const IDLE_LEDS: [(H8Led, H8LedMode); 2] = [
    (H8Led::Dock1, H8LedMode::Off),
    (H8Led::Dock2, H8LedMode::Off),
];
const DISCONNECTED_LEDS: [(H8Led, H8LedMode); 2] = [
    (H8Led::Dock2, H8LedMode::Off),
    (H8Led::Dock1, H8LedMode::Off),
];

fn set_dock_leds(h8: &H8, commands: [(H8Led, H8LedMode); 2]) -> bool {
    commands
        .into_iter()
        .map(|(led, mode)| h8.set_led(led, mode))
        .fold(true, |ok, acknowledged| acknowledged && ok)
}
const DLPC_INDEX: u16 = 0x164e;
const DLPC_SWITCH: u16 = 0x164c;
const DLPC_GPIO: u16 = 0x1680;
const DOCK_INDEX: u16 = 0x2e;
const DOCK_GPIO: u16 = 0x1620;

/// Firmware/SMM-owned interfaces, even while the external dock is absent.
/// Shared by PCI allocation policy and the consumed ACPI resource device.
/// EC/PMH7 resources are already described by the shared H8 AML.
pub const FIRMWARE_RESOURCES: [fstart_core::acpi::AcpiResource; 5] = [
    fstart_core::acpi::AcpiResource::Pio {
        base: DOCK_INDEX,
        size: 2,
    },
    fstart_core::acpi::AcpiResource::Pio {
        base: DOCK_GPIO,
        size: 16,
    },
    fstart_core::acpi::AcpiResource::Pio {
        base: DLPC_SWITCH,
        size: 2,
    },
    fstart_core::acpi::AcpiResource::Pio {
        base: DLPC_INDEX,
        size: 2,
    },
    fstart_core::acpi::AcpiResource::Pio {
        base: DLPC_GPIO,
        size: 16,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleRoute {
    Undocked,
    LegacyModule,
    Dock,
}

const LAPTOP_PINS: [pc87382::GpioPinConfig; 8] = {
    use pc87382::{GpioMode::*, GpioPinConfig as Pin};
    [
        Pin {
            pin: 0,
            mode: Output,
        },
        Pin {
            pin: 1,
            mode: Output,
        },
        Pin {
            pin: 2,
            mode: Input,
        },
        Pin {
            pin: 3,
            mode: Output,
        },
        Pin {
            pin: 4,
            mode: InputPullUp,
        },
        Pin {
            pin: 20,
            mode: InputPullUp,
        },
        Pin {
            pin: 21,
            mode: InputPullUp,
        },
        Pin {
            pin: 23,
            mode: InputPullUp,
        },
    ]
};
const DOCK_PINS: [pc87384::GpioPinConfig; 8] = {
    use pc87384::{GpioDirection::*, GpioDrive::*, GpioPinConfig as Pin};
    let input = Pin {
        pin: 0,
        direction: Input,
        drive: OpenDrain,
        pull_up: true,
        debounce: true,
    };
    let output = Pin {
        pin: 1,
        direction: Output,
        drive: PushPull,
        pull_up: false,
        debounce: false,
    };
    [
        input,
        output,
        Pin { pin: 2, ..output },
        Pin { pin: 3, ..input },
        Pin { pin: 4, ..input },
        Pin { pin: 5, ..input },
        Pin { pin: 6, ..input },
        Pin { pin: 7, ..input },
    ]
};

fn laptop() -> Result<Pc87382, DeviceError> {
    Pc87382::new_at_base(SuperIoConfig::default(), DLPC_INDEX)
}
fn delay_us(us: u32) {
    fstart_arch::udelay(us);
}

/// Whether the legacy I/O module is fitted, from the active-low
/// [`LEGACY_MODULE_GPIO`] pin.
pub fn legacy_io_present(
    pin: &mut impl embedded_hal::digital::InputPin,
) -> Result<bool, ServiceError> {
    pin.is_low().map_err(|_| ServiceError::IoError)
}

fn initialize_dlpc() -> Result<GpioPort0, DeviceError> {
    let dlpc = laptop()?;
    dlpc.start_14mhz_clock(1000, delay_us)?;
    dlpc.enable_dlpc(DLPC_SWITCH)?;
    let gpio = dlpc.enable_gpio(DLPC_GPIO)?;
    dlpc.configure_gpio_pins(&LAPTOP_PINS)?;
    Ok(gpio)
}

fn initialize_dock() -> Result<(), DeviceError> {
    let dock = Pc87384::new_at_base(
        SuperIoConfig {
            com1: Some(ComPortConfig {
                io_base: 0x3f8,
                irq: 4,
                baud_rate: 115200,
            }),
            gpio: Some(GpioConfig { io_base: DOCK_GPIO }),
            ..SuperIoConfig::default()
        },
        DOCK_INDEX,
    )?;
    dock.start_14mhz_clock(1000, delay_us)?;
    dock.select_serial_parallel_pads();
    dock.disable_parallel_and_uart2();
    dock.configure_resources()?;
    dock.configure_gpio_pins(&DOCK_PINS)?;
    dock.quiesce_gpio_events()?;
    // Reference FDh: GP01 low, GP02 high. Retain all unlisted input latches.
    dock.gpio_port0()?.set_levels(&[(1, false), (2, true)])
}

/// Called only with an initialized laptop-side DLPC and no legacy-I/O module.
/// Preserve unlisted GPIO latches instead of the reference's whole-byte writes.
fn connect(gpio: &GpioPort0) -> Result<(), DeviceError> {
    let dlpc = laptop()?;
    // Re-enable after a previous timeout deactivated the DLPC LDN.
    dlpc.enable_dlpc(DLPC_SWITCH)?;
    dlpc.connect_dlpc(DLPC_SWITCH, 1000, delay_us)?;
    // Assert D_PLTRST# with DLPC power and reference GP03 high.
    gpio.set_levels(&[(0, false), (1, true), (3, true)])?;
    delay_us(1000);
    gpio.set_level(0, true)?;
    delay_us(10000);
    if let Err(error) = initialize_dock() {
        let _ = disconnect();
        return Err(error);
    }
    Ok(())
}

/// Disconnect in T60 order: gate the LPC segment, then assert reset/power-down.
pub fn disconnect() -> Result<(), DeviceError> {
    let dlpc = laptop()?;
    dlpc.disconnect_dlpc(DLPC_SWITCH)?;
    dlpc.gpio_port0(DLPC_GPIO)?
        .set_levels(&[(0, false), (1, false)])
}

/// The legacy module and external dock share config port 2eh. Fail closed on
/// GPIO read errors and isolate the dock before powering/accessing that module.
/// Failures are non-fatal to a laptop boot without a serial console.
/// `legacy_module` is the [`LEGACY_MODULE_GPIO`] pin.
pub fn setup_console(
    legacy_module: &mut impl embedded_hal::digital::InputPin,
    config: &H8Config,
) -> Result<ConsoleRoute, DeviceError> {
    let legacy = legacy_io_present(legacy_module).map_err(|_| DeviceError::InitFailed)?;
    let gpio = initialize_dlpc()?;
    if legacy {
        Pmh7::new(config.resources.pmh7_base.raw()).ultrabay_power_enable(true);
        delay_us(100000);
        initialize_dock()?;
        Ok(ConsoleRoute::LegacyModule)
    } else if Pmh7::new(config.resources.pmh7_base.raw()).dock_present() {
        connect(&gpio)?;
        Ok(ConsoleRoute::Dock)
    } else {
        Ok(ConsoleRoute::Undocked)
    }
}

pub fn connected() -> bool {
    laptop()
        .and_then(|dlpc| dlpc.dlpc_connected(DLPC_SWITCH))
        .unwrap_or(false)
}

/// Separate IDE and power policy: the legacy module uses the Ultrabay
/// connector but is not a PATA disk. H8 presence controls an ordinary bay disk.
pub const fn ide_primary_enabled(legacy: bool, bay_present: bool) -> bool {
    !legacy && bay_present
}

pub fn mainstage_power_policy(ec: Ec, config: &H8Config, legacy: bool) -> bool {
    let h8 = H8::new(ec);
    let power = if legacy {
        true
    } else {
        // Do not cut power to a populated bay on an EC communication failure.
        let Some(present) = h8.ultrabay_device_present() else {
            return false;
        };
        present
    };
    Pmh7::new(config.resources.pmh7_base.raw()).ultrabay_power_enable(power);
    [
        h8.set_led(
            H8Led::Ultrabay,
            if power { H8LedMode::On } else { H8LedMode::Off },
        ),
        set_dock_leds(
            &h8,
            if connected() {
                CONNECTED_LEDS
            } else {
                IDLE_LEDS
            },
        ),
    ]
    .into_iter()
    .all(core::convert::identity)
}

pub const fn smm_event_command(event: u8) -> Option<u8> {
    match event {
        0x18 | 0x27 | 0x50 => Some(SMI_DOCK_DISCONNECT),
        0x37 => Some(SMI_DOCK_CONNECT),
        _ => None,
    }
}

/// Caller samples GPIO6 under SMM ownership before a connect request. Never
/// connect the external LPC bus while a legacy module owns port 2eh.
pub fn smm_dock_command(ec: Ec, command: u8, legacy: bool) -> Option<u8> {
    let h8 = H8::new(ec);
    match command {
        SMI_DOCK_CONNECT => {
            if legacy {
                return Some(0);
            }
            // Boot tolerates an unavailable console. Runtime reconnect must
            // establish clock, GPIO decode and pin modes again, not manufacture
            // a handle that bypasses a failed boot-stage initialization.
            let success = initialize_dlpc().and_then(|gpio| connect(&gpio)).is_ok();
            let leds = set_dock_leds(&h8, if success { CONNECTED_LEDS } else { FAILED_LEDS });
            Some(u8::from(success && leds))
        }
        SMI_DOCK_DISCONNECT => {
            let success = disconnect().is_ok();
            let leds = set_dock_leds(&h8, DISCONNECTED_LEDS);
            Some(u8::from(success && leds))
        }
        SMI_DOCK_STATUS => Some(u8::from(!legacy && connected())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dock_indicator_commands_match_the_t60_ec_bytes() {
        use crate::h8::LED_CONTROL;
        fn encoded(commands: [(H8Led, H8LedMode); 2]) -> [u8; 2] {
            commands.map(|(led, mode)| {
                (LED_CONTROL::SELECTOR.val(led as u8) + LED_CONTROL::MODE.val(mode as u8)).value
            })
        }
        assert_eq!(encoded(CONNECTED_LEDS), [0x08, 0x89]);
        assert_eq!(encoded(FAILED_LEDS), [0xc8, 0x09]);
        assert_eq!(encoded(IDLE_LEDS), [0x08, 0x09]);
        assert_eq!(encoded(DISCONNECTED_LEDS), [0x09, 0x08]);
        // A legacy module suppresses external-dock status without EC/PnP I/O.
        let ec = Ec::new(crate::ec::EcPorts::new(0x62, 0x66));
        assert_eq!(smm_dock_command(ec, SMI_DOCK_STATUS, true), Some(0));
    }

    #[test]
    fn legacy_module_is_not_an_ide_drive_and_events_are_t6_specific() {
        assert!(!ide_primary_enabled(true, true));
        assert!(!ide_primary_enabled(false, false));
        assert!(ide_primary_enabled(false, true));
        assert_eq!(smm_event_command(0x37), Some(SMI_DOCK_CONNECT));
        for event in [0x18, 0x27, 0x50] {
            assert_eq!(smm_event_command(event), Some(SMI_DOCK_DISCONNECT));
        }
        assert_eq!(smm_event_command(0x58), None);
    }
}
