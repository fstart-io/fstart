//! ThinkPad X6 UltraBase dock of the X60 and X61.
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
//! asserting the dock reset; both boards share the vendor sequence here.

#[cfg(target_arch = "x86_64")]
use fstart_core::pio::{PioRegister, inb, outb};
#[cfg(target_arch = "x86_64")]
use fstart_driver_superio::{IoResource, IrqResource, LogicalDevice};
#[cfg(target_arch = "x86_64")]
use tock_registers::interfaces::{ReadWriteable, Readable};

use crate::ec::Ec;
use crate::h8::{H8, H8DockEvent, H8Led, H8LedMode};

/// Dock commands the board SMM handler accepts through the TCO mailbox.
pub const SMI_DOCK_CONNECT: u8 = 0x01;
pub const SMI_DOCK_DISCONNECT: u8 = 0x02;

/// ICH GPIO sampling dock presence (`SLICE_ON_3M#`, active low).
pub const DOCK_PRESENT_GPIO: u32 = 13;

const DLPC_INDEX: u16 = 0x164e;
const DLPC_DATA: u16 = 0x164f;
const DLPC_SWITCH: u16 = 0x164c;
const DLPC_GPIO: u16 = 0x1680;
const DOCK_INDEX: u16 = 0x002e;
const DOCK_DATA: u16 = 0x002f;
const DOCK_GPIO_BASE: u16 = 0x1620;

// These names describe the X6 wiring, not a universal PC87382/PC87392 pin map.
#[cfg(target_arch = "x86_64")]
tock_registers::register_bitfields![u8,
    DLPC_OUTPUT [
        D_PLTRST_N OFFSET(0) NUMBITS(1) [],
        DLPC_POWER_ENABLE OFFSET(1) NUMBITS(1) []
    ],
    DLPC_STATUS [CONNECTED OFFSET(3) NUMBITS(1) []],
    DOCK_INPUT [ULTRABAY_ABSENT OFFSET(1) NUMBITS(1) []],
    DOCK_POWER [
        ULTRABAY_ENABLE OFFSET(0) NUMBITS(1) [],
        USB_ENABLE OFFSET(1) NUMBITS(1) []
    ]
];

#[cfg(target_arch = "x86_64")]
const DLPC_OUTPUT: PioRegister<u8, DLPC_OUTPUT::Register> = PioRegister::new(DLPC_GPIO);
#[cfg(target_arch = "x86_64")]
const DLPC_STATUS: PioRegister<u8, DLPC_STATUS::Register> = PioRegister::new(DLPC_SWITCH);
#[cfg(target_arch = "x86_64")]
const DOCK_INPUT: PioRegister<u8, DOCK_INPUT::Register> = PioRegister::new(DOCK_GPIO_BASE + 1);
#[cfg(target_arch = "x86_64")]
const DOCK_POWER: PioRegister<u8, DOCK_POWER::Register> = PioRegister::new(DOCK_GPIO_BASE + 8);

/// Assert dock reset and remove laptop-side DLPC power, preserving siblings.
#[cfg(target_arch = "x86_64")]
fn reset_dlpc(output: &impl ReadWriteable<T = u8, R = DLPC_OUTPUT::Register>) {
    output.modify(DLPC_OUTPUT::D_PLTRST_N::CLEAR + DLPC_OUTPUT::DLPC_POWER_ENABLE::CLEAR);
}

const PC87392_GPIO_PIN_OE: u8 = 0x01;
const PC87392_GPIO_PIN_TYPE_PUSH_PULL: u8 = 0x02;
const PC87392_GPIO_PIN_PULLUP: u8 = 0x04;
const PC87392_GPIO_PIN_DEBOUNCE: u8 = 0x40;
const PC87392_GPIO_PIN_TRIGGERS_SMI: u8 = 0x02;

#[cfg(target_arch = "x86_64")]
fn delay_ms(ms: u32) {
    for _ in 0..ms {
        fstart_arch::x86::udelay(1000);
    }
}

