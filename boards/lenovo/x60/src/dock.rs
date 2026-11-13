//! ThinkPad X6 UltraBase, the dock of the X60 and X61.
//!
//! The laptop side carries an NSC PC87382 "DLPC" LPC switch (config ports
//! 0x164e/0x164f) with its GPIO block at 0x1680; the dock carries a PC87392
//! SuperIO on the switched LPC segment (0x2e/0x2f) with the dock UART, the
//! parallel port and the dock GPIO block at 0x1620. The dock signals its
//! presence on `SLICE_ON_3M#`, active low; which GPIO carries it is the
//! board's.
//!
//! The X61 carries the same code for the same dock.
//!
//! Ported from coreboot `mainboard/lenovo/x61/dock.c`, which keeps the vendor
//! reset/power order and preserves unrelated DLPC and dock GPIO bits. The
//! older X60 port wrote whole bytes and disconnected the LPC segment before
//! asserting the dock reset; this board retains the vendor sequence.
//!
//! The OS docks and undocks through the dock's ACPI device (`acpi.rs`),
//! whose `_DCK` asks the board's SMI handler through the ICH TCO mailbox;
//! the handler answers with [`smm_dock_command`].

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use fstart_core::services::device::BusDevice;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use fstart_driver_superio::{
    ComPortConfig, ParallelConfig, SuperIoConfig,
    pc87382::{self, Pc87382},
    pc87392::{self, Gpio, Pc87392},
};
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use tock_registers::interfaces::{ReadWriteable, Readable, Writeable};

use fstart_driver_lenovo::ec::Ec;
use fstart_driver_lenovo::h8::{H8, H8DockEvent, H8Led, H8LedMode};

/// Dock commands the board SMM handler accepts through the TCO mailbox.
pub const SMI_DOCK_CONNECT: u8 = 0x01;
pub const SMI_DOCK_DISCONNECT: u8 = 0x02;

const DLPC_INDEX: u16 = 0x164e;
pub const DLPC_SWITCH: u16 = 0x164c;
const DLPC_GPIO: u16 = 0x1680;
const DOCK_INDEX: u16 = 0x002e;
const DOCK_GPIO_BASE: u16 = 0x1620;

// These names describe the X6 wiring, not a universal PC87382/PC87392 pin map.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
tock_registers::register_bitfields![u8,
    /// Laptop GPIO port 0 data out, +0x00 (8-bit): dock reset and power.
    DLPC_OUTPUT [
        D_PLTRST_N OFFSET(0) NUMBITS(1) [],
        DLPC_POWER_ENABLE OFFSET(1) NUMBITS(1) []
    ],
    /// Dock GPIO port 0 data in, +0x01 (8-bit): UltraBay population.
    DOCK_INPUT [ULTRABAY_ABSENT OFFSET(1) NUMBITS(1) []],
    /// Dock GPIO port 0 event enable, +0x02 (8-bit): GP01 and GP07, whose
    /// events assert SMI.
    DOCK_EVENTS [
        GP01 OFFSET(1) NUMBITS(1) [],
        GP07 OFFSET(7) NUMBITS(1) []
    ],
    /// Dock GPIO port 1 data out, +0x04 (8-bit).
    DOCK_OUTPUT [GP16 OFFSET(6) NUMBITS(1) []],
    /// Dock GPIO port 2 data out, +0x08 (8-bit): UltraBay and USB power.
    DOCK_POWER [
        ULTRABAY_ENABLE OFFSET(0) NUMBITS(1) [],
        USB_ENABLE OFFSET(1) NUMBITS(1) []
    ]
];

/// The laptop PC87382's GPIO port 0 data out.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn dlpc_output(
    dlpc: &Pc87382,
) -> Result<impl ReadWriteable<T = u8, R = DLPC_OUTPUT::Register>, Timeout> {
    Ok(dlpc
        .gpio_port0(DLPC_GPIO)
        .map_err(|_| Timeout)?
        .register::<DLPC_OUTPUT::Register>())
}

/// The dock PC87392's active GPIO block.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn dock_gpio() -> Result<Gpio, Timeout> {
    dock(false)?.gpio(DOCK_GPIO_BASE).map_err(|_| Timeout)
}

