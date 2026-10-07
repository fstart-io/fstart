//! Lenovo ThinkPad X61 mainboard glue.
//!
//! This module intentionally contains the board-specific parts that do not
//! belong in the reusable GM965 northbridge or ICH8 southbridge drivers.  The
//! important pre-console path is the X6 UltraBase dock: ICH8 opens the LPC/GPIO
//! decode windows, then this driver initializes the laptop-side DLPC, connects
//! the dock-side LPC bus when present, and enables the dock PC87392 COM1 before
//! the NS16550 console driver probes port 0x3f8.

#![allow(clippy::result_unit_err)]

#[cfg(all(not(test), any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
use crate::config::Hardware as Gm965Ich8;
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
use fstart_core::services::ServiceError;
#[cfg(all(not(test), fstart_stage_env = "car"))]
use fstart_platform_intel::{IntelEarlyBoardHooks, IntelEarlyCtx};
#[cfg(all(not(test), fstart_stage_env = "ram"))]
use fstart_platform_intel::{IntelMainstageBoardCtx, IntelMainstageBoardHooks};

/// Board-specific X61 hooks for the GM965/ICH8 flow.
#[cfg(any(fstart_stage_env = "car", fstart_stage_env = "ram"))]
#[derive(Default)]
pub struct X61Mainboard;

/// The mainboard contributes ACPI fragments through the same `AcpiDevice`
/// abstraction the chipset drivers use.
#[cfg(fstart_stage_env = "ram")]
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

#[cfg(all(not(test), fstart_stage_env = "car"))]
impl IntelEarlyBoardHooks<Gm965Ich8> for X61Mainboard {
    fn before_console(&mut self, ctx: &mut IntelEarlyCtx<Gm965Ich8>) -> Result<(), ServiceError> {
        setup_dock_console(ctx.southbridge());
        Ok(())
    }

    fn after_memory(&mut self, ctx: &mut IntelEarlyCtx<Gm965Ich8>) -> Result<(), ServiceError> {
        dock::post_raminit_setup(ctx.southbridge());
        Ok(())
    }
}

#[cfg(all(not(test), fstart_stage_env = "ram"))]
impl IntelMainstageBoardHooks<Gm965Ich8> for X61Mainboard {
    fn before_console(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<Gm965Ich8>,
    ) -> Result<(), ServiceError> {
        setup_dock_console(ctx.southbridge());
        Ok(())
    }

    fn before_devices(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<Gm965Ich8>,
    ) -> Result<(), ServiceError> {
        // Sample the detachable UltraBay before ICH8 programs IDE timings.
        // An absent or disconnected dock cannot supply a primary-channel disk.
        let primary = dock::dock_present(ctx.southbridge()) && dock::ultrabay_present();
        ctx.southbridge().set_ide_primary_enabled(primary);
        Ok(())
    }

    fn after_devices(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<Gm965Ich8>,
    ) -> Result<(), ServiceError> {
        dock::post_raminit_setup(ctx.southbridge());
        // EC/PMH7 hardware setup is independent of ACPI table emission.
        let resume = ctx.resume;
        x61_ec_init(ctx.southbridge(), resume);
        dock::mainstage_power_policy();
        if !resume && init_ck505(ctx.southbridge()).is_err() {
            fstart_log::error!("lenovo-x61: CK505 programming failed");
        }
        ricoh_sd_write_protect();
        Ok(())
    }

    fn before_handoff(
        &mut self,
        ctx: &mut IntelMainstageBoardCtx<Gm965Ich8>,
    ) -> Result<(), ServiceError> {
        // The shared installer sets SCI_EN directly, without an APMC. Once
        // permanent SMM is installed, initialize board EC routing via its
        // existing ACPI command handler (keep ACPI ownership on resume).
        use fstart_driver_intel::southbridge::smi::{
            APM_CNT, APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE,
        };
        let command = if ctx.resume {
            APM_CNT_ACPI_ENABLE
        } else {
            APM_CNT_ACPI_DISABLE
        };
        // SAFETY: permanent SMM is installed and owns this command port.
        unsafe { fstart_core::pio::outb(APM_CNT, command) };
        Ok(())
    }
}

#[cfg(all(not(test), any(fstart_stage_env = "car", fstart_stage_env = "ram")))]
fn setup_dock_console(southbridge: &mut fstart_driver_intel::ich8::IntelIch8) {
    // X61-specific dock routing. Failures are non-fatal before console.
    let _ = dock::dlpc_init();
    if dock::dock_present(southbridge) {
        let _ = dock::dock_connect();
        dock::early_superio_config();
    }
}

