//! Lenovo ThinkPad X61 mainboard glue.
//!
//! This module intentionally contains the board-specific parts that do not
//! belong in the reusable GM965 northbridge or ICH8 southbridge drivers.  The
//! important pre-console path is the X6 UltraBase dock: ICH8 opens the LPC/GPIO
//! decode windows, then this driver initializes the laptop-side DLPC, connects
//! the dock-side LPC bus when present, and enables the dock PC87392 COM1 before
//! the NS16550 console driver probes port 0x3f8.

#![allow(clippy::result_unit_err)]

#[cfg(feature = "stage")]
use fstart_driver_intel_ich8::IntelIch8;
#[cfg(feature = "stage")]
use fstart_platform_intel_gm965_ich8::Gm965Ich8Mainboard;
#[cfg(feature = "stage")]
use fstart_services::ServiceError;

/// Board-specific X61 hooks for the GM965/ICH8 recipe.
#[cfg(feature = "stage")]
pub struct X61Mainboard;

#[cfg(feature = "stage")]
impl X61Mainboard {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(feature = "stage")]
impl Gm965Ich8Mainboard for X61Mainboard {
    fn pre_console(&mut self, ich8: &mut IntelIch8) -> Result<(), ServiceError> {
        // Match coreboot's bootblock_mainboard_early_init(): DLPC init and
        // dock connection failures are non-fatal before the console exists.
        let _ = dock::dlpc_init();
        if dock::dock_present(ich8) {
            let _ = dock::dock_connect();
            dock::early_superio_config();
        }
        Ok(())
    }

    fn post_dram(&mut self, ich8: &mut IntelIch8) -> Result<(), ServiceError> {
        dock::post_raminit_setup(ich8);
        Ok(())
    }

    fn finalize(&mut self, _ich8: &mut IntelIch8) -> Result<(), ServiceError> {
        fstart_superio::quiesce_i8042_for_os();
        Ok(())
    }
}

/// X61 dock and DLPC helpers ported from coreboot `mainboard/lenovo/x61/dock.c`.
pub mod dock {
    #[cfg(target_arch = "x86_64")]
    use fstart_pio::{inb, outb};

    const DLPC_INDEX: u16 = 0x164e;
    const DLPC_DATA: u16 = 0x164f;
    const DLPC_SWITCH: u16 = 0x164c;
    const DLPC_GPIO: u16 = 0x1680;
    const DOCK_INDEX: u16 = 0x002e;
    const DOCK_DATA: u16 = 0x002f;
    const DOCK_GPIO_BASE: u16 = 0x1620;

    const PC87392_GPIO_PIN_OE: u8 = 0x01;
    const PC87392_GPIO_PIN_TYPE_PUSH_PULL: u8 = 0x02;
    const PC87392_GPIO_PIN_PULLUP: u8 = 0x04;
    const PC87392_GPIO_PIN_DEBOUNCE: u8 = 0x40;
    const PC87392_GPIO_PIN_TRIGGERS_SMI: u8 = 0x02;

