//! ThinkPad X61 mainboard policy for the X6 UltraBase dock.
//!
//! The laptop side carries an NSC PC87382 "DLPC" LPC switch (config ports
//! 0x164e/0x164f) with its GPIO block at 0x1680; the dock carries a PC87392
//! SuperIO on the switched LPC segment (0x2e/0x2f) with the dock UART, the
//! parallel port and the dock GPIO block at 0x1620. ICH GPIO13 reads low with
//! a dock attached on both boards.
//!
//! Ported from coreboot `mainboard/lenovo/x61/dock.c`, which keeps the vendor
//! reset/power order and preserves unrelated DLPC and dock GPIO bits. The
//! older X60 port wrote whole bytes and disconnected the LPC segment before
//! asserting the dock reset; this board retains the vendor sequence.

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use fstart_core::pio::{PioRegister, inb, outb};
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use fstart_core::services::device::BusDevice;
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use fstart_driver_superio::{
    ComPortConfig, ParallelConfig, SuperIoConfig,
    pc87382::{self, Pc87382},
    pc87392::{self, Pc87392},
};
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
use tock_registers::interfaces::{ReadWriteable, Readable};

use fstart_driver_lenovo::ec::Ec;
use fstart_driver_lenovo::h8::{H8, H8DockEvent, H8Led, H8LedMode};

/// Dock commands the board SMM handler accepts through the TCO mailbox.
pub const SMI_DOCK_CONNECT: u8 = 0x01;
pub const SMI_DOCK_DISCONNECT: u8 = 0x02;

/// ICH GPIO sampling dock presence (`SLICE_ON_3M#`, active low). The X60
/// and X61 wire it the same; boards hand [`dock_present`] that pin.
pub const DOCK_PRESENT_GPIO: u8 = 13;

const DLPC_INDEX: u16 = 0x164e;
const DLPC_SWITCH: u16 = 0x164c;
const DLPC_GPIO: u16 = 0x1680;
const DOCK_INDEX: u16 = 0x002e;
const DOCK_GPIO_BASE: u16 = 0x1620;

// These names describe the X6 wiring, not a universal PC87382/PC87392 pin map.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
tock_registers::register_bitfields![u8,
    /// Laptop GPIO offset 0x00 (8-bit): X6 dock reset and power outputs.
    DLPC_OUTPUT [
        D_PLTRST_N OFFSET(0) NUMBITS(1) [],
        DLPC_POWER_ENABLE OFFSET(1) NUMBITS(1) []
    ],
    /// Dock GPIO offset 0x01 (8-bit): X6 UltraBay population input.
    DOCK_INPUT [ULTRABAY_ABSENT OFFSET(1) NUMBITS(1) []],
    /// Dock GPIO offset 0x08 (8-bit): X6 UltraBay and USB power outputs.
    DOCK_POWER [
        ULTRABAY_ENABLE OFFSET(0) NUMBITS(1) [],
        USB_ENABLE OFFSET(1) NUMBITS(1) []
    ]
];

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
const DLPC_OUTPUT: PioRegister<u8, DLPC_OUTPUT::Register> = PioRegister::new(DLPC_GPIO);
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
const DOCK_INPUT: PioRegister<u8, DOCK_INPUT::Register> = PioRegister::new(DOCK_GPIO_BASE + 1);
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
const DOCK_POWER: PioRegister<u8, DOCK_POWER::Register> = PioRegister::new(DOCK_GPIO_BASE + 8);

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

/// Return whether an X6 UltraBase dock is attached, from the active-low
/// [`DOCK_PRESENT_GPIO`] pin. A pin that cannot be read means no dock.
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
    // Start from the vendor state: dock reset asserted and DLPC powered down,
    // preserving unrelated DLPC GPIO outputs. The dock UART sits behind this
    // switch, so marginal sequencing shows up later as serial corruption.
    reset_dlpc(&DLPC_OUTPUT);

    laptop()?
        .connect_dlpc(DLPC_SWITCH, 1000, fstart_arch::udelay)
        .map_err(|_| Timeout)?;

    // Power up DLPC while keeping D_PLTRST# asserted, then deassert it.
    DLPC_OUTPUT.modify(DLPC_OUTPUT::D_PLTRST_N::CLEAR + DLPC_OUTPUT::DLPC_POWER_ENABLE::SET);
    delay_ms(100);
    DLPC_OUTPUT.modify(DLPC_OUTPUT::D_PLTRST_N::SET + DLPC_OUTPUT::DLPC_POWER_ENABLE::SET);
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
    dock.enable_gpio(DOCK_GPIO_BASE, &pins)
        .map_err(|_| Timeout)?;

    set_ultrabay_power(&DOCK_POWER, ultrabay_present());
    // SAFETY: the dock GPIO block is configured at 0x1620.
    unsafe {
        outb(DOCK_GPIO_BASE + 0x03, 0x00);
        outb(DOCK_GPIO_BASE + 0x02, 0x82);
        outb(DOCK_GPIO_BASE + 0x04, inb(DOCK_GPIO_BASE + 0x04) | 0x40);
    }
    if !set_usb_power(ec, true) {
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
    connected() && !DOCK_INPUT.is_set(DOCK_INPUT::ULTRABAY_ABSENT)
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
pub fn ultrabay_present() -> bool {
    false
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn set_ultrabay_power(power: &impl ReadWriteable<T = u8, R = DOCK_POWER::Register>, on: bool) {
    power.modify(DOCK_POWER::ULTRABAY_ENABLE.val(on.into()));
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn set_usb_power(ec: Ec, on: bool) -> bool {
    DOCK_POWER.modify(DOCK_POWER::USB_ENABLE.val(on.into()));
    ec.modify_register(fstart_driver_lenovo::h8::CONFIG2::DOCK_USB_POWER.val(on.into()))
}

/// UltraBay power and LED policy after the H8 configuration reset.
pub fn mainstage_power_policy(ec: Ec) {
    let present = ultrabay_present();
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if connected() {
        set_ultrabay_power(&DOCK_POWER, present);
        let _ = set_usb_power(ec, true);
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
    // Assert D_PLTRST# and DLPCPD before removing power and the LPC segment.
    reset_dlpc(&DLPC_OUTPUT);
    delay_ms(10);
    let _ = set_usb_power(ec, false);
    set_ultrabay_power(&DOCK_POWER, false);
    delay_ms(10);
    if let Ok(dlpc) = laptop() {
        let _ = dlpc.disconnect_dlpc(DLPC_SWITCH);
    }
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
/// `dock_present_pin` is the [`DOCK_PRESENT_GPIO`] pin.
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