#[cfg(target_arch = "x86_64")]
fn dlpc_write(reg: u8, value: u8) {
    // SAFETY: fixed laptop-side NSC PC87382 PnP config ports decoded by the ICH LPC setup.
    unsafe {
        outb(DLPC_INDEX, reg);
        outb(DLPC_DATA, value);
    }
}

#[cfg(target_arch = "x86_64")]
fn dlpc_read(reg: u8) -> u8 {
    // SAFETY: fixed laptop-side NSC PC87382 PnP config ports decoded by the ICH LPC setup.
    unsafe {
        outb(DLPC_INDEX, reg);
        inb(DLPC_DATA)
    }
}

#[cfg(target_arch = "x86_64")]
fn dock_write(reg: u8, value: u8) {
    // SAFETY: fixed dock-side PC87392 PnP config ports decoded after DLPC connect.
    unsafe {
        outb(DOCK_INDEX, reg);
        outb(DOCK_DATA, value);
    }
}

#[cfg(target_arch = "x86_64")]
fn dock_read(reg: u8) -> u8 {
    // SAFETY: fixed dock-side PC87392 PnP config ports decoded after DLPC connect.
    unsafe {
        outb(DOCK_INDEX, reg);
        inb(DOCK_DATA)
    }
}

/// Poll `ready` once per millisecond; `false` after `timeout_ms`.
#[cfg(target_arch = "x86_64")]
fn wait_ms(timeout_ms: u32, ready: impl Fn() -> bool) -> bool {
    (0..timeout_ms).any(|_| {
        if ready() {
            return true;
        }
        fstart_arch::x86::udelay(1000);
        false
    }) || ready()
}

#[cfg(target_arch = "x86_64")]
fn dlpc_gpio_set_mode(port: u8, mode: u8) {
    dlpc_write(0xf0, port);
    dlpc_write(0xf1, mode);
}

#[cfg(target_arch = "x86_64")]
fn dock_gpio_set_mode(port: u8, mode: u8, irq: u8) {
    dock_write(0xf0, port);
    dock_write(0xf1, mode);
    dock_write(0xf2, irq);
}

#[cfg(target_arch = "x86_64")]
fn dlpc_gpio_init() {
    let mut gpio = LogicalDevice::select(0x07, dlpc_read, dlpc_write);
    gpio.set_io_base(IoResource::Primary(DLPC_GPIO));
    gpio.set_enabled(true);
    for (port, mode) in [
        (0x00, 3),
        (0x01, 3),
        (0x02, 0),
        (0x03, 3),
        (0x04, 4),
        (0x20, 4),
        (0x21, 4),
        (0x23, 4),
    ] {
        dlpc_gpio_set_mode(port, mode);
    }
}

/// Initialize the laptop-side DLPC switch and its GPIO block. Required even
/// when undocked: the switch is what isolates the dock LPC segment.
#[cfg(target_arch = "x86_64")]
pub fn dlpc_init() -> Result<(), ()> {
    // Enable the 14.318 MHz clock on CLKIN.
    dlpc_write(0x29, 0xa0);
    if !wait_ms(1000, || dlpc_read(0x29) & 0x10 != 0) {
        return Err(());
    }
    let mut switch = LogicalDevice::select(0x19, dlpc_read, dlpc_write);
    switch.set_io_base(IoResource::Primary(DLPC_SWITCH));
    switch.set_enabled(true);
    dlpc_gpio_init();
    Ok(())
}

#[cfg(not(target_arch = "x86_64"))]
pub fn dlpc_init() -> Result<(), ()> {
    Ok(())
}

/// Return whether an X6 UltraBase dock is attached.
pub fn dock_present(southbridge: &impl fstart_core::services::Southbridge) -> bool {
    southbridge
        .gpio_get(DOCK_PRESENT_GPIO)
        .is_ok_and(|high| !high)
}