/// Assert dock reset and remove laptop-side DLPC power, preserving siblings.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn reset_dlpc(output: &impl ReadWriteable<T = u8, R = DLPC_OUTPUT::Register>) {
    output.modify(DLPC_OUTPUT::D_PLTRST_N::CLEAR + DLPC_OUTPUT::DLPC_POWER_ENABLE::CLEAR);
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn delay_ms(ms: u32) {
    for _ in 0..ms {
        fstart_arch::x86::udelay(1000);
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn laptop() -> Result<Pc87382, Timeout> {
    Pc87382::new_at_base(SuperIoConfig::default(), DLPC_INDEX).map_err(|_| Timeout)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn dock(with_parallel: bool) -> Result<Pc87392, Timeout> {
    Pc87392::new_at_base(
        SuperIoConfig {
            com1: Some(ComPortConfig {
                io_base: 0x3f8,
                irq: 4,
                baud_rate: 115200,
            }),
            parallel: with_parallel.then_some(ParallelConfig {
                io_base: 0x3bc,
                irq: 7,
            }),
            ..SuperIoConfig::default()
        },
        DOCK_INDEX,
    )
    .map_err(|_| Timeout)
}

/// A clock or the LPC switch did not come up within its second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timeout;

/// Initialize the laptop-side DLPC switch and its GPIO block. Required even
/// when undocked: the switch is what isolates the dock LPC segment.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub fn dlpc_init() -> Result<(), Timeout> {
    use pc87382::{GpioMode::*, GpioPinConfig};
    let dlpc = laptop()?;
    dlpc.start_14mhz_clock(1000, fstart_arch::udelay)
        .map_err(|_| Timeout)?;
    dlpc.enable_dlpc(DLPC_SWITCH).map_err(|_| Timeout)?;
    dlpc.enable_gpio(DLPC_GPIO).map_err(|_| Timeout)?;
    let pins = [
        (0, Output),
        (1, Output),
        (2, Input),
        (3, Output),
        (4, InputPullUp),
        (20, InputPullUp),
        (21, InputPullUp),
        (23, InputPullUp),
    ]
    .map(|(pin, mode)| GpioPinConfig { pin, mode });
    dlpc.configure_gpio_pins(&pins).map_err(|_| Timeout)?;
    Ok(())
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
pub fn dlpc_init() -> Result<(), Timeout> {
    Ok(())
}

/// Return whether an X6 UltraBase dock is attached, from the board's
/// `SLICE_ON_3M#` pin. A pin that cannot be read means no dock.
pub fn dock_present(pin: &mut impl embedded_hal::digital::InputPin) -> bool {
    pin.is_low().unwrap_or(false)
}

/// Whether the laptop-side LPC switch reports a connected dock.
pub fn connected() -> bool {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    {
        laptop()
            .and_then(|dlpc| dlpc.dlpc_connected(DLPC_SWITCH).map_err(|_| Timeout))
            .unwrap_or(false)
    }
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    {
        false
    }
}

/// Connect the dock-side LPC bus and initialize dock GPIO, power and LDNs.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub fn connect(ec: Ec) -> Result<(), Timeout> {
    let dlpc = laptop()?;
    let output = dlpc_output(&dlpc)?;
    // Start from the vendor state: dock reset asserted and DLPC powered down,
    // preserving unrelated DLPC GPIO outputs. The dock UART sits behind this
    // switch, so marginal sequencing shows up later as serial corruption.
    reset_dlpc(&output);

    dlpc.connect_dlpc(DLPC_SWITCH, 1000, fstart_arch::udelay)
        .map_err(|_| Timeout)?;

    // Power up DLPC while keeping D_PLTRST# asserted, then deassert it.
    output.modify(DLPC_OUTPUT::D_PLTRST_N::CLEAR + DLPC_OUTPUT::DLPC_POWER_ENABLE::SET);
    delay_ms(100);
    output.modify(DLPC_OUTPUT::D_PLTRST_N::SET + DLPC_OUTPUT::DLPC_POWER_ENABLE::SET);
    delay_ms(100);

    use pc87392::{GpioMode::*, GpioPinConfig};
    let mut dock = dock(true)?;
    dock.start_14mhz_clock(1000, fstart_arch::udelay)
        .map_err(|_| Timeout)?;
    // X6 wiring: CLKRUN/DR1/SMI/MTR pin functions, active-high PNF,
    // FDC off, GPIO IRQ routed to SMI. These are complete board-selected images.
    dock.configure_pin_mux(&pc87392::PinMux {
        register24: 0x37,
        register25: 0xa0,
        register26: 0x01,
        register28: 0x02,
    });
    // This board selects the GPIO wiring and event routes.
    let pins = [
        (0, InputDebouncedPullUp, false),
        (1, InputDebouncedPullUp, true),
        (7, InputDebouncedPullUp, true),
        (10, InputDebouncedPullUp, true),
        (2, InputPullUp, false),
        (3, InputPullUp, false),
        (4, InputPullUp, false),
        (5, InputPullUp, false),
        (6, InputPullUp, false),
        (11, InputPullUp, false),
        (12, InputPullUp, false),
        (13, InputPullUp, false),
        (14, InputPullUp, false),
        (15, InputPullUp, false),
        (17, InputPullUp, false),
        (22, InputPullUp, false),
        (23, InputPullUp, false),
        (24, InputPullUp, false),
        (25, InputPullUp, false),
        (26, InputPullUp, false),
        (27, InputPullUp, false),
        (30, InputPullUp, false),
        (31, InputPullUp, false),
        (32, InputPullUp, false),
        (33, InputPullUp, false),
        (34, InputPullUp, false),
        (36, InputPullUp, false),
        (37, InputPullUp, false),
        (16, OutputPullUp, false),
        (20, OutputPushPull, false),
        (21, OutputPushPull, false),
        (35, OutputPullUp, false),
    ]
    .map(|(pin, mode, smi)| GpioPinConfig { pin, mode, smi });
    let gpio = dock
        .enable_gpio(DOCK_GPIO_BASE, &pins)
        .map_err(|_| Timeout)?;

    set_ultrabay_power(&dock_power(&gpio)?, ultrabay_present());
    gpio.event_status::<()>(0).map_err(|_| Timeout)?.set(0x00);
    gpio.event_enable::<DOCK_EVENTS::Register>(0)
        .map_err(|_| Timeout)?
        .write(DOCK_EVENTS::GP01::SET + DOCK_EVENTS::GP07::SET);
    gpio.data_out::<DOCK_OUTPUT::Register>(1)
        .map_err(|_| Timeout)?
        .modify(DOCK_OUTPUT::GP16::SET);
    if !set_usb_power(&gpio, ec, true)? {
        return Err(Timeout);
    }
    dock.init().map_err(|_| Timeout)?;
    dock.disable_watchdog();
    Ok(())
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
pub fn connect(_ec: Ec) -> Result<(), Timeout> {
    Ok(())
}

/// UltraBay population; never sample an absent dock.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub fn ultrabay_present() -> bool {
    connected()
        && dock_gpio()
            .and_then(|gpio| gpio.data_in::<DOCK_INPUT::Register>(0).map_err(|_| Timeout))
            .is_ok_and(|input| !input.is_set(DOCK_INPUT::ULTRABAY_ABSENT))
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
pub fn ultrabay_present() -> bool {
    false
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn set_ultrabay_power(power: &impl ReadWriteable<T = u8, R = DOCK_POWER::Register>, on: bool) {
    power.modify(DOCK_POWER::ULTRABAY_ENABLE.val(on.into()));
}

/// The dock's GPIO port 2 data out.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn dock_power(
    gpio: &Gpio,
) -> Result<impl ReadWriteable<T = u8, R = DOCK_POWER::Register>, Timeout> {
    gpio.data_out::<DOCK_POWER::Register>(2)
        .map_err(|_| Timeout)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn set_usb_power(gpio: &Gpio, ec: Ec, on: bool) -> Result<bool, Timeout> {
    dock_power(gpio)?.modify(DOCK_POWER::USB_ENABLE.val(on.into()));
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
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if connected()
        && let Ok(gpio) = dock_gpio()
    {
        if let Ok(power) = dock_power(&gpio) {
            set_ultrabay_power(&power, present);
        }
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
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub fn disconnect(ec: Ec) {
    let Ok(dlpc) = laptop() else {
        return;
    };
    // Assert D_PLTRST# and DLPCPD before removing power and the LPC segment.
    if let Ok(output) = dlpc_output(&dlpc) {
        reset_dlpc(&output);
    }
    delay_ms(10);
    if let Ok(gpio) = dock_gpio() {
        let _ = set_usb_power(&gpio, ec, false);
        if let Ok(power) = dock_power(&gpio) {
            set_ultrabay_power(&power, false);
        }
    }
    delay_ms(10);
    let _ = dlpc.disconnect_dlpc(DLPC_SWITCH);
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
pub fn disconnect(_ec: Ec) {}

/// Re-enable the dock-side COM1 after a warm stage transition.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
pub fn early_superio_config() {
    if let Ok(mut dock) = dock(false) {
        let _ = dock.start_14mhz_clock(100_000, fstart_arch::udelay);
        let _ = dock.init();
        dock.disable_watchdog();
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
pub fn early_superio_config() {}

/// Route the dock UART before the console probes 0x3f8. Failures are
/// non-fatal: an undocked laptop simply has no serial console.
/// `dock_present_pin` is the board's `SLICE_ON_3M#` pin.
pub fn setup_console(dock_present_pin: &mut impl embedded_hal::digital::InputPin, ec: Ec) {
    let _ = dlpc_init();
    if dock_present(dock_present_pin) {
        let _ = connect(ec);
        early_superio_config();
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
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            fstart_arch::x86::udelay(250_000);
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
            disconnect(ec);
            Some(u8::from(handshake))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn dock_power_and_reset_preserve_other_outputs() {
        use tock_registers::interfaces::Readable;
        use tock_registers::registers::InMemoryRegister;

        let power = InMemoryRegister::<u8, DOCK_POWER::Register>::new(0xfe);
        set_ultrabay_power(&power, true);
        assert_eq!(power.get(), 0xff);
        set_ultrabay_power(&power, false);
        assert_eq!(power.get(), 0xfe);

        let output = InMemoryRegister::<u8, DLPC_OUTPUT::Register>::new(0xff);
        reset_dlpc(&output);
        assert_eq!(output.get(), 0xfc);
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
