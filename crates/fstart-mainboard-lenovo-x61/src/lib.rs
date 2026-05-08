//! Lenovo ThinkPad X61 mainboard glue.
//!
//! This crate intentionally contains the board-specific parts that do not
//! belong in the reusable GM965 northbridge or ICH8 southbridge drivers.  The
//! important pre-console path is the X6 UltraBase dock: ICH8 opens the LPC/GPIO
//! decode windows, then this driver initializes the laptop-side DLPC, connects
//! the dock-side LPC bus when present, and enables the dock PC87392 COM1 before
//! the NS16550 console driver probes port 0x3f8.

#![no_std]

use fstart_services::device::{Device, DeviceError};
use fstart_services::{Mainboard, ServiceError};
use serde::{Deserialize, Serialize};

/// Lenovo ThinkPad X61 mainboard configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LenovoX61MainboardConfig {
    /// ICH GPIO base used to sample the dock-present GPIO.
    #[serde(default = "default_gpio_base")]
    pub gpio_base: u16,
    /// Initialize dock LPC and the dock-side PC87392 COM1 before console init.
    #[serde(default = "default_true")]
    pub dock_early_console: bool,
    /// ACPI contributor name marker for codegen.
    #[serde(default)]
    pub acpi_name: Option<heapless::String<8>>,
}

impl Default for LenovoX61MainboardConfig {
    fn default() -> Self {
        Self {
            gpio_base: default_gpio_base(),
            dock_early_console: true,
            acpi_name: None,
        }
    }
}

fn default_gpio_base() -> u16 {
    0x0580
}

fn default_true() -> bool {
    true
}

/// Lenovo ThinkPad X61 mainboard hook driver.
pub struct LenovoX61Mainboard {
    config: LenovoX61MainboardConfig,
}

impl Device for LenovoX61Mainboard {
    const NAME: &'static str = "lenovo-x61-mainboard";
    const COMPATIBLE: &'static [&'static str] = &["lenovo,thinkpad-x61"];
    type Config = LenovoX61MainboardConfig;

    fn new(config: &Self::Config) -> Result<Self, DeviceError> {
        Ok(Self {
            config: config.clone(),
        })
    }

    fn init(&mut self) -> Result<(), DeviceError> {
        Ok(())
    }
}

impl Mainboard for LenovoX61Mainboard {
    fn pre_console_init(&mut self) -> Result<(), ServiceError> {
        // Match coreboot's bootblock_mainboard_early_init(): DLPC init and
        // dock connection failures are non-fatal before the console exists.
        // When the dock-present GPIO is asserted, coreboot still attempts the
        // PC87392 COM1 enable after dock_connect(); do the same so a marginal
        // delay/timeout does not suppress all serial output.
        let _ = dock::dlpc_init();
        if self.config.dock_early_console && dock::dock_present(self.config.gpio_base) {
            let _ = dock::dock_connect();
            dock::early_superio_config();
        }
        Ok(())
    }

    fn ramstage_init(&mut self) -> Result<(), ServiceError> {
        dock::post_raminit_setup(self.config.gpio_base);
        dock::ec_dock_state_update();
        dock::ultrabay_power_update();
        Ok(())
    }

    fn finalize(&mut self) -> Result<(), ServiceError> {
        Ok(())
    }
}

/// X61 dock and DLPC helpers ported from coreboot `mainboard/lenovo/x61/dock.c`.
pub mod dock {
    #[cfg(target_arch = "x86_64")]
    use fstart_pio::{inb, inw, outb};

    const DLPC_INDEX: u16 = 0x164e;
    const DLPC_DATA: u16 = 0x164f;
    const DLPC_SWITCH: u16 = 0x164c;
    const DLPC_GPIO: u16 = 0x1680;
    const DOCK_INDEX: u16 = 0x002e;
    const DOCK_DATA: u16 = 0x002f;
    const DOCK_GPIO_BASE: u16 = 0x1620;