    #[cfg(target_arch = "x86_64")]
    fn delay_us(us: u32) {
        fstart_arch_x86::udelay(us);
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
    pub fn dock_present(southbridge: &dyn fstart_services::Southbridge) -> bool {
        // Coreboot samples ICH GPIO13 low for dock present.  Ask the reusable
        // southbridge driver instead of duplicating its GPIOBASE in board config.
        southbridge.gpio_get(13).is_ok_and(|high| !high)
    }

    /// Connect the dock-side LPC bus and initialize dock GPIO/power.
    #[cfg(target_arch = "x86_64")]
    pub fn dock_connect() -> Result<(), ()> {
        let mut timeout = 1000;
        // Start from the vendor/coreboot state: dock reset asserted and DLPC
        // powered down, preserving unrelated GPIO bits in the DLPC GPIO data
        // register.  The dock UART is behind this LPC switch, so marginal
        // reset/power sequencing shows up later as serial corruption.
        unsafe { outb(DLPC_GPIO, inb(DLPC_GPIO) & 0xfc) };

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

        // Power up DLPC while keeping D_PLTRST# asserted, then deassert
        // D_PLTRST#.  Match coreboot's read-modify-write sequence exactly.
        unsafe { outb(DLPC_GPIO, (inb(DLPC_GPIO) & 0xfe) | 0x02) };
        delay_ms(100);
        unsafe { outb(DLPC_GPIO, inb(DLPC_GPIO) | 0x03) };
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
        disable_dock_watchdog();
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
        dock_write(0x70, 4);
        dock_write(0x30, 0x01);
        disable_dock_watchdog();
    }

    /// Enable the dock-side PC87392 COM1 at 0x3f8.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn early_superio_config() {}

    /// Keep the dock-side PC87392 watchdog LDN disabled, matching coreboot's
    /// `device pnp 2e.a off end # WDT` for the X61 dock SuperIO.
    #[cfg(target_arch = "x86_64")]
    fn disable_dock_watchdog() {
        const PC87392_WDT_LDN: u8 = 0x0a;
        dock_write(0x07, PC87392_WDT_LDN);
        dock_write(0x30, 0x00);
    }

    /// Switch the X61 SMBus mux back to the EEPROM side after SPD/raminit.
    pub fn post_raminit_setup(southbridge: &dyn fstart_services::Southbridge) {
        let _ = southbridge.gpio_set(42, false);
    }
}

const BIOS_RELEASE_DATE: &str = match option_env!("FSTART_SMBIOS_DATE") {
    Some(date) => date,
    None => "05/08/2026",
};

static X61_SMBIOS_PROCESSORS: [fstart_smbios::ProcessorDesc<'static>; 1] =
    [fstart_smbios::ProcessorDesc {
        socket: "Socket M",
        manufacturer: "Intel",
        family: 0x28,
        max_speed_mhz: 0,
        core_count: 0,
        thread_count: 0,
        caches: &[],
    }];

static X61_SMBIOS_MEMORY_DEVICES: [fstart_smbios::MemoryDeviceDesc<'static>; 2] = [
    fstart_smbios::MemoryDeviceDesc {
        locator: "DIMM0",
        size_mb: 0,
        speed_mhz: 0,
        memory_type: 0x02,
    },
    fstart_smbios::MemoryDeviceDesc {
        locator: "DIMM1",
        size_mb: 0,
        speed_mhz: 0,
        memory_type: 0x02,
    },
];

pub static X61_SMBIOS_DESC: fstart_smbios::SmbiosDesc<'static> = fstart_smbios::SmbiosDesc {
    bios_vendor: "fstart",
    bios_version: "0.1.0",
    bios_release_date: BIOS_RELEASE_DATE,
    sys_manufacturer: "LENOVO",
    sys_product: "ThinkPad X61",
    sys_version: "1.0",
    sys_serial: None,
    bb_manufacturer: "LENOVO",
    bb_product: "ThinkPad X61",
    chassis_type: 0x01,
    chassis_manufacturer: "LENOVO",
    processors: &X61_SMBIOS_PROCESSORS,
    memory_devices: &X61_SMBIOS_MEMORY_DEVICES,
    ram_base: 0x0010_0000,
    ram_end: 0x3fff_ffff,
};

#[cfg(feature = "acpi")]
mod acpi_impl {
    extern crate alloc;

    use alloc::string::String;
    use alloc::vec::Vec;
    use fstart_acpi_macros::acpi_dsl;
    use fstart_platform_intel_gm965_ich8::Gm965Ich8AcpiContext;

    struct X61AcpiPaths {
        sb_scope: &'static str,
        lpc_scope: &'static str,
        gpe_scope: &'static str,
        dock: String,
        ec_mute: String,
        ec_usbp: String,
        ec_radi: String,
        ec_hkey_mhkc: String,
        ec_hkey_wake: String,
        ec_wake: String,
        ec_lid: String,
        ec_slpb: String,
    }

    impl X61AcpiPaths {
        fn new(context: Gm965Ich8AcpiContext) -> Self {
            let ec = child_path(context.lpc_scope(), "EC__");
            let hkey = child_path(&ec, "HKEY");

            Self {
                sb_scope: context.sb_scope(),
                lpc_scope: context.lpc_scope(),
                gpe_scope: context.gpe_scope(),
                dock: child_path(context.sb_scope(), "DOCK"),
                ec_mute: child_path(&ec, "MUTE"),
                ec_usbp: child_path(&ec, "USBP"),
                ec_radi: child_path(&ec, "RADI"),
                ec_hkey_mhkc: child_path(&hkey, "MHKC"),
                ec_hkey_wake: child_path(&hkey, "WAKE"),
                ec_wake: child_path(&ec, "WAKE"),
                ec_lid: child_path(&ec, "LID_"),
                ec_slpb: child_path(&ec, "SLPB"),
            }
        }

        fn sb_scope(&self) -> &str {
            self.sb_scope
        }

        fn lpc_scope(&self) -> &str {
            self.lpc_scope
        }

        fn gpe_scope(&self) -> &str {
            self.gpe_scope
        }

        fn dock(&self) -> &str {
            &self.dock
        }

