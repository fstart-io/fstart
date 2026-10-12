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
}

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
        fstart_platform_intel::tables::SmbiosIdentity {
            oem_string: self.ec_oem_string.as_ref().map(|oem| oem.as_str()),
            ..*configured
        }
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
        use fstart_acpi_macros::acpi_dsl;
        let mut config = H8_CONFIG;
        config.has_bluetooth = self.bluetooth_present;
        let mut aml = fstart_driver_lenovo::h8_acpi::dsdt_aml(&config, context.lpc_scope());
        aml.extend_from_slice(&acpi_dsl! {
            Scope("\\") {
                Method("_PTS", 1, NotSerialized) {
                    #{const "\\_SB_.PCI0.LPCB.EC__.MUTE"}(1u32);
                    #{const "\\_SB_.PCI0.LPCB.EC__.USBP"}(0u32);
                    #{const "\\_SB_.PCI0.LPCB.EC__.RADI"}(0u32);
                }
                Method("_WAK", 1, NotSerialized) {
                    #{const "\\_SB_.PCI0.LPCB.EC__.HKEY.WAKE"}(Arg0);
                    Return(Package(0u32, 0u32));
                }
            }
            Scope("\\_GPE") {
                Method("_L18", 0, NotSerialized) {
                    Local0 = #{const "\\_SB_.PCI0.LPCB.EC__.WAKE"};
                }
            }
        });
        aml
    }
}
#[cfg(fstart_stage_env = "ram")]
extern crate alloc;
