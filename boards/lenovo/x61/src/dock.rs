//! ThinkPad X6 UltraBase, the dock of the X60 and X61.
//!
//! The laptop side carries an NSC PC87382 "DLPC" LPC switch (config ports
//! 0x164e/0x164f) with its GPIO block at 0x1680; the dock carries a PC87392
//! SuperIO on the switched LPC segment (0x2e/0x2f) with the dock UART, the
//! parallel port and the dock GPIO block at 0x1620. The dock signals its
//! presence on `SLICE_ON_3M#`, active low; which GPIO carries it is the
//! board's.
//!
//! The X60 carries the same code for the same dock.
//!
//! Ported from coreboot `mainboard/lenovo/x61/dock.c`, which keeps the vendor
//! reset/power order and preserves unrelated DLPC and dock GPIO bits. The
//! older X60 port wrote whole bytes and disconnected the LPC segment before
//! asserting the dock reset; both boards share the vendor sequence here.
//! Both SuperIOs go through their `fstart-driver-superio` drivers; this
//! module holds the dock's wiring and sequence.
//!
//! The OS docks and undocks through the dock's ACPI device (`acpi.rs`),
//! whose `_DCK` asks the board's SMI handler through the ICH TCO mailbox;
//! the handler answers with [`smm_dock_command`].

use fstart_core::services::device::DeviceError;
use fstart_driver_superio::pc87382::{self, Pc87382};
use fstart_driver_superio::pc87392::{self, Gpio, Pc87392, PinMode};
use fstart_driver_superio::{ComPortConfig, GpioConfig, ParallelConfig, SuperIoConfig};
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use fstart_driver_lenovo::ec::Ec;
use fstart_driver_lenovo::h8::{H8, H8DockEvent, H8Led, H8LedMode};

/// Dock commands the board SMM handler accepts through the TCO mailbox.
pub const SMI_DOCK_CONNECT: u8 = 0x01;
pub const SMI_DOCK_DISCONNECT: u8 = 0x02;

/// The laptop's PC87382 config port, its LPC switch and its GPIO block.
const DLPC_INDEX: u16 = 0x164e;
pub const DLPC_SWITCH: u16 = 0x164c;
const DLPC_GPIO: u16 = 0x1680;
/// The dock's PC87392 config port and its GPIO block.
const DOCK_INDEX: u16 = 0x002e;
const DOCK_GPIO: u16 = 0x1620;

// The SMM handler reaches the connect and disconnect paths: they may not
// address read-only data, so no tables or slices of constants.
tock_registers::register_bitfields![u8,
    /// The laptop PC87382's GPIO port 0 data out.
    DLPC_OUTPUT [
        D_PLTRST_N OFFSET(0) NUMBITS(1) [],
        DLPC_POWER_ENABLE OFFSET(1) NUMBITS(1) []
    ],
    /// The dock PC87392's GPIO port 0 data in.
    DOCK_INPUT [ULTRABAY_ABSENT OFFSET(1) NUMBITS(1) []],
    /// The dock's GPIO port 0 event enable: GP01 and GP07, whose events
    /// assert SMI.
    DOCK_EVENTS [
        GP01 OFFSET(1) NUMBITS(1) [],
        GP07 OFFSET(7) NUMBITS(1) []
    ],
    /// The dock's GPIO port 1 data out.
    DOCK_OUTPUT [GP16 OFFSET(6) NUMBITS(1) []],
    /// The dock's GPIO port 2 data out: GP20 and GP21.
    DOCK_POWER [
        ULTRABAY_ENABLE OFFSET(0) NUMBITS(1) [],
        USB_ENABLE OFFSET(1) NUMBITS(1) []
    ]
];

/// The laptop's GPIO pins.
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

/// The dock's SuperIO configuration: CLKRUN, #DR1, #SMI and #MTR pin
/// functions, PNF active high, the floppy controller off and GPIO events
/// routed to #SMI.
const DOCK_SIO: pc87392::SioConfig = pc87392::SioConfig {
    pin_functions: 0x37,
    parallel: 0xa0,
    fdc: 0x01,
    gpio_routing: 0x02,
};

/// The dock's GPIO pin modes.
const PULL_UP: PinMode = PinMode {
    direction: pc87392::GpioDirection::Input,
    drive: pc87392::GpioDrive::OpenDrain,
    pull_up: true,
    debounce: false,
    smi: false,
};
const DEBOUNCED: PinMode = PinMode {
    debounce: true,
    ..PULL_UP
};
const DEBOUNCED_SMI: PinMode = PinMode {
    smi: true,
    ..DEBOUNCED
};
const PULL_UP_OUTPUT: PinMode = PinMode {
    direction: pc87392::GpioDirection::Output,
    ..PULL_UP
};
const PUSH_PULL_OUTPUT: PinMode = PinMode {
    direction: pc87392::GpioDirection::Output,
    drive: pc87392::GpioDrive::PushPull,
    pull_up: false,
    ..PULL_UP
};