        fn ec_mute(&self) -> &str {
            &self.ec_mute
        }

        fn ec_usbp(&self) -> &str {
            &self.ec_usbp
        }

        fn ec_radi(&self) -> &str {
            &self.ec_radi
        }

        fn ec_hkey_mhkc(&self) -> &str {
            &self.ec_hkey_mhkc
        }

        fn ec_hkey_wake(&self) -> &str {
            &self.ec_hkey_wake
        }

        fn ec_wake(&self) -> &str {
            &self.ec_wake
        }

        fn ec_lid(&self) -> &str {
            &self.ec_lid
        }

        fn ec_slpb(&self) -> &str {
            &self.ec_slpb
        }
    }

    fn child_path(scope: &str, name: &str) -> String {
        let mut path = String::new();
        path.push_str(scope);
        if scope != "\\" && !scope.ends_with('.') {
            path.push('.');
        }
        path.push_str(name);
        path
    }

    pub fn x61_mainboard_dsdt_aml(context: Gm965Ich8AcpiContext) -> Vec<u8> {
        let paths = X61AcpiPaths::new(context);
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
                    #{p(paths.ec_mute())}(1u32);
                    #{p(paths.ec_usbp())}(0u32);
                    #{p(paths.ec_radi())}(0u32);
                    #{p(paths.ec_hkey_mhkc())}(0u32);
                }
                Method("_WAK", 1, NotSerialized) {
                    #{p(paths.ec_hkey_mhkc())}(1u32);
                    #{p(paths.ec_hkey_wake())}(Arg0);
                    Return(Package(0u32, 0u32));
                }
            }

            Scope(#{paths.lpc_scope()}) {
                    Device("EC__") {
                        Name("_HID", EisaId("PNP0C09"));
                        Name("_UID", 0u32);
                        // Coreboot X61 uses THINKPAD_EC_GPE = 0x12 for
                        // the EC query GPE.  The board-level _L18 method
                        // below handles the level-triggered GPIO8 wake
                        // event; using 0x18 here makes ACPICA install an
                        // EC edge handler on the same GPE and produces a
                        // level/edge type mismatch.
                        Name("_GPE", 0x12u32);
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
                            Name("_PCL", Package(#{p(paths.sb_scope())}));
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
                            Name("_PCL", Package(#{p(paths.sb_scope())}));
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
                            Name("_PCL", Package(#{p(paths.sb_scope())}));
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
                        Method("_Q50", 0, NotSerialized) { Notify(#{p(paths.dock())}, 3u32); }
                        Method("_Q58", 0, NotSerialized) { Notify(#{p(paths.dock())}, 0u32); }
                    }
            }

            Scope(#{paths.sb_scope()}) {
                OperationRegion("DLPC", SystemIO, 0x164Cu32, 0x01u32);
                Field("DLPC", ByteAcc, NoLock, Preserve) {
                    , 3,
                    DSTA, 1,
                }
                OperationRegion("TCOX", SystemIO, 0x0560u32, 0x20u32);
                Field("TCOX", ByteAcc, NoLock, Preserve) {
                    Offset(0x02),
                    TDIN, 8,
                    TDOT, 8,
                }
                Device("DOCK") {
                    Name("_HID", "ACPI0003");
                    Name("_UID", 0u32);
                    Name("_PCL", Package(#{p(paths.sb_scope())}));
                    Method("_DCK", 1, Serialized) {
                        If (Arg0) {
                            TDIN = 1u32;
                        } Else {
                            TDIN = 2u32;
                        }
                        Return(TDOT);
                    }
                    Method("_PSR", 0, NotSerialized) {
                        Return(DSTA);
                    }
                    Method("_STA", 0, NotSerialized) {
                        Return(DSTA);
                    }
                }
            }

            Scope(#{paths.gpe_scope()}) {
                Method("_L18", 0, NotSerialized) {
                    Local0 = #{p(paths.ec_wake())};
                    If (Local0 & 0x04u32) {
                        Notify(#{p(paths.ec_lid())}, 0x02u32);
                    }
                    If (Local0 & 0x08u32) {
                        Notify(#{p(paths.dock())}, 0x03u32);
                        Notify(#{p(paths.ec_slpb())}, 0x02u32);
                    }
                    If (Local0 & 0x10u32) {
                        Notify(#{p(paths.ec_slpb())}, 0x02u32);
                    }
                    If (Local0 & 0x80u32) {
                        Notify(#{p(paths.ec_slpb())}, 0x02u32);
                    }
                }
            }
        }
    }
}

#[cfg(feature = "acpi")]
pub use acpi_impl::x61_mainboard_dsdt_aml;
