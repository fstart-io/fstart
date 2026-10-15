//! X200 wiring: GPIO42 SMBus mux, H8/PMH7 policy and dock connection.
use fstart_core::services::{ServiceError, Southbridge};
use fstart_platform_intel::gm45::Gm45Ich9;
use fstart_platform_intel::{IntelSmbusRouting, SmbusRoute};

#[derive(Default)]
pub struct X200Mainboard {
    #[cfg(fstart_stage_env = "ram")]
    bluetooth_present: bool,
    #[cfg(fstart_stage_env = "ram")]
    ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
    #[cfg(fstart_stage_env = "ram")]
    identity: Option<fstart_driver_lenovo::eeprom::Identity>,
    #[cfg(fstart_stage_env = "ram")]
    wacom_present: bool,
}

/// variants/x200/overridetree.cb: the PC87382 serves only the X200t
/// digitizer (LDN 3); IR, GPIO and DLPC are not connected.
#[cfg(fstart_stage_env = "ram")]
const PC87382_BASE: u16 = 0x164e;
#[cfg(fstart_stage_env = "ram")]
const PC87382_DEVICES: &[fstart_driver_superio::pc87382::PnpDevice] = {
    use fstart_driver_superio::pc87382::PnpDevice;
    const fn off(ldn: u8) -> PnpDevice {
        PnpDevice {
            ldn,
            enabled: false,
            io: None,
            registers: &[],
        }
    }
    &[
        PnpDevice {
            ldn: 3,
            enabled: true,
            io: Some(0x200),
            registers: &[(0x29, 0xb0), (0x70, 0x05), (0xf0, 0x82)],
        },
        off(2),
        off(7),
        off(0x19),
    ]
};

impl<S: Southbridge> IntelSmbusRouting<S> for X200Mainboard {
    fn select_smbus(sb: &S, route: SmbusRoute) -> Result<(), ServiceError> {
        sb.gpio_set(42, matches!(route, SmbusRoute::Spd))
    }
}

#[cfg(fstart_stage_env = "car")]
impl fstart_platform_intel::IntelEarlyBoardHooks<Gm45Ich9> for X200Mainboard {}

#[cfg(fstart_stage_env = "ram")]
const H8_CONFIG: fstart_driver_lenovo::h8::H8Config = {
    use fstart_driver_lenovo::{
        ec::EcPorts,
        h8::{H8BatteryThresholdBackend, H8Config, H8Resources},
    };
    H8Config {
        has_bluetooth: true,
        has_wwan: true,
        has_uwb: true,
        has_thinklight: true,
        second_thermal_zone: true,
        critical_temp_celsius: 100,
        passive_temp_celsius: 90,
        bat_info_extended: true,
        bat_thresholds: true,
        bat_threshold_backend: H8BatteryThresholdBackend::Registers24 { bit7: false },
        ..H8Config::new(
            H8Resources::new(EcPorts::new(0x62, 0x66), 0x1600, 0x15e0),
            0x11,
            "IBM0068",
        )
    }
};