/// X61 dock and DLPC helpers ported from coreboot `mainboard/lenovo/x61/dock.c`.
pub mod dock {
    #[cfg(target_arch = "x86_64")]
    use fstart_core::pio::{inb, outb};
    #[cfg(target_arch = "x86_64")]
    use fstart_driver_superio::{IoResource, IrqResource, LogicalDevice};

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
        let mut gpio = LogicalDevice::select(0x07, dlpc_read, dlpc_write);
        gpio.set_io_base(IoResource::Primary(DLPC_GPIO));
        gpio.set_enabled(true);
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

        let mut switch = LogicalDevice::select(0x19, dlpc_read, dlpc_write);
        switch.set_io_base(IoResource::Primary(DLPC_SWITCH));
        switch.set_enabled(true);
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

    /// Whether the laptop-side LPC switch reports a connected dock.
    pub fn connected() -> bool {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: fixed decoded DLPC switch.
        unsafe {
            inb(DLPC_SWITCH) & 8 != 0
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            false
        }
    }

    /// Connect using the firmware's legacy EC channel.
    pub fn dock_connect() -> Result<(), ()> {
        dock_connect_with_ec(fstart_driver_lenovo::ec::Ec::new(
            crate::config::X61_H8.resources.os,
        ))
    }

    /// Connect the dock-side LPC bus and initialize dock GPIO/power.
    #[cfg(target_arch = "x86_64")]
    pub fn dock_connect_with_ec(ec: fstart_driver_lenovo::ec::Ec) -> Result<(), ()> {
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
            LogicalDevice::select(0x19, dlpc_read, dlpc_write).set_enabled(false);
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
        let mut gpio = LogicalDevice::select(0x07, dock_read, dock_write);
        gpio.set_io_base(IoResource::Primary(DOCK_GPIO_BASE));

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
        dock_gpio_set_mode(
            0x07,
            PC87392_GPIO_PIN_PULLUP | PC87392_GPIO_PIN_DEBOUNCE,
            PC87392_GPIO_PIN_TRIGGERS_SMI,
        );
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

        gpio.set_enabled(true);
        // SAFETY: dock GPIO block is configured at 0x1620.
        unsafe {
            set_ultrabay_power(ultrabay_present());
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

    /// Connect the dock-side LPC bus and initialize dock GPIO/power.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn dock_connect_with_ec(_ec: fstart_driver_lenovo::ec::Ec) -> Result<(), ()> {
        Ok(())
    }

    /// UltraBay presence for board IDE/power policy; never sample an absent dock.
    #[cfg(target_arch = "x86_64")]
    pub fn ultrabay_present() -> bool {
        // SAFETY: dock GPIO input is decoded while connected.
        connected() && unsafe { inb(DOCK_GPIO_BASE + 1) & 2 == 0 }
    }

    #[cfg(target_arch = "x86_64")]
    fn set_ultrabay_power(on: bool) {
        // SAFETY: decoded dock power register; preserve USB and other rails.
        unsafe {
            let previous = inb(DOCK_GPIO_BASE + 8);
            outb(
                DOCK_GPIO_BASE + 8,
                if on { previous | 1 } else { previous & !1 },
            );
        }
    }

    #[cfg(target_arch = "x86_64")]
    fn set_usb_power(ec: fstart_driver_lenovo::ec::Ec, on: bool) -> bool {
        // SAFETY: decoded dock power register; preserve UltraBay/other rails.
        unsafe {
            let previous = inb(DOCK_GPIO_BASE + 8);
            outb(
                DOCK_GPIO_BASE + 8,
                if on { previous | 2 } else { previous & !2 },
            );
        }
        if on {
            ec.set_bit(0x02, 0)
        } else {
            ec.clear_bit(0x02, 0)
        }
    }

    /// Mainboard UltraBay power/LED policy after the H8 configuration reset.
    #[cfg(target_arch = "x86_64")]
    pub fn mainstage_power_policy() {
        let present = ultrabay_present();
        if connected() {
            set_ultrabay_power(present);
            let _ = set_usb_power(
                fstart_driver_lenovo::ec::Ec::new(crate::config::X61_H8.resources.os),
                true,
            );
        }
        let h8 = fstart_driver_lenovo::h8::H8::new(fstart_driver_lenovo::ec::Ec::new(
            crate::config::X61_H8.resources.os,
        ));
        use fstart_driver_lenovo::h8::{H8Led, H8LedMode};
        let _ = h8.set_led(
            H8Led::Ultrabay,
            if present {
                H8LedMode::On
            } else {
                H8LedMode::Off
            },
        );
    }

    /// Disconnect the dock-side LPC bus and power rails, in vendor order.
    #[cfg(target_arch = "x86_64")]
    pub fn dock_disconnect_with_ec(ec: fstart_driver_lenovo::ec::Ec) {
        // Assert D_PLTRST# and DLPCPD before removing power/LPC.
        unsafe { outb(DLPC_GPIO, inb(DLPC_GPIO) & 0xfc) };
        delay_ms(10);
        let _ = set_usb_power(ec, false);
        set_ultrabay_power(false);
        delay_us(10_000);
        // SAFETY: fixed DLPC switch, disconnected only after power removal.
        unsafe { outb(DLPC_SWITCH, 0x00) };
    }

    pub fn dock_disconnect() {
        dock_disconnect_with_ec(fstart_driver_lenovo::ec::Ec::new(
            crate::config::X61_H8.resources.os,
        ));
    }

    /// Disconnect the dock-side LPC bus and power rails.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn dock_disconnect_with_ec(_ec: fstart_driver_lenovo::ec::Ec) {}

    /// Enable the dock-side PC87392 COM1 at 0x3f8.
    #[cfg(target_arch = "x86_64")]
    pub fn early_superio_config() {
        let mut timeout = 100_000;
        dock_write(0x29, 0x06);
        while (dock_read(0x29) & 0x08) == 0 && timeout != 0 {
            timeout -= 1;
            delay_us(1000);
        }
        enable_dock_console();
        disable_dock_watchdog();
    }

    #[cfg(target_arch = "x86_64")]
    fn enable_dock_console() {
        let mut serial = LogicalDevice::select(0x03, dock_read, dock_write);
        serial.set_io_base(IoResource::Primary(0x3f8));
        serial.set_irq(IrqResource::Primary(4));
        serial.set_enabled(true);
    }

    /// Enable the dock-side PC87392 COM1 at 0x3f8.
    #[cfg(not(target_arch = "x86_64"))]
    pub fn early_superio_config() {}

    /// Keep the dock-side PC87392 watchdog LDN disabled, matching coreboot's
    /// `device pnp 2e.a off end # WDT` for the X61 dock SuperIO.
    #[cfg(target_arch = "x86_64")]
    fn disable_dock_watchdog() {
        const PC87392_WDT_LDN: u8 = 0x0a;
        LogicalDevice::select(PC87392_WDT_LDN, dock_read, dock_write).set_enabled(false);
    }

    /// Legacy SMM EC query mapping, distinct from the OS AML hotkey policy.
    pub const fn smm_event_command(event: u8) -> Option<u8> {
        use fstart_driver_lenovo::h8::H8DockEvent;
        match H8DockEvent::from_query(event) {
            Some(H8DockEvent::FnF9 | H8DockEvent::AcLost | H8DockEvent::DockDisconnected) => {
                Some(2)
            }
            Some(H8DockEvent::DockConnected | H8DockEvent::DockConnectedAlternate) => Some(1),
            None => None,
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn smm_ec_dock_event_mapping_matches_coreboot() {
            for event in [0x18, 0x27, 0x50] {
                assert_eq!(smm_event_command(event), Some(2));
            }
            for event in [0x37, 0x58] {
                assert_eq!(smm_event_command(event), Some(1));
            }
            assert_eq!(smm_event_command(0x14), None);
        }
    }

    /// Switch the X61 SMBus mux back to the EEPROM side after SPD/raminit.
    pub fn post_raminit_setup(southbridge: &impl fstart_core::services::Southbridge) {
        let _ = southbridge.gpio_set(42, false);
    }
}

/// CK505 shares the SPD side of GPIO42's SMBus mux. Always restore EEPROM
/// routing, including a failed block transfer.
#[cfg(fstart_stage_env = "ram")]
fn init_ck505(
    southbridge: &mut (impl fstart_core::services::Southbridge + fstart_core::services::SmBus),
) -> Result<(), ServiceError> {
    use fstart_driver_intel::generic::ck505::I2cCk505;

    let mut clock = I2cCk505::new_at_address(crate::config::x61_ck505_config(), 0x69)
        .map_err(|_| ServiceError::HardwareError)?;
    southbridge.gpio_set(42, true)?;
    let programmed = clock.init_on_smbus(southbridge);
    let restored = southbridge.gpio_set(42, false);
    restored?;
    programmed.map_err(|_| ServiceError::HardwareError)
}

/// Board quirk for the Ricoh SD controller behind the ICH PCI bridge. Byte
/// accesses are essential: F9 is a write-protect key, not a dword RMW field.
#[cfg(all(not(test), fstart_stage_env = "ram"))]
fn ricoh_sd_write_protect() {
    use fstart_driver_intel::ich8::ich8::{PCI_BRIDGE_DEV, PCI_BRIDGE_FUNC};
    use fstart_pci::{EcamDevice, PciType1Config};
    use tock_registers::interfaces::Readable;
    const RICOH_VENDOR: u16 = 0x1180;
    const RICOH_R5C822: u16 = 0x0822;
    const WRITE_PROTECT_KEY: u16 = 0xf9;
    const SD_CONTROL: u16 = 0xfa;
    const KEY_UNLOCK: u8 = 0xfc;
    const KEY_LOCK: u8 = 0;
    const SD_WRITE_PROTECT_POLARITY: u8 = 1 << 5;
    let bridge = EcamDevice::new(0, PCI_BRIDGE_DEV, PCI_BRIDGE_FUNC);
    // SAFETY: the enumerated ICH8 PCI bridge has a mapped Type 1 header.
    let bus = unsafe { bridge.regs::<PciType1Config>() }
        .secondary_bus
        .get();
    if bus == 0 || bus == 0xff {
        return;
    }
    for dev in 0..32u8 {
        for function in 0..8u8 {
            let sd = EcamDevice::new(bus, dev, function);
            if sd.vendor_id() != RICOH_VENDOR || sd.device_id() != RICOH_R5C822 {
                continue;
            }
            if sd.read8(SD_CONTROL) != SD_WRITE_PROTECT_POLARITY {
                sd.write8(WRITE_PROTECT_KEY, KEY_UNLOCK);
                // Exact policy write: SDWPPol, no CLKRUNDis/SDPWRPol.
                sd.write8(SD_CONTROL, SD_WRITE_PROTECT_POLARITY);
                sd.write8(WRITE_PROTECT_KEY, KEY_LOCK);
            }
            return;
        }
    }
}

static X61_SMBIOS_PROCESSOR_SOCKETS: [&str; 1] = ["Socket M"];

pub static X61_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X61",
        sys_version: "1.0",
        sys_serial: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X61",
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &X61_SMBIOS_PROCESSOR_SOCKETS,
    };

#[cfg(fstart_stage_env = "ram")]
mod acpi_impl {
    extern crate alloc;