    const PC87392_GPIO_PIN_DEBOUNCE: u8 = 1 << 0;
    const PC87392_GPIO_PIN_PULLUP: u8 = 1 << 1;
    const PC87392_GPIO_PIN_TRIGGERS_SMI: u8 = 1 << 2;
    const PC87392_GPIO_PIN_TYPE_PUSH_PULL: u8 = 1 << 3;
    const PC87392_GPIO_PIN_OE: u8 = 1 << 4;

    #[cfg(target_arch = "x86_64")]
    fn delay_us(us: u32) {
        for _ in 0..us.saturating_mul(100) {
            core::hint::spin_loop();
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn delay_ms(ms: u32) {
        for _ in 0..ms {
            delay_us(1000);
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn dlpc_write(reg: u8, value: u8) {
        // SAFETY: fixed laptop-side NSC PC87382 PnP config ports decoded by ICH8 LPC setup.
        unsafe {
            outb(DLPC_INDEX, reg);
            outb(DLPC_DATA, value);
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn dlpc_read(reg: u8) -> u8 {
        // SAFETY: fixed laptop-side NSC PC87382 PnP config ports decoded by ICH8 LPC setup.
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
        dlpc_write(0x07, 0x07);
        dlpc_write(0x60, 0x16);
        dlpc_write(0x61, 0x80);
        dlpc_write(0x30, 0x01);
        dlpc_gpio_set_mode(0x00, 3);
        dlpc_gpio_set_mode(0x01, 3);
        dlpc_gpio_set_mode(0x02, 0);
        dlpc_gpio_set_mode(0x03, 3);
        dlpc_gpio_set_mode(0x04, 4);
        dlpc_gpio_set_mode(0x20, 4);
        dlpc_gpio_set_mode(0x21, 4);
        dlpc_gpio_set_mode(0x23, 4);
    }

    /// Initialize the laptop-side DLPC switch and its GPIO block.
    #[cfg(target_arch = "x86_64")]
    pub fn dlpc_init() -> Result<(), ()> {
        let mut timeout = 1000;
        dlpc_write(0x29, 0xa0);
        while (dlpc_read(0x29) & 0x10) == 0 && timeout != 0 {
            timeout -= 1;
            delay_us(1000);
        }
        if timeout == 0 {
            return Err(());
        }

        dlpc_write(0x07, 0x19);
        dlpc_write(0x60, 0x16);
        dlpc_write(0x61, 0x4c);
        dlpc_write(0x30, 0x01);
        dlpc_gpio_init();
        Ok(())
    }

    /// Initialize the laptop-side DLPC switch and its GPIO block.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn dlpc_init() -> Result<(), ()> {
        Ok(())
    }

    /// Return whether an X6 UltraBase dock is attached.
    #[cfg(target_arch = "x86_64")]
    pub fn dock_present(gpiobase: u16) -> bool {
        // SAFETY: GPIOBASE is programmed by the ICH8 pre-console path.
        unsafe { ((inw(gpiobase + 0x0c) >> 13) & 1) == 0 }
    }

    /// Return whether an X6 UltraBase dock is attached.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn dock_present(_gpiobase: u16) -> bool {
        false
    }

    /// Connect the dock-side LPC bus and initialize dock GPIO/power.
    #[cfg(target_arch = "x86_64")]
    pub fn dock_connect() -> Result<(), ()> {
        let mut timeout = 1000;
        // SAFETY: DLPC switch I/O base was activated by dlpc_init().
        unsafe { outb(DLPC_SWITCH, 0x07) };
        while unsafe { inb(DLPC_SWITCH) } & 8 == 0 && timeout != 0 {
            timeout -= 1;
            delay_us(1000);
        }
        if timeout == 0 {
            // SAFETY: disable the DLPC switch on failure.
            unsafe { outb(DLPC_SWITCH, 0x00) };
            dlpc_write(0x30, 0x00);
            return Err(());
        }

        // SAFETY: dock GPIO base is activated by dlpc_init().
        unsafe { outb(DLPC_GPIO, 0xfe) };
        delay_ms(100);
        // SAFETY: dock GPIO base is activated by dlpc_init().
        unsafe { outb(DLPC_GPIO, 0xff) };
        delay_ms(100);

        dock_write(0x29, 0x06);
        timeout = 1000;
        while (dock_read(0x29) & 0x08) == 0 && timeout != 0 {
            timeout -= 1;
            delay_us(1000);
        }
        if timeout == 0 {
            return Err(());
        }

        dock_write(0x24, 0x37);
        dock_write(0x25, 0xa0);
        dock_write(0x26, 0x01);
        dock_write(0x28, 0x02);
        dock_write(0x07, 0x07);
        dock_write(0x60, 0x16);
        dock_write(0x61, 0x20);

        dock_gpio_set_mode(
            0x00,
            PC87392_GPIO_PIN_DEBOUNCE | PC87392_GPIO_PIN_PULLUP,
            0x00,
        );
        dock_gpio_set_mode(
            0x01,
            PC87392_GPIO_PIN_DEBOUNCE | PC87392_GPIO_PIN_PULLUP,
            PC87392_GPIO_PIN_TRIGGERS_SMI,
        );
        for port in [
            0x02, 0x03, 0x04, 0x05, 0x06, 0x11, 0x12, 0x13, 0x14, 0x15, 0x17, 0x22, 0x23, 0x24,
            0x25, 0x26, 0x27, 0x30, 0x31, 0x32, 0x33, 0x34, 0x36, 0x37,
        ] {
            dock_gpio_set_mode(port, PC87392_GPIO_PIN_PULLUP, 0x00);
        }
        dock_gpio_set_mode(0x07, PC87392_GPIO_PIN_PULLUP, 0x02);
        dock_gpio_set_mode(
            0x10,
            PC87392_GPIO_PIN_DEBOUNCE | PC87392_GPIO_PIN_PULLUP,
            PC87392_GPIO_PIN_TRIGGERS_SMI,
        );
        dock_gpio_set_mode(0x16, PC87392_GPIO_PIN_PULLUP | PC87392_GPIO_PIN_OE, 0x00);
        dock_gpio_set_mode(
            0x20,
            PC87392_GPIO_PIN_TYPE_PUSH_PULL | PC87392_GPIO_PIN_OE,
            0x00,
        );
        dock_gpio_set_mode(
            0x21,
            PC87392_GPIO_PIN_TYPE_PUSH_PULL | PC87392_GPIO_PIN_OE,
            0x00,
        );
        dock_gpio_set_mode(0x35, PC87392_GPIO_PIN_PULLUP | PC87392_GPIO_PIN_OE, 0x00);

        dock_write(0x30, 0x01);
        // SAFETY: dock GPIO block is configured at 0x1620.
        unsafe {
            outb(DOCK_GPIO_BASE + 0x08, 0x00);
            outb(DOCK_GPIO_BASE + 0x03, 0x00);
            outb(DOCK_GPIO_BASE + 0x02, 0x82);
            outb(DOCK_GPIO_BASE + 0x04, 0xff);
            outb(DOCK_GPIO_BASE + 0x08, 0x03);
        }
        dock_write(0x07, 0x03);
        dock_write(0x30, 0x01);
        Ok(())
    }

    /// Connect the dock-side LPC bus and initialize dock GPIO/power.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn dock_connect() -> Result<(), ()> {
        Ok(())
    }

    /// Disconnect the dock-side LPC bus and power rails.
    #[cfg(target_arch = "x86_64")]
    pub fn dock_disconnect() {
        // SAFETY: DLPC and dock GPIO ports are fixed board resources.
        unsafe { outb(DLPC_SWITCH, 0x00) };
        delay_ms(10);
        // SAFETY: dock GPIO base is active while connected.
        unsafe { outb(DLPC_GPIO, 0xfc) };
        delay_ms(10);
        // SAFETY: dock GPIO block is configured at 0x1620.
        unsafe { outb(DOCK_GPIO_BASE + 0x08, 0x00) };
        delay_us(10_000);
    }

    /// Disconnect the dock-side LPC bus and power rails.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn dock_disconnect() {}

    /// Enable the dock-side PC87392 COM1 at 0x3f8.
    #[cfg(target_arch = "x86_64")]
    pub fn early_superio_config() {
        let mut timeout = 100_000;
        dock_write(0x29, 0x06);
        while (dock_read(0x29) & 0x08) == 0 && timeout != 0 {
            timeout -= 1;
            delay_us(1000);
        }
        dock_write(0x07, 0x03);
        dock_write(0x60, 0x03);
        dock_write(0x61, 0xf8);
        dock_write(0x30, 0x01);
    }

    /// Enable the dock-side PC87392 COM1 at 0x3f8.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn early_superio_config() {}

    /// Switch the X61 SMBus mux back to the EEPROM side after SPD/raminit.
    #[cfg(target_arch = "x86_64")]
    pub fn post_raminit_setup(gpiobase: u16) {
        fstart_gpio_ich::IchGpio::new(gpiobase).set(42, false);
    }

    /// Switch the X61 SMBus mux back to the EEPROM side after SPD/raminit.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn post_raminit_setup(_gpiobase: u16) {}

    const EC_DATA: u16 = 0x62;
    const EC_SC: u16 = 0x66;
    const EC_OBF: u8 = 1 << 0;
    const EC_IBF: u8 = 1 << 1;
    const EC_CMD_READ: u8 = 0x80;
    const EC_CMD_WRITE: u8 = 0x81;

    #[cfg(target_arch = "x86_64")]
    fn ec_wait_input_clear() -> bool {
        for _ in 0..100_000 {
            // SAFETY: fixed ACPI EC status port decoded by ICH8 LPC setup.
            if unsafe { inb(EC_SC) } & EC_IBF == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    #[cfg(target_arch = "x86_64")]
    fn ec_wait_output_full() -> bool {
        for _ in 0..100_000 {
            // SAFETY: fixed ACPI EC status port decoded by ICH8 LPC setup.
            if unsafe { inb(EC_SC) } & EC_OBF != 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    #[cfg(target_arch = "x86_64")]
    fn ec_read(index: u8) -> Option<u8> {
        if !ec_wait_input_clear() {
            return None;
        }
        // SAFETY: fixed ACPI EC command/data ports.
        unsafe { outb(EC_SC, EC_CMD_READ) };
        if !ec_wait_input_clear() {
            return None;
        }
        unsafe { outb(EC_DATA, index) };
        if !ec_wait_output_full() {
            return None;
        }
        Some(unsafe { inb(EC_DATA) })
    }

    #[cfg(target_arch = "x86_64")]
    fn ec_write(index: u8, value: u8) -> bool {
        if !ec_wait_input_clear() {
            return false;
        }
        // SAFETY: fixed ACPI EC command/data ports.
        unsafe { outb(EC_SC, EC_CMD_WRITE) };
        if !ec_wait_input_clear() {
            return false;
        }
        unsafe { outb(EC_DATA, index) };
        if !ec_wait_input_clear() {
            return false;
        }
        unsafe { outb(EC_DATA, value) };
        true
    }

    #[cfg(target_arch = "x86_64")]
    fn ec_update_bits(index: u8, clear: u8, set: u8) {
        if let Some(value) = ec_read(index) {
            let _ = ec_write(index, (value & !clear) | set);
        }
    }

    /// Mirror coreboot X61 ramstage EC dock-bit update.
    #[cfg(target_arch = "x86_64")]
    pub fn ec_dock_state_update() {
        ec_update_bits(0x03, 1 << 2, 0);
        // SAFETY: DLPC switch base is fixed and active when dock connected.
        if unsafe { inb(DLPC_SWITCH) } & 0x08 != 0 {
            ec_update_bits(0x03, 0, 1 << 2);
            let _ = ec_write(0x0c, 0x88);
        }
    }

    /// Mirror coreboot X61 ramstage EC dock-bit update.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn ec_dock_state_update() {}

    /// Update UltraBay power and EC LED/state. This follows the coreboot X61
    /// mainboard path; IDE-primary enabling itself still needs a PCI IDE
    /// device model in fstart.
    #[cfg(target_arch = "x86_64")]
    pub fn ultrabay_power_update() {
        // SAFETY: dock GPIO block is configured at 0x1620 when docked.
        let present = unsafe { inb(DOCK_GPIO_BASE + 0x01) } & 0x02 == 0;
        let power = unsafe { inb(DOCK_GPIO_BASE + 0x08) };
        if present {
            unsafe { outb(DOCK_GPIO_BASE + 0x08, power | 0x01) };
            let _ = ec_write(0x0c, 0x84);
        } else {
            unsafe { outb(DOCK_GPIO_BASE + 0x08, power & !0x01) };
            let _ = ec_write(0x0c, 0x04);
        }
    }

    /// Update UltraBay power and EC LED/state.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn ultrabay_power_update() {}
}

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi::device::AcpiDevice;
    use fstart_acpi_macros::acpi_dsl;

    use super::*;

    impl AcpiDevice for LenovoX61Mainboard {
        type Config = LenovoX61MainboardConfig;

        fn dsdt_aml(&self, _config: &Self::Config) -> Vec<u8> {
            let p = |s: &str| fstart_acpi::aml::Path::new(s);
            acpi_dsl! {
                Scope("\\") {
                    Name("SMIF", 0u32);
                    OperationRegion("IOT_", SystemIO, 0x0800u32, 0x10u32);
                    Field("IOT_", ByteAcc, NoLock, Preserve) {
                        Offset(0x08),
                        TRP0, 8,
                    }
                    Method("TRAP", 1, Serialized) {
                        SMIF = Arg0;
                        TRP0 = 0u32;
                        Return(SMIF);
                    }

                    Method("_PTS", 1, NotSerialized) {
                        #{p("\\_SB_.PCI0.LPCB.EC__.MUTE")}(1u32);
                        #{p("\\_SB_.PCI0.LPCB.EC__.USBP")}(0u32);
                        #{p("\\_SB_.PCI0.LPCB.EC__.RADI")}(0u32);
                        #{p("\\_SB_.PCI0.LPCB.EC__.HKEY.MHKC")}(0u32);
                    }
                    Method("_WAK", 1, NotSerialized) {
                        #{p("\\_SB_.PCI0.LPCB.EC__.HKEY.MHKC")}(1u32);
                        #{p("\\_SB_.PCI0.LPCB.EC__.HKEY.WAKE")}(Arg0);
                        Return(Package(0u32, 0u32));
                    }
                }

                Scope("\\_SB_.PCI0.LPCB") {
                        Device("EC__") {
                            Name("_HID", EisaId("PNP0C09"));
                            Name("_UID", 0u32);
                            Name("_GPE", 0x18u32);
                            Name("_CRS", ResourceTemplate {
                                IO(0x0062u16, 0x0062u16, 0x01u8, 0x01u8);
                                IO(0x0066u16, 0x0066u16, 0x01u8, 0x01u8);
                            });
                            OperationRegion("ECOR", EmbeddedControl, 0x00u32, 0x100u32);
                            Field("ECOR", ByteAcc, Lock, Preserve) {
                                Offset(0x02),
                                DKR1, 1,
                                Offset(0x0F),
                                , 7,
                                TBSW, 1,
                                Offset(0x2F),
                                , 6,
                                FAND, 1,
                                FANA, 1,
                                Offset(0x30),
                                , 6,
                                ALMT, 1,
                                Offset(0x38),
                                B0ST, 4,
                                , 1,
                                B0CH, 1,
                                B0DI, 1,
                                B0PR, 1,
                                B1ST, 4,
                                , 1,
                                B1CH, 1,
                                B1DI, 1,
                                B1PR, 1,
                                Offset(0x3A),
                                AMUT, 1,
                                , 3,
                                BTEB, 1,
                                WLEB, 1,
                                WWEB, 1,
                                Offset(0x3B),
                                , 1,
                                KBLT, 1,
                                , 2,
                                USPW, 1,
                                Offset(0x46),
                                , 4,
                                HPAC, 1,
                                Offset(0x48),
                                HPPI, 1,
                                GSTS, 1,
                                Offset(0x4E),
                                WAKE, 16,
                                Offset(0x78),
                                TMP0, 8,
                                TMP1, 8,
                                Offset(0x81),
                                PAGE, 8,
                                Offset(0xA0),
                                BARC, 16,
                                BAFC, 16,
                                Offset(0xA8),
                                BAPR, 16,
                                BAVO, 16,
                            }
                            Method("MUTE", 1, NotSerialized) { AMUT = Arg0; }
                            Method("RADI", 1, NotSerialized) { WLEB = Arg0; WWEB = Arg0; BTEB = Arg0; }
                            Method("USBP", 1, NotSerialized) { USPW = Arg0; }
                            Method("LGHT", 1, NotSerialized) { KBLT = Arg0; }
                            Method("FANE", 1, NotSerialized) {
                                If (Arg0) {
                                    FAND = 1u32;
                                    FANA = 0u32;
                                } Else {
                                    FAND = 0u32;
                                    FANA = 1u32;
                                }
                            }

                            Device("AC__") {
                                Name("_HID", "ACPI0003");
                                Name("_UID", 0u32);
                                Name("_PCL", Package(#{p("\\_SB_")}));
                                Method("_PSR", 0, NotSerialized) { Return(HPAC); }
                                Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }
                            }
                            Device("LID_") { Name("_HID", EisaId("PNP0C0D")); Method("_LID", 0, NotSerialized) { Return(1u32); } }
                            Device("SLPB") { Name("_HID", EisaId("PNP0C0E")); }
                            Device("HKEY") {
                                Name("_HID", EisaId("IBM0068"));
                                Name("BTN_", 0u32);
                                Name("BTAB", 0u32);
                                Name("DHKN", 0x080Cu32);
                                Name("EMSK", 0u32);
                                Name("ETAB", 0u32);
                                Name("EN__", 0u32);
                                Method("_STA", 0, NotSerialized) { Return(0x0Fu32); }
                                Method("MHKP", 0, NotSerialized) {
                                    Local0 = BTN_;
                                    If (Local0 != 0u32) {
                                        BTN_ = 0u32;
                                        Local0 = Local0 + 0x1000u32;
                                        Return(Local0);
                                    }
                                    Local0 = BTAB;
                                    If (Local0 != 0u32) {
                                        BTAB = 0u32;
                                        Local0 = Local0 + 0x5000u32;
                                        Return(Local0);
                                    }
                                    Return(0u32);
                                }
                                Method("RHK_", 1, NotSerialized) {
                                    BTN_ = Arg0;
                                    Notify(HKEY, 0x80u32);
                                }
                                Method("RTAB", 1, NotSerialized) {
                                    BTAB = Arg0;
                                    Notify(HKEY, 0x80u32);
                                }
                                Method("MHKC", 1, NotSerialized) {
                                    If (Arg0) {
                                        EMSK = DHKN;
                                        ETAB = 0xFFFFFFFFu32;
                                    } Else {
                                        EMSK = 0u32;
                                        ETAB = 0u32;
                                    }
                                    EN__ = Arg0;
                                }
                                Method("MHKV", 0, NotSerialized) { Return(0x0100u32); }
                                Method("WLSW", 0, NotSerialized) { Return(GSTS); }
                                Method("MHKG", 0, NotSerialized) { Return(TBSW << 3u32); }
                                Method("WAKE", 1, NotSerialized) { Return(0u32); }
                            }
                            Device("BAT0") {
                                Name("_HID", EisaId("PNP0C0A"));
                                Name("_UID", 0u32);
                                Name("_PCL", Package(#{p("\\_SB_")}));
                                Method("_BIF", 0, NotSerialized) { Return(Package(0u32, 0xFFFFFFFFu32, 0xFFFFFFFFu32, 1u32, 10800u32, 0u32, 200u32, 1u32, 1u32, "", "", "", "")); }
                                Method("_BST", 0, NotSerialized) {
                                    If (B0PR) {
                                        If (B0CH) { Return(Package(2u32, 0u32, #{p("BARC")}, #{p("BAVO")})); }
                                        If (B0DI) { Return(Package(1u32, 0u32, #{p("BARC")}, #{p("BAVO")})); }
                                    }
                                    Return(Package(0u32, 0u32, 0u32, 0u32));
                                }
                                Method("_STA", 0, NotSerialized) { If (B0PR) { Return(0x1Fu32); } Else { Return(0x0Fu32); } }
                            }
                            Device("BAT1") {
                                Name("_HID", EisaId("PNP0C0A"));
                                Name("_UID", 1u32);
                                Name("_PCL", Package(#{p("\\_SB_")}));
                                Method("_BIF", 0, NotSerialized) { Return(Package(0u32, 0xFFFFFFFFu32, 0xFFFFFFFFu32, 1u32, 10800u32, 0u32, 200u32, 1u32, 1u32, "", "", "", "")); }
                                Method("_BST", 0, NotSerialized) {
                                    If (B1PR) {
                                        If (B1CH) { Return(Package(2u32, 0u32, #{p("BARC")}, #{p("BAVO")})); }
                                        If (B1DI) { Return(Package(1u32, 0u32, #{p("BARC")}, #{p("BAVO")})); }
                                    }
                                    Return(Package(0u32, 0u32, 0u32, 0u32));
                                }
                                Method("_STA", 0, NotSerialized) { If (B1PR) { Return(0x1Fu32); } Else { Return(0x0Fu32); } }
                            }
                            Method("_Q13", 0, NotSerialized) { Notify(SLPB, 0x80u32); }
                            Method("_Q26", 0, NotSerialized) { Notify(AC__, 0x80u32); }
                            Method("_Q27", 0, NotSerialized) { Notify(AC__, 0x80u32); }
                            Method("_Q2A", 0, NotSerialized) { Notify(LID_, 0x80u32); }
                            Method("_Q2B", 0, NotSerialized) { Notify(LID_, 0x80u32); }
                            Method("_Q24", 0, NotSerialized) { Notify(BAT0, 0x80u32); }
                            Method("_Q25", 0, NotSerialized) { Notify(BAT1, 0x80u32); }
                            Method("_Q4A", 0, NotSerialized) { Notify(BAT0, 0x81u32); }
                            Method("_Q4B", 0, NotSerialized) { Notify(BAT0, 0x80u32); }
                            Method("_Q4C", 0, NotSerialized) { Notify(BAT1, 0x81u32); }
                            Method("_Q4D", 0, NotSerialized) { Notify(BAT1, 0x80u32); }
                            Method("_Q50", 0, NotSerialized) { Notify(#{p("\\_SB_.DOCK")}, 3u32); }
                            Method("_Q58", 0, NotSerialized) { Notify(#{p("\\_SB_.DOCK")}, 0u32); }
                        }
                }

                Scope("\\_SB_") {
                    OperationRegion("DLPC", SystemIO, 0x164Cu32, 0x01u32);
                    Field("DLPC", ByteAcc, NoLock, Preserve) {
                        , 3,
                        DSTA, 1,
                    }
                    Device("DOCK") {
                        Name("_HID", "ACPI0003");
                        Name("_UID", 0u32);
                        Name("_PCL", Package(#{p("\\_SB_")}));
                        Method("_DCK", 1, NotSerialized) {
                            If (Arg0) {
                                TRAP(1u32);
                            } Else {
                                TRAP(2u32);
                            }
                            Local0 = Arg0 ^ DSTA;
                            Return(Local0);
                        }
                        Method("_STA", 0, NotSerialized) {
                            Return(DSTA);
                        }
                    }
                }

                Scope("\\_GPE") {
                    Method("_L18", 0, NotSerialized) {
                        Local0 = #{p("\\_SB_.PCI0.LPCB.EC__.WAKE")};
                        If (Local0 & 0x04u32) {
                            Notify(#{p("\\_SB_.PCI0.LPCB.EC__.LID_")}, 0x02u32);
                        }
                        If (Local0 & 0x08u32) {
                            Notify(#{p("\\_SB_.DOCK")}, 0x03u32);
                            Notify(#{p("\\_SB_.PCI0.LPCB.EC__.SLPB")}, 0x02u32);
                        }
                        If (Local0 & 0x10u32) {
                            Notify(#{p("\\_SB_.PCI0.LPCB.EC__.SLPB")}, 0x02u32);
                        }
                        If (Local0 & 0x80u32) {
                            Notify(#{p("\\_SB_.PCI0.LPCB.EC__.SLPB")}, 0x02u32);
                        }
                    }
                }
            }
        }

        fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
            Vec::new()
        }
    }
}