/// Whether the laptop-side LPC switch reports a connected dock.
pub fn connected() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        DLPC_STATUS.is_set(DLPC_STATUS::CONNECTED)
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Connect the dock-side LPC bus and initialize dock GPIO, power and LDNs.
#[cfg(target_arch = "x86_64")]
pub fn connect(ec: Ec) -> Result<(), ()> {
    // Start from the vendor state: dock reset asserted and DLPC powered down,
    // preserving unrelated DLPC GPIO outputs. The dock UART sits behind this
    // switch, so marginal sequencing shows up later as serial corruption.
    reset_dlpc(&DLPC_OUTPUT);

    // SAFETY: the DLPC switch I/O base was activated by dlpc_init().
    unsafe { outb(DLPC_SWITCH, 0x07) };
    if !wait_ms(1000, connected) {
        // SAFETY: disable the DLPC switch again on failure.
        unsafe { outb(DLPC_SWITCH, 0x00) };
        LogicalDevice::select(0x19, dlpc_read, dlpc_write).set_enabled(false);
        return Err(());
    }

    // Power up DLPC while keeping D_PLTRST# asserted, then deassert it.
    DLPC_OUTPUT.modify(DLPC_OUTPUT::D_PLTRST_N::CLEAR + DLPC_OUTPUT::DLPC_POWER_ENABLE::SET);
    delay_ms(100);
    DLPC_OUTPUT.modify(DLPC_OUTPUT::D_PLTRST_N::SET + DLPC_OUTPUT::DLPC_POWER_ENABLE::SET);
    delay_ms(100);

    // Start the dock 14.318 MHz clock and wait for it to settle.
    dock_write(0x29, 0x06);
    if !wait_ms(1000, || dock_read(0x29) & 0x08 != 0) {
        return Err(());
    }

    // CLKRUN/#DR1/#SMI/#MTR pin functions, PNF active high, FDC off,
    // GPIO IRQ routed to #SMI.
    dock_write(0x24, 0x37);
    dock_write(0x25, 0xa0);
    dock_write(0x26, 0x01);
    dock_write(0x28, 0x02);
    let mut gpio = LogicalDevice::select(0x07, dock_read, dock_write);
    gpio.set_io_base(IoResource::Primary(DOCK_GPIO_BASE));

    let debounced_pullup = PC87392_GPIO_PIN_DEBOUNCE | PC87392_GPIO_PIN_PULLUP;
    let push_pull_out = PC87392_GPIO_PIN_TYPE_PUSH_PULL | PC87392_GPIO_PIN_OE;
    let pullup_out = PC87392_GPIO_PIN_PULLUP | PC87392_GPIO_PIN_OE;
    dock_gpio_set_mode(0x00, debounced_pullup, 0x00);
    for port in [0x01, 0x07, 0x10] {
        dock_gpio_set_mode(port, debounced_pullup, PC87392_GPIO_PIN_TRIGGERS_SMI);
    }
    for port in [
        0x02, 0x03, 0x04, 0x05, 0x06, 0x11, 0x12, 0x13, 0x14, 0x15, 0x17, 0x22, 0x23, 0x24, 0x25,
        0x26, 0x27, 0x30, 0x31, 0x32, 0x33, 0x34, 0x36, 0x37,
    ] {
        dock_gpio_set_mode(port, PC87392_GPIO_PIN_PULLUP, 0x00);
    }
    dock_gpio_set_mode(0x16, pullup_out, 0x00);
    dock_gpio_set_mode(0x20, push_pull_out, 0x00);
    dock_gpio_set_mode(0x21, push_pull_out, 0x00);
    dock_gpio_set_mode(0x35, pullup_out, 0x00);
    gpio.set_enabled(true);

    set_ultrabay_power(&DOCK_POWER, ultrabay_present());
    // SAFETY: the dock GPIO block is configured at 0x1620.
    unsafe {
        outb(DOCK_GPIO_BASE + 0x03, 0x00);
        outb(DOCK_GPIO_BASE + 0x02, 0x82);
        outb(DOCK_GPIO_BASE + 0x04, inb(DOCK_GPIO_BASE + 0x04) | 0x40);
    }
    if !set_usb_power(ec, true) {
        return Err(());
    }
    let mut parallel = LogicalDevice::select(0x01, dock_read, dock_write);
    parallel.set_io_base(IoResource::Primary(0x3bc));
    parallel.set_irq(IrqResource::Primary(7));
    parallel.set_enabled(true);
    enable_dock_console();
    disable_dock_watchdog();
    Ok(())
}