/// The dock's GPIO pins, in the order they are configured, as the
/// physical GPpb given as the decimal `pb`.
fn dock_pins(
    mut configure: impl FnMut(u8, PinMode) -> Result<(), DeviceError>,
) -> Result<(), DeviceError> {
    configure(0, DEBOUNCED)?;
    configure(1, DEBOUNCED_SMI)?;
    configure(7, DEBOUNCED_SMI)?;
    configure(10, DEBOUNCED_SMI)?;
    for pin in (2..=6)
        .chain(11..=15)
        .chain(17..=17)
        .chain(22..=27)
        .chain(30..=34)
    {
        configure(pin, PULL_UP)?;
    }
    configure(36, PULL_UP)?;
    configure(37, PULL_UP)?;
    configure(16, PULL_UP_OUTPUT)?;
    configure(20, PUSH_PULL_OUTPUT)?;
    configure(21, PUSH_PULL_OUTPUT)?;
    configure(35, PULL_UP_OUTPUT)
}

/// Place the dock's GPIO block, configure its pins and activate it.
fn enable_dock_gpio(dock: &Pc87392) -> Result<Gpio, DeviceError> {
    let pins = dock.place_gpio()?;
    dock_pins(|pin, mode| pins.configure(pin, mode))?;
    Ok(pins.enable())
}

fn laptop() -> Result<Pc87382, DeviceError> {
    Pc87382::new_at_base(SuperIoConfig::default(), DLPC_INDEX)
}

/// The dock's PC87392: its COM1 at 0x3f8, its parallel port at 0x3bc and
/// its GPIO block.
fn dock() -> Result<Pc87392, DeviceError> {
    Pc87392::new_at_base(
        SuperIoConfig {
            com1: Some(ComPortConfig {
                io_base: 0x3f8,
                irq: 4,
                baud_rate: 115200,
            }),
            parallel: Some(ParallelConfig {
                io_base: 0x3bc,
                irq: 7,
            }),
            gpio: Some(GpioConfig { io_base: DOCK_GPIO }),
            ..SuperIoConfig::default()
        },
        DOCK_INDEX,
    )
}

/// Assert the dock's reset and remove the DLPC's power, preserving the
/// laptop's other GPIO outputs.
fn reset_dlpc(output: &impl ReadWriteable<T = u8, R = DLPC_OUTPUT::Register>) {
    output.modify(DLPC_OUTPUT::D_PLTRST_N::CLEAR + DLPC_OUTPUT::DLPC_POWER_ENABLE::CLEAR);
}

fn dlpc_output(
    dlpc: &Pc87382,
) -> Result<impl ReadWriteable<T = u8, R = DLPC_OUTPUT::Register>, DeviceError> {
    Ok(dlpc
        .gpio_port0(DLPC_GPIO)?
        .register::<DLPC_OUTPUT::Register>())
}

/// Initialize the laptop-side DLPC switch, disconnected, and its GPIO
/// block. Required even when undocked: the switch is what isolates the dock
/// LPC segment.
pub fn dlpc_init() -> Result<(), DeviceError> {
    let dlpc = laptop()?;
    dlpc.start_14mhz_clock(1000, fstart_arch::udelay)?;
    dlpc.enable_dlpc(DLPC_SWITCH)?;
    dlpc.enable_gpio(DLPC_GPIO)?;
    dlpc.configure_gpio_pins(&LAPTOP_PINS)
}

/// Return whether an X6 UltraBase dock is attached, from the board's
/// `SLICE_ON_3M#` pin. A pin that cannot be read means no dock.
pub fn dock_present(pin: &mut impl embedded_hal::digital::InputPin) -> bool {
    pin.is_low().unwrap_or(false)
}

/// Whether the laptop-side LPC switch reports a connected dock.
pub fn connected() -> bool {
    laptop()
        .and_then(|dlpc| dlpc.dlpc_connected(DLPC_SWITCH))
        .unwrap_or(false)
}

