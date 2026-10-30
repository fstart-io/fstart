//! T6 board work. Reusable drivers own register encodings; the family owns
//! dock/EC sequencing. No S3, ATI initialization/brightness, TI CardBus vendor
//! setup, IR, parallel-port or thermal-safety support is claimed.

use embedded_hal::digital::InputPin;
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};

pub const fn console() -> Ns16550Config {
    Ns16550Config {
        regs: AccessMode::Pio {
            base: crate::UART0_PIO_BASE as u64,
        },
        clock_freq: crate::UART0_CLOCK_FREQ,
        baud_rate: crate::UART0_BAUD_RATE,
    }
}

pub fn setup_console(legacy_module: &mut impl InputPin) -> bool {
    fstart_driver_lenovo::t6::dock::setup_console(legacy_module, &crate::T6_H8).is_err()
}

fn os_ec() -> fstart_driver_lenovo::ec::Ec {
    fstart_driver_lenovo::ec::Ec::new(crate::T6_H8.resources.os)
}

pub struct Devices {
    pub ide_primary: Option<bool>,
    pub native_brightness: bool,
}

pub fn init_ec_and_bay(
    bdc: &mut impl InputPin,
    legacy_module: &mut impl InputPin,
    console_failed: bool,
) -> Result<Devices, ServiceError> {
    use fstart_driver_lenovo::{
        h8::H8,
        t6::{self, dock},
    };
    if !t6::ec_init(crate::MODEL, bdc, &crate::T6_H8, false) {
        fstart_log::error!("T6: EC/thermal initialization incomplete");
    }
    let legacy = dock::legacy_io_present(legacy_module)?;
    let bay = H8::new(os_ec()).ultrabay_device_present();
    let ide_primary = if let Some(present) = bay {
        Some(dock::ide_primary_enabled(legacy, present))
    } else {
        fstart_log::error!("T6: Ultrabay presence unavailable; PATA remains disabled");
        None
    };
    if !dock::mainstage_power_policy(os_ec(), &crate::T6_H8, legacy) {
        fstart_log::error!("T6: Ultrabay power/LED policy incomplete");
    }
    let native_brightness = fstart_pci::EcamDevice::new(0, 2, 0).vendor_id() == 0x8086;
    fstart_log::info!(
        "T6: experimental, no hardware/thermal validation; S3, ATI, TI CardBus setup, IR and parallel unsupported"
    );
    if console_failed {
        fstart_log::error!("T6: dock/module setup failed; serial console may be unavailable");
    }
    Ok(Devices {
        ide_primary,
        native_brightness,
    })
}

pub struct Identity {
    eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    ec_oem: Option<fstart_driver_lenovo::h8::EcOemString>,
}

impl Identity {
    pub fn smbios(&self) -> fstart_acpi::smbios::SmbiosIdentity<'_> {
        let mut result = T6_SMBIOS_IDENTITY;
        if let Some(eeprom) = &self.eeprom {
            result.sys_product = eeprom.part_number();
            result.sys_version = eeprom.version();
            result.sys_serial = Some(eeprom.serial_number());
            result.sys_uuid = Some(eeprom.uuid);
            result.bb_product = eeprom.part_number();
            result.bb_version = eeprom.version();
            result.bb_serial = Some(eeprom.serial_number());
        }
        result.oem_string = self.ec_oem.as_ref().map(|oem| oem.as_str());
        result
    }
}

pub fn init_clock_and_identity(smbus: &mut impl SmBus) -> Result<Identity, ServiceError> {
    use fstart_driver_intel::generic::ck505::I2cCk505;
    let mut clock = I2cCk505::new_at_address(crate::ck505_config(), 0x69)?;
    if clock.init_on_smbus(smbus).is_err() {
        fstart_log::error!("T6: CK505 initialization failed");
    }
    let eeprom = fstart_driver_lenovo::eeprom::Identity::read(smbus).ok();
    let ec_oem = fstart_driver_lenovo::h8::H8::new(os_ec()).smbios_oem_string();
    Ok(Identity { eeprom, ec_oem })
}

pub fn handoff_ec_events() {
    use fstart_driver_intel::southbridge::smi::{APM_CNT, APM_CNT_ACPI_DISABLE};
    // SAFETY: permanent board SMM has been installed and owns APMC.
    unsafe { fstart_core::pio::outb(APM_CNT, APM_CNT_ACPI_DISABLE) };
}

pub struct Acpi {
    pub native_brightness: bool,
}

impl fstart_acpi::device::AcpiDevice for Acpi {
    type Config = fstart_platform_intel::i945::I945Ich7AcpiContext;
    fn dsdt_aml(&self, ctx: &Self::Config) -> alloc::vec::Vec<u8> {
        fstart_driver_lenovo::t6::acpi::dsdt_aml(
            &crate::T6_H8,
            ctx.lpc_scope(),
            self.native_brightness,
        )
    }
}

static SOCKETS: [&str; 1] = ["Socket M"];
pub static T6_SMBIOS_IDENTITY: fstart_acpi::smbios::SmbiosIdentity<'static> =
    fstart_acpi::smbios::SmbiosIdentity {
        bios_vendor: "fstart",
        bios_version: "CBET4000 fstart 0.1.0",
        bios_release_date: fstart_platform_intel::SMBIOS_RELEASE_DATE,
        sys_manufacturer: "LENOVO",
        sys_product: crate::PRODUCT_NAME,
        sys_version: crate::PRODUCT_NAME,
        sys_serial: None,
        sys_uuid: None,
        bb_manufacturer: "LENOVO",
        bb_product: crate::PRODUCT_NAME,
        bb_version: "",
        bb_serial: None,
        chassis_type: 0x0a,
        chassis_manufacturer: "LENOVO",
        processor_sockets: &SOCKETS,
        oem_string: None,
    };