    use alloc::vec::Vec;
    use fstart_acpi_macros::acpi_dsl;
    use fstart_platform_intel::gm965::Gm965Ich8AcpiContext;

    /// Assemble the X61 DSDT: board glue (TCO dock commands, sleep/wake hooks,
    /// dock, GPE routing) plus the complete H8 EC surface from the Lenovo
    /// driver.
    pub fn x61_mainboard_dsdt_aml(context: Gm965Ich8AcpiContext) -> Vec<u8> {
        let mut out = Vec::new();

        // Root sleep/wake glue. Dock commands use the existing TCO mailbox,
        // not an unbacked AML SMIF variable or an I/O trap/GNVS channel.
        out.extend_from_slice(&acpi_dsl! {
            Scope("\\") {
                Method("_PTS", 1, NotSerialized) {
                    #{const "\\_SB_.PCI0.LPCB.EC__.MUTE"}(1u32);
                    #{const "\\_SB_.PCI0.LPCB.EC__.USBP"}(0u32);
                    #{const "\\_SB_.PCI0.LPCB.EC__.RADI"}(0u32);
                    #{const "\\_SB_.PCI0.LPCB.EC__.HKEY.MHKC"}(0u32);
                }
                Method("_WAK", 1, NotSerialized) {
                    #{const "\\_SB_.PCI0.LPCB.EC__.HKEY.MHKC"}(1u32);
                    #{const "\\_SB_.PCI0.LPCB.EC__.HKEY.WAKE"}(Arg0);
                    Return(Package(0u32, 0u32));
                }
            }
        });

        // Dock: DLPC presence + Toshiba dock registers under \_SB.
        out.extend_from_slice(&acpi_dsl! {
            Scope(#{const "\\_SB_"}) {
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
                    Name("_PCL", Package(#{const "\\_SB_"}));
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
        out.extend_from_slice(&acpi_dsl! {
            Scope(#{const "\\_GPE"}) {
                Method("_L18", 0, NotSerialized) {
                    Local0 = #{const "\\_SB_.PCI0.LPCB.EC__.WAKE"};
                    If (Local0 & 0x04u32) {
                        Notify(#{const "\\_SB_.PCI0.LPCB.EC__.LID_"}, 0x02u32);
                    }
                    If (Local0 & 0x08u32) {
                        Notify(#{const "\\_SB_.DOCK"}, 0x03u32);
                        Notify(#{const "\\_SB_.PCI0.LPCB.EC__.SLPB"}, 0x02u32);
                    }
                    If (Local0 & 0x10u32) {
                        Notify(#{const "\\_SB_.PCI0.LPCB.EC__.SLPB"}, 0x02u32);
                    }
                    If (Local0 & 0x80u32) {
                        Notify(#{const "\\_SB_.PCI0.LPCB.EC__.SLPB"}, 0x02u32);
                    }
                }
            }
        });

        // The complete H8 EC surface (EC device, batteries, thermal zones
        // with fan power resource, lid, AC, sleep button, HKEY hub, and the
        // PMH7/ECMM/ECGS/TWRI resource devices).
        let mut h8 = crate::config::X61_H8;
        h8.has_bluetooth = super::ec::bluetooth_present();
        let brightness = acpi_dsl! {
            Scope("\\") {
                Method("BRTU", 0, NotSerialized) { #{const "\\_SB_.PCI0.GFX0.INCB"}(); }
                Method("BRTD", 0, NotSerialized) { #{const "\\_SB_.PCI0.GFX0.DECB"}(); }
            }
        };
        out.extend(fstart_driver_lenovo::h8_acpi::dsdt_aml_with_brightness(
            &h8,
            context.lpc_scope(),
            &brightness,
        ));

        // Open the EC scope only after its Device declaration. This also
        // allows standalone ACPICA disassembly/recompilation without externals.
        out.extend_from_slice(&acpi_dsl! {
            Scope(#{const "\\_SB_.PCI0.LPCB.EC__"}) {
                Method("_Q18", 0, NotSerialized) { #{const "^HKEY.RHK_"}(0x09u32); }
                Method("_Q37", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
                Method("_Q50", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 3u32); }
                Method("_Q58", 0, NotSerialized) { Notify(#{const "\\_SB_.DOCK"}, 0u32); }
            }
        });

        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use fstart_acpi::device::AcpiDevice;
        use fstart_driver_intel::gm965::IntelGm965;
        use fstart_driver_intel::ich8::IntelIch8;
        use fstart_driver_intel::{IntelNorthbridgeDriver, IntelSouthbridgeDriver};
        use std::fs;
        use std::process::Command;

        struct ClockBus {
            mux: std::sync::Mutex<std::vec::Vec<(u32, bool)>>,
            block: [u8; 4],
            fail_transfer: bool,
        }

        impl fstart_core::services::Southbridge for ClockBus {
            fn gpio_set(&self, pin: u32, high: bool) -> Result<(), crate::mainboard::ServiceError> {
                self.mux.lock().unwrap().push((pin, high));
                Ok(())
            }
        }

        impl fstart_core::services::SmBus for ClockBus {
            fn read_byte(&mut self, _: u8, _: u8) -> Result<u8, crate::mainboard::ServiceError> {
                panic!("CK505 requires block transfers");
            }
            fn write_byte(
                &mut self,
                _: u8,
                _: u8,
                _: u8,
            ) -> Result<(), crate::mainboard::ServiceError> {
                panic!("CK505 requires block transfers");
            }
            fn block_read(
                &mut self,
                addr: u8,
                command: u8,
                data: &mut [u8],
            ) -> Result<usize, crate::mainboard::ServiceError> {
                assert_eq!((addr, command), (0x69, 0));
                assert_eq!(self.mux.lock().unwrap().last(), Some(&(42, true)));
                if self.fail_transfer {
                    return Err(crate::mainboard::ServiceError::IoError);
                }
                data[..4].copy_from_slice(&self.block);
                Ok(4)
            }
            fn block_write(
                &mut self,
                addr: u8,
                command: u8,
                data: &[u8],
            ) -> Result<(), crate::mainboard::ServiceError> {
                assert_eq!((addr, command), (0x69, 0));
                assert_eq!(self.mux.lock().unwrap().last(), Some(&(42, true)));
                self.block.copy_from_slice(data);
                Ok(())
            }
        }

        #[test]
        fn clock_mux_is_restored_after_success_and_transfer_failure() {
            for fail_transfer in [false, true] {
                let mut bus = ClockBus {
                    mux: std::sync::Mutex::new(std::vec::Vec::new()),
                    block: [0x04, 0xa5, 0x5a, 0xff],
                    fail_transfer,
                };
                assert_eq!(
                    crate::mainboard::init_ck505(&mut bus).is_err(),
                    fail_transfer
                );
                assert_eq!(*bus.mux.lock().unwrap(), [(42, true), (42, false)]);
            }
        }

        #[test]
        fn complete_dsdt_iasl_round_trip() {
            let north_config = &crate::X61_PLATFORM.northbridge;
            let south_config = &crate::X61_PLATFORM.southbridge;
            let north = IntelGm965::new_from_config(north_config).unwrap();
            let south = IntelIch8::new_from_config(south_config).unwrap();

            let mut aml = north.dsdt_aml(north_config);
            aml.extend(south.dsdt_aml(south_config));
            aml.extend(x61_mainboard_dsdt_aml(Gm965Ich8AcpiContext));
            let dsdt = fstart_acpi::platform::build_dsdt(&aml);

            let dir =
                std::env::temp_dir().join(std::format!("fstart-x61-dsdt-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("dsdt.aml"), &dsdt).unwrap();

            let disassemble = Command::new("iasl")
                .current_dir(&dir)
                .args(["-d", "dsdt.aml"])
                .output()
                .unwrap();
            assert!(
                disassemble.status.success(),
                "iasl -d failed: {}{}",
                std::string::String::from_utf8_lossy(&disassemble.stdout),
                std::string::String::from_utf8_lossy(&disassemble.stderr)
            );
            let source = fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
            for method in ["_Q18", "_Q37", "_Q50", "_Q58", "BRTU", "BRTD"] {
                assert!(source.contains(&std::format!("Method ({method},")));
            }
            for (method, event) in [("_Q37", "Zero"), ("_Q58", "Zero"), ("_Q50", "0x03")] {
                let body = source
                    .split(&std::format!("Method ({method},"))
                    .nth(1)
                    .unwrap()
                    .split('}')
                    .next()
                    .unwrap();
                assert!(body.contains(&std::format!("Notify (\\_SB.DOCK, {event})")));
            }
            assert!(source.contains("HKEY.RHK (0x09)"));
            assert!(source.contains("GFX0.INCB ()"));
            assert!(source.contains("GFX0.DECB ()"));
            assert!(!source.contains("SMIF"));
            assert!(!source.contains("Method (TRAP,"));
            fs::rename(dir.join("dsdt.aml"), dir.join("original.aml")).unwrap();
            let compile = Command::new("iasl")
                .current_dir(&dir)
                .args(["-oa", "-tc", "dsdt.dsl"])
                .output()
                .unwrap();
            assert!(
                compile.status.success(),
                "iasl -tc failed: {}{}",
                std::string::String::from_utf8_lossy(&compile.stdout),
                std::string::String::from_utf8_lossy(&compile.stderr)
            );

            // ACPICA narrows pre-existing shared Intel integer encodings on
            // the first pass. Once canonicalized, require stable AML bytes.
            let canonical = fs::read(dir.join("dsdt.aml")).unwrap();
            fs::rename(dir.join("dsdt.dsl"), dir.join("original.dsl")).unwrap();
            for arguments in [&["-d", "dsdt.aml"][..], &["-oa", "-tc", "dsdt.dsl"][..]] {
                let output = Command::new("iasl")
                    .current_dir(&dir)
                    .args(arguments)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "canonical roundtrip failed: {}{}",
                    std::string::String::from_utf8_lossy(&output.stdout),
                    std::string::String::from_utf8_lossy(&output.stderr)
                );
            }
            let recompiled = fs::read(dir.join("dsdt.aml")).unwrap();
            // Ignore only compiler-identification fields in the table header.
            assert_eq!(&recompiled[36..], &canonical[36..]);
            fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[cfg(fstart_stage_env = "ram")]
pub use acpi_impl::x61_mainboard_dsdt_aml;

#[cfg(all(
    any(
        fstart_stage_env = "car",
        fstart_stage_env = "postcar",
        fstart_stage_env = "ram"
    ),
    fstart_stage_env = "ram"
))]
mod ec;
#[cfg(all(
    any(
        fstart_stage_env = "car",
        fstart_stage_env = "postcar",
        fstart_stage_env = "ram"
    ),
    fstart_stage_env = "ram"
))]
pub use ec::x61_ec_init;