/// Connect the dock-side LPC bus and initialize dock GPIO, power and LDNs.
pub fn connect(ec: Ec) -> Result<(), DeviceError> {
    let dlpc = laptop()?;
    let output = dlpc_output(&dlpc)?;
    // Start from the vendor state: dock reset asserted and DLPC powered down,
    // preserving unrelated DLPC GPIO outputs. The dock UART sits behind this
    // switch, so marginal sequencing shows up later as serial corruption.
    reset_dlpc(&output);
    dlpc.connect_dlpc(DLPC_SWITCH, 1000, fstart_arch::udelay)?;

    // Power up DLPC while keeping D_PLTRST# asserted, then deassert it.
    output.modify(DLPC_OUTPUT::D_PLTRST_N::CLEAR + DLPC_OUTPUT::DLPC_POWER_ENABLE::SET);
    fstart_arch::udelay(100_000);
    output.modify(DLPC_OUTPUT::D_PLTRST_N::SET + DLPC_OUTPUT::DLPC_POWER_ENABLE::SET);
    fstart_arch::udelay(100_000);

    let dock = dock()?;
    dock.start_14mhz_clock(1000, fstart_arch::udelay)?;
    dock.configure_sio(DOCK_SIO);
    let gpio = enable_dock_gpio(&dock)?;

    set_ultrabay_power(&gpio, ultrabay_present())?;
    gpio.event_status::<()>(0)?.set(0x00);
    gpio.event_enable::<DOCK_EVENTS::Register>(0)?
        .write(DOCK_EVENTS::GP01::SET + DOCK_EVENTS::GP07::SET);
    gpio.data_out::<DOCK_OUTPUT::Register>(1)?
        .modify(DOCK_OUTPUT::GP16::SET);
    if !set_usb_power(&gpio, ec, true)? {
        return Err(DeviceError::InitFailed);
    }
    dock.enable_parallel()?;
    dock.enable_com1()?;
    dock.disable_watchdog();
    Ok(())
}

/// UltraBay population; never sample an absent dock.
pub fn ultrabay_present() -> bool {
    connected()
        && dock()
            .and_then(|dock| dock.gpio())
            .and_then(|gpio| gpio.data_in::<DOCK_INPUT::Register>(0))
            .is_ok_and(|input| !input.is_set(DOCK_INPUT::ULTRABAY_ABSENT))
}

fn set_ultrabay_power(gpio: &Gpio, on: bool) -> Result<(), DeviceError> {
    gpio.data_out::<DOCK_POWER::Register>(2)?
        .modify(DOCK_POWER::ULTRABAY_ENABLE.val(on.into()));
    Ok(())
}

/// The dock's USB power, mirrored into the EC. Returns whether the EC
/// acknowledged it.
fn set_usb_power(gpio: &Gpio, ec: Ec, on: bool) -> Result<bool, DeviceError> {
    gpio.data_out::<DOCK_POWER::Register>(2)?
        .modify(DOCK_POWER::USB_ENABLE.val(on.into()));
    Ok(ec.modify_register(fstart_driver_lenovo::h8::CONFIG2::DOCK_USB_POWER.val(on.into())))
}

/// The EC's dock latch after its configuration reset: released, then set
/// again with the dock LED for a dock the stages connected. Returns whether
/// the EC acknowledged it.
pub fn latch_connected_dock(ec: Ec) -> bool {
    let h8 = H8::new(ec);
    h8.dock_latch(false)
        && (!connected() || (h8.dock_latch(true) && h8.set_led(H8Led::Dock1, H8LedMode::On)))
}

/// UltraBay power and LED policy after the H8 configuration reset.
pub fn mainstage_power_policy(ec: Ec) {
    let present = ultrabay_present();
    if connected()
        && let Ok(gpio) = dock().and_then(|dock| dock.gpio())
    {
        let _ = set_ultrabay_power(&gpio, present);
        let _ = set_usb_power(&gpio, ec, true);
    }
    let led = if present {
        H8LedMode::On
    } else {
        H8LedMode::Off
    };
    let _ = H8::new(ec).set_led(H8Led::Ultrabay, led);
}

/// Disconnect the dock-side LPC bus and power rails, in vendor order.
pub fn disconnect(ec: Ec) -> Result<(), DeviceError> {
    let dlpc = laptop()?;
    let gpio = dock()?.gpio()?;
    // Assert D_PLTRST# and DLPCPD before removing power and the LPC segment.
    reset_dlpc(&dlpc_output(&dlpc)?);
    fstart_arch::udelay(10_000);
    let _ = set_usb_power(&gpio, ec, false);
    set_ultrabay_power(&gpio, false)?;
    fstart_arch::udelay(10_000);
    dlpc.disconnect_dlpc(DLPC_SWITCH)
}

/// Re-enable the dock-side COM1 after a warm stage transition.
pub fn early_superio_config() -> Result<(), DeviceError> {
    let dock = dock()?;
    let _ = dock.start_14mhz_clock(100_000, fstart_arch::udelay);
    dock.enable_com1()?;
    dock.disable_watchdog();
    Ok(())
}