#[cfg(fstart_stage_env = "ram")]
impl fstart_platform_intel::IntelMainstageBoardHooks<Gm45Ich9> for X200Mainboard {
    fn smbios_identity<'a>(
        &'a self,
        configured: &fstart_platform_intel::tables::SmbiosIdentity<'a>,
    ) -> fstart_platform_intel::tables::SmbiosIdentity<'a> {
        let mut identity = fstart_platform_intel::tables::SmbiosIdentity {
            oem_string: self.ec_oem_string.as_ref().map(|oem| oem.as_str()),
            ..*configured
        };
        // Coreboot at24rf08c/lenovo_serials.c: the EEPROM part number, version
        // ("ThinkPad X200"), serial and UUID describe system and baseboard.
        if let Some(eeprom) = &self.identity {
            identity.sys_product = eeprom.part_number();
            identity.sys_version = eeprom.version();
            identity.sys_serial = Some(eeprom.serial_number());
            identity.sys_uuid = Some(eeprom.uuid);
            identity.bb_product = eeprom.part_number();
            identity.bb_version = eeprom.version();
            identity.bb_serial = Some(eeprom.serial_number());
        }
        identity
    }

    fn after_devices(
        &mut self,
        ctx: &mut fstart_platform_intel::IntelMainstageBoardCtx<Gm45Ich9>,
    ) -> Result<(), ServiceError> {
        use fstart_driver_lenovo::{
            ec::Ec,
            h8::{CONFIG2, H8, H8Illumination},
            pmh7::Pmh7,
        };
        // The platform routed the SMBus mux to the EEPROM before this hook.
        // Identity reads never touch EEPROM data or protection registers.
        self.identity = fstart_driver_lenovo::eeprom::Identity::read(ctx.southbridge())
            .inspect_err(|error| {
                fstart_log::error!("x200: EEPROM identity unavailable, code {}", *error as u8)
            })
            .ok();
        fstart_driver_superio::pc87382::configure(PC87382_BASE, PC87382_DEVICES);
        // Coreboot drivers/lenovo/wacom.c: a responding SuperIO and a tablet
        // machine type.
        self.wacom_present = fstart_driver_superio::pc87382::sid(PC87382_BASE) != 0xff
            && self
                .identity
                .as_ref()
                .is_some_and(|eeprom| fstart_driver_lenovo::wacom::is_tablet(eeprom.part_number()));
        let sb = ctx.southbridge();
        self.bluetooth_present = !sb.gpio_get(7)?;
        let h8 = H8::new(Ec::new(H8_CONFIG.resources.os));
        h8.clear_out_queue();
        let masks = [
            0, 0, 0xff, 0xff, 0xf4, 0x3c, 0x80, 0x01, 0x01, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0,
        ];
        let controls = [
            h8.init_config0(0xa6),
            h8.init_controls(
                [0x04, 0xa0, 0x01],
                Some(H8Illumination::Thinklight),
                [0xfe, 0x96],
            ),
            h8.program_event_masks(&masks),
            h8.bluetooth_enable(self.bluetooth_present),
            h8.wwan_enable(true),
            h8.uwb_enable(true),
            h8.wlan_enable(true),
            h8.trackpoint_enable(true),
            h8.usb_power_enable(true),
            h8.usb_always_on_disable(),
            h8.fn_ctrl_swap(false),
            h8.sticky_fn(false),
            h8.charge_primary_first(true),
            h8.audio_mute(false),
            h8.volume(3),
        ];
        if !controls.into_iter().all(core::convert::identity) {
            fstart_log::error!("x200: EC configuration failed");
        }
        self.ec_oem_string = h8.smbios_oem_string();
        if self.ec_oem_string.is_none() {
            fstart_log::error!("x200: H8 EC firmware id unavailable");
        }
        let pmh7 = Pmh7::new(0x15e0);
        pmh7.backlight_enable(true);
        // variants/x200/overridetree.cb: dock_event_enable.
        pmh7.dock_event_enable(true);
        pmh7.trackpoint_enable(true);
        pmh7.touchpad_enable(true);
        let dock_id = [2, 3, 4].into_iter().try_fold(0, |id, pin| {
            Ok::<_, ServiceError>(id | (u8::from(sb.gpio_get(pin)?) << (pin - 2)))
        })?;
        if dock_id != 7 {
            if !Ec::new(H8_CONFIG.resources.os).modify_register(CONFIG2::DOCK_USB_POWER::SET) {
                fstart_log::error!("x200: dock EC handshake failed");
            } else {
                sb.gpio_set(28, true)?;
            }
        }
        Ok(())
    }
}

#[cfg(fstart_stage_env = "ram")]
pub static IDENTITY: fstart_platform_intel::tables::SmbiosIdentity<'static> =
    fstart_platform_intel::tables::SmbiosIdentity {
        bios_vendor: "fstart",
        // Non-OEM firmware ID, following coreboot and the X61 binding;
        // Linux thinkpad_acpi validates this before the ThinkPad model string.
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: "ThinkPad X200",
        sys_version: "ThinkPad X200",
        sys_serial: None,
        sys_uuid: None,
        oem_string: None,
        bb_manufacturer: "LENOVO",
        bb_product: "ThinkPad X200",
        bb_version: "1.0",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &["BGA956"],
    };

#[cfg(fstart_stage_env = "ram")]
impl fstart_acpi::device::AcpiDevice for X200Mainboard {
    type Config = fstart_platform_intel::gm45::Gm45Ich9AcpiContext;
    fn dsdt_aml(&self, context: &Self::Config) -> alloc::vec::Vec<u8> {
        let mut config = H8_CONFIG;
        config.has_bluetooth = self.bluetooth_present;
        fstart_driver_lenovo::x200::dsdt_aml(
            &config,
            context.lpc_scope(),
            fstart_driver_intel::ich9::ich9::DEFAULT_GPIOBASE,
            self.wacom_present,
        )
    }
}
#[cfg(fstart_stage_env = "ram")]
extern crate alloc;