#[cfg(not(target_arch = "x86_64"))]
pub fn connect(_ec: Ec) -> Result<(), ()> {
    Ok(())
}

/// UltraBay population; never sample an absent dock.
#[cfg(target_arch = "x86_64")]
pub fn ultrabay_present() -> bool {
    connected() && !DOCK_INPUT.is_set(DOCK_INPUT::ULTRABAY_ABSENT)
}

#[cfg(not(target_arch = "x86_64"))]
pub fn ultrabay_present() -> bool {
    false
}

#[cfg(target_arch = "x86_64")]
fn set_ultrabay_power(power: &impl ReadWriteable<T = u8, R = DOCK_POWER::Register>, on: bool) {
    power.modify(DOCK_POWER::ULTRABAY_ENABLE.val(on.into()));
}

#[cfg(target_arch = "x86_64")]
fn set_usb_power(ec: Ec, on: bool) -> bool {
    DOCK_POWER.modify(DOCK_POWER::USB_ENABLE.val(on.into()));
    ec.modify_register(crate::h8::CONFIG2::DOCK_USB_POWER.val(on.into()))
}

/// UltraBay power and LED policy after the H8 configuration reset.
pub fn mainstage_power_policy(ec: Ec) {
    let present = ultrabay_present();
    #[cfg(target_arch = "x86_64")]
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
#[cfg(target_arch = "x86_64")]
pub fn disconnect(ec: Ec) {
    // Assert D_PLTRST# and DLPCPD before removing power and the LPC segment.
    reset_dlpc(&DLPC_OUTPUT);
    delay_ms(10);
    let _ = set_usb_power(ec, false);
    set_ultrabay_power(&DOCK_POWER, false);
    delay_ms(10);
    // SAFETY: fixed DLPC switch, disconnected only after power removal.
    unsafe { outb(DLPC_SWITCH, 0x00) };
}

#[cfg(not(target_arch = "x86_64"))]
pub fn disconnect(_ec: Ec) {}

/// Re-enable the dock-side COM1 after a warm stage transition.
#[cfg(target_arch = "x86_64")]
pub fn early_superio_config() {
    dock_write(0x29, 0x06);
    let _ = wait_ms(100_000, || dock_read(0x29) & 0x08 != 0);
    enable_dock_console();
    disable_dock_watchdog();
}

#[cfg(not(target_arch = "x86_64"))]
pub fn early_superio_config() {}

#[cfg(target_arch = "x86_64")]
fn enable_dock_console() {
    let mut serial = LogicalDevice::select(0x03, dock_read, dock_write);
    serial.set_io_base(IoResource::Primary(0x3f8));
    serial.set_irq(IrqResource::Primary(4));
    serial.set_enabled(true);
}

/// Keep the dock PC87392 watchdog LDN off (`device pnp 2e.a off`).
#[cfg(target_arch = "x86_64")]
fn disable_dock_watchdog() {
    const PC87392_WDT_LDN: u8 = 0x0a;
    LogicalDevice::select(PC87392_WDT_LDN, dock_read, dock_write).set_enabled(false);
}

/// Route the dock UART before the console probes 0x3f8. Failures are
/// non-fatal: an undocked laptop simply has no serial console.
pub fn setup_console(southbridge: &impl fstart_core::services::Southbridge, ec: Ec) {
    let _ = dlpc_init();
    if dock_present(southbridge) {
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
            #[cfg(target_arch = "x86_64")]
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

    #[cfg(target_arch = "x86_64")]
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