/// Route the dock UART before the console probes 0x3f8. Failures are
/// non-fatal: an undocked laptop simply has no serial console.
/// `dock_present_pin` is the board's `SLICE_ON_3M#` pin.
pub fn setup_console(dock_present_pin: &mut impl embedded_hal::digital::InputPin, ec: Ec) {
    let _ = dlpc_init();
    if dock_present(dock_present_pin) {
        let _ = connect(ec);
        let _ = early_superio_config();
    }
}

/// Legacy SMM EC query mapping, distinct from the OS AML hotkey policy.
pub const fn smm_event_command(event: u8) -> Option<u8> {
    match H8DockEvent::from_query(event) {
        Some(H8DockEvent::FnF9 | H8DockEvent::AcLost | H8DockEvent::DockDisconnected) => {
            Some(SMI_DOCK_DISCONNECT)
        }
        Some(H8DockEvent::DockConnected | H8DockEvent::DockConnectedAlternate) => {
            Some(SMI_DOCK_CONNECT)
        }
        None => None,
    }
}

/// Execute a dock command from SMM (ACPI `_DCK` or a legacy EC event) and
/// return its TCO mailbox result, or `None` for a foreign command.
pub fn smm_dock_command(ec: Ec, command: u8) -> Option<u8> {
    let h8 = H8::new(ec);
    match command {
        SMI_DOCK_CONNECT => {
            let handshake = h8.dock_latch(false);
            fstart_arch::udelay(250_000);
            if handshake && connect(ec).is_ok() {
                let state = h8.dock_latch(true);
                let led_off = h8.set_led(H8Led::Dock2, H8LedMode::Off);
                let led_on = h8.set_led(H8Led::Dock1, H8LedMode::On);
                Some(u8::from(state && led_off && led_on))
            } else {
                // Blink the dock LED to report the failure.
                let _ = h8.set_led(H8Led::Dock1, H8LedMode::Off);
                let _ = h8.set_led(H8Led::Dock2, H8LedMode::Blink);
                Some(0)
            }
        }
        SMI_DOCK_DISCONNECT => {
            let handshake = h8.dock_latch(false);
            let _ = disconnect(ec);
            Some(u8::from(handshake))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    /// The pins program the images of coreboot's X61 `dock.c`, in the
    /// order this driver always wrote them: F0h selector, F1h, F2h.
    #[test]
    fn dock_pins_program_the_reference_images() {
        const PULL_UP: u8 = 0x04;
        const OUTPUT: u8 = 0x01;
        const PUSH_PULL: u8 = 0x02;
        const DEBOUNCE: u8 = 0x40;
        const SMI: u8 = 0x02;
        let mut reference = std::vec![(0x00, DEBOUNCE | PULL_UP, 0)];
        for selector in [0x01, 0x07, 0x10] {
            reference.push((selector, DEBOUNCE | PULL_UP, SMI));
        }
        for selector in [
            0x02, 0x03, 0x04, 0x05, 0x06, 0x11, 0x12, 0x13, 0x14, 0x15, 0x17, 0x22, 0x23, 0x24,
            0x25, 0x26, 0x27, 0x30, 0x31, 0x32, 0x33, 0x34, 0x36, 0x37,
        ] {
            reference.push((selector, PULL_UP, 0));
        }
        reference.push((0x16, PULL_UP | OUTPUT, 0));
        reference.push((0x20, PUSH_PULL | OUTPUT, 0));
        reference.push((0x21, PUSH_PULL | OUTPUT, 0));
        reference.push((0x35, PULL_UP | OUTPUT, 0));
        let mut pins = std::vec::Vec::new();
        dock_pins(|pin, mode| {
            pins.push((
                pc87392::pin_selector(pin).unwrap(),
                mode.config_image(),
                mode.event_image(),
            ));
            Ok(())
        })
        .unwrap();
        assert_eq!(pins, reference);
        let laptop: std::vec::Vec<_> = LAPTOP_PINS
            .iter()
            .map(|pin| (((pin.pin / 10) << 4) | (pin.pin % 10), pin.mode as u8))
            .collect();
        assert_eq!(
            laptop,
            [
                (0x00, 3),
                (0x01, 3),
                (0x02, 0),
                (0x03, 3),
                (0x04, 4),
                (0x20, 4),
                (0x21, 4),
                (0x23, 4)
            ]
        );
    }

    #[test]
    fn smm_ec_dock_event_mapping_matches_coreboot() {
        for event in [0x18, 0x27, 0x50] {
            assert_eq!(smm_event_command(event), Some(SMI_DOCK_DISCONNECT));
        }
        for event in [0x37, 0x58] {
            assert_eq!(smm_event_command(event), Some(SMI_DOCK_CONNECT));
        }
        assert_eq!(smm_event_command(0x14), None);
    }
}
