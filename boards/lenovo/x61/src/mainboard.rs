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
use fstart_core::services::ServiceError;
#[cfg(feature = "stage")]
use fstart_platform_intel::gm965::Gm965Ich8;
#[cfg(feature = "stage")]
use fstart_platform_intel::{IntelEarlyBoardHooks, IntelEarlyCtx};

/// Board-specific X61 hooks for the GM965/ICH8 flow.
#[cfg(feature = "stage")]
pub struct X61Mainboard;

/// The mainboard contributes ACPI fragments through the same `AcpiDevice`
/// abstraction the chipset drivers use.
#[cfg(feature = "acpi")]
mod mainboard_acpi_device {
    extern crate alloc;

    use super::{X61Mainboard, x61_mainboard_dsdt_aml};

    impl fstart_acpi::device::AcpiDevice for X61Mainboard {
        type Config = fstart_platform_intel::gm965::Gm965Ich8AcpiContext;

        fn dsdt_aml(&self, config: &Self::Config) -> alloc::vec::Vec<u8> {
            x61_mainboard_dsdt_aml(*config)
        }
    }
}

#[cfg(feature = "stage")]
impl X61Mainboard {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

#[cfg(feature = "stage")]
impl IntelEarlyBoardHooks<Gm965Ich8> for X61Mainboard {
    fn before_console(&mut self, ctx: &mut IntelEarlyCtx<Gm965Ich8>) -> Result<(), ServiceError> {
        // Match coreboot's bootblock_mainboard_early_init(): DLPC init and
        // dock connection failures are non-fatal before the console exists.
        let _ = dock::dlpc_init();
        if dock::dock_present(ctx.southbridge()) {
            let _ = dock::dock_connect();
            dock::early_superio_config();
        }
        Ok(())
    }

    fn after_memory(&mut self, ctx: &mut IntelEarlyCtx<Gm965Ich8>) -> Result<(), ServiceError> {
        dock::post_raminit_setup(ctx.southbridge());
        // The ACPI-enabled mainstage owns EC/PMH7 runtime bring-up.
        #[cfg(feature = "acpi")]
        x61_ec_init();
        Ok(())
    }
}

/// X61 dock and DLPC helpers ported from coreboot `mainboard/lenovo/x61/dock.c`.
pub mod dock {
    #[cfg(target_arch = "x86_64")]
    use fstart_core::pio::{inb, outb};

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
        fstart_arch::x86::udelay(us);
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
    pub fn dock_present(southbridge: &impl fstart_core::services::Southbridge) -> bool {
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
    pub fn post_raminit_setup(southbridge: &impl fstart_core::services::Southbridge) {
        let _ = southbridge.gpio_set(42, false);
    }
}

const BIOS_RELEASE_DATE: &str = match option_env!("FSTART_SMBIOS_DATE") {
    Some(date) => date,
    None => "05/08/2026",
};

static X61_SMBIOS_PROCESSORS: [fstart_acpi::smbios::ProcessorDesc<'static>; 1] =
    [fstart_acpi::smbios::ProcessorDesc {
        socket: "Socket M",
        manufacturer: "Intel",
        family: 0x28,
        max_speed_mhz: 0,
        core_count: 0,
        thread_count: 0,
        caches: &[],
    }];

static X61_SMBIOS_MEMORY_DEVICES: [fstart_acpi::smbios::MemoryDeviceDesc<'static>; 2] = [
    fstart_acpi::smbios::MemoryDeviceDesc {
        locator: "DIMM0",
        size_mb: 0,
        speed_mhz: 0,
        memory_type: 0x02,
    },
    fstart_acpi::smbios::MemoryDeviceDesc {
        locator: "DIMM1",
        size_mb: 0,
        speed_mhz: 0,
        memory_type: 0x02,
    },
];

pub static X61_SMBIOS_DESC: fstart_acpi::smbios::SmbiosDesc<'static> =
    fstart_acpi::smbios::SmbiosDesc {
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
    use fstart_driver_lenovo::h8::{H8, H8_CONFIG0_EVENTS_ENABLE, H8Config};
    use fstart_driver_lenovo::pmh7::Pmh7;
    use fstart_platform_intel::gm965::Gm965Ich8AcpiContext;

    const X61_H8_EVENT_MASKS: [u8; 16] = [
        0x00, 0x00, 0xff, 0xff, 0xf4, 0x3c, 0x80, 0x01, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00,
        0x00,
    ];

    struct X61AcpiPaths {
        sb_scope: &'static str,
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

    /// Assemble the X61 DSDT: board glue (TRAP mechanism, sleep/wake hooks,
    /// dock, GPE routing) plus the complete H8 EC surface from the Lenovo
    /// driver.
    pub fn x61_mainboard_dsdt_aml(context: Gm965Ich8AcpiContext) -> Vec<u8> {
        let paths = X61AcpiPaths::new(context);
        let p = |s: &str| fstart_acpi::aml::Path::new(s);
        let mut out = Vec::new();

        // Root scope: ICH8 SMI trap + sleep/wake glue into the EC.
        out.extend(acpi_dsl! {
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
                    #{p(&paths.ec_mute)}(1u32);
                    #{p(&paths.ec_usbp)}(0u32);
                    #{p(&paths.ec_radi)}(0u32);
                    #{p(&paths.ec_hkey_mhkc)}(0u32);
                }
                Method("_WAK", 1, NotSerialized) {
                    #{p(&paths.ec_hkey_mhkc)}(1u32);
                    #{p(&paths.ec_hkey_wake)}(Arg0);
                    Return(Package(0u32, 0u32));
                }
            }
        });

        // Dock: DLPC presence + Toshiba dock registers under \_SB.
        out.extend(acpi_dsl! {
            Scope(#{&paths.sb_scope}) {
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
                    Name("_PCL", Package(#{p(&paths.sb_scope)}));
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
        });

        // GPE routing: EC wake events (level-triggered GPIO8 wake path).
        out.extend(acpi_dsl! {
            Scope(#{&paths.gpe_scope}) {
                Method("_L18", 0, NotSerialized) {
                    Local0 = #{p(&paths.ec_wake)};
                    If (Local0 & 0x04u32) {
                        Notify(#{p(&paths.ec_lid)}, 0x02u32);
                    }
                    If (Local0 & 0x08u32) {
                        Notify(#{p(&paths.dock)}, 0x03u32);
                        Notify(#{p(&paths.ec_slpb)}, 0x02u32);
                    }
                    If (Local0 & 0x10u32) {
                        Notify(#{p(&paths.ec_slpb)}, 0x02u32);
                    }
                    If (Local0 & 0x80u32) {
                        Notify(#{p(&paths.ec_slpb)}, 0x02u32);
                    }
                }
            }
        });

        // The complete H8 EC surface (EC device, batteries, thermal zones
        // with fan power resource, lid, AC, sleep button, HKEY hub, and the
        // PMH7/ECMM/ECGS/TWRI resource devices).
        let h8 = H8Config::x61();
        out.extend(fstart_driver_lenovo::h8_acpi::dsdt_aml(
            &h8,
            context.lpc_scope(),
        ));

        out
    }

    /// Runtime EC/PMH7 bring-up, mirroring coreboot's `h8_enable()` and
    /// `pmh7` device init: thermal management + event/hotkey reporting on,
    /// trackpoint and USB power on, WLAN/BT radios per board presence, and
    /// the PMH7 backlight + dock-event sources the board declares.
    pub fn x61_ec_init() {
        let pmh7 = Pmh7::new(0x15e0);
        pmh7.log_identity();
        pmh7.backlight_enable(true);
        pmh7.dock_event_enable(true);

        let h8 = H8;
        h8.clear_out_queue();
        // CONFIG0: events + hotkey enable (SMM H8 + thermal management are
        // set by init_config0 itself, matching coreboot).
        let config_ok = h8.init_config0(H8_CONFIG0_EVENTS_ENABLE);
        let events_ok = h8.program_event_masks(&X61_H8_EVENT_MASKS);
        let trackpoint_ok = h8.trackpoint_enable(true);
        let usb_ok = h8.usb_power_enable(true);
        let wlan_ok = h8.wlan_enable(true);
        let bluetooth_ok = h8.bluetooth_enable(true);
        if ![
            config_ok,
            events_ok,
            trackpoint_ok,
            usb_ok,
            wlan_ok,
            bluetooth_ok,
        ]
        .into_iter()
        .all(core::convert::identity)
        {
            fstart_log::error!("lenovo-x61: H8 EC initialization incomplete");
        }
    }
}

#[cfg(feature = "acpi")]
pub use acpi_impl::x61_mainboard_dsdt_aml;

#[cfg(all(feature = "acpi", feature = "stage"))]
pub use acpi_impl::x61_ec_init;
