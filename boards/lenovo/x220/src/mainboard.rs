//! Lenovo ThinkPad X220 mainboard work the stage `main`s call: the
//! AT24RF08C RFID lock, H8/PMH7 policy, the RCE822 card reader, the TPM
//! and the identity EEPROM.

use embedded_hal::digital::InputPin;
use fstart_core::services::SmBus;
use fstart_driver_intel::southbridge::gpio_ich::{IchGpio, IchPin};
use fstart_driver_lenovo::ec::Ec;
use fstart_driver_lenovo::h8::{H8, H8Illumination};
use fstart_driver_lenovo::pmh7::Pmh7;
use fstart_pci::EcamDevice;

use crate::config::{X220_H8, X220_H8_BEEPS, X220_H8_CONFIG, X220_H8_EVENTS};

/// LPC I/O the EC/PMH7 decode windows and the GEN4 window claim, kept out
/// of PCI I/O allocation (coreboot `pch_lpc_read_resources` reserves the
/// generic decode ranges above 0x1000 and the H8's own ports).
pub const IO_RESOURCES: [fstart_core::acpi::AcpiResource; 9] = {
    let h8 = X220_H8.resources.io_resources();
    [
        h8[0],
        h8[1],
        h8[2],
        h8[3],
        h8[4],
        h8[5],
        h8[6],
        h8[7],
        fstart_core::acpi::AcpiResource::Pio {
            base: 0x06a0,
            size: 0x10,
        },
    ]
};

/// The PCH GPIOs the board's own devices hang off.
pub struct Pins {
    /// WWAN card fitted (low when present).
    pub wwan_presence: IchPin,
}

impl Pins {
    pub const fn of(gpio: IchGpio) -> Self {
        Self {
            wwan_presence: gpio.pin(crate::config::WWAN_PRESENCE_GPIO),
        }
    }
}

/// coreboot `mainboard_early_init`: lock the identity EEPROM's RFID
/// interface before raminit.
pub fn early_init(smbus: &mut impl SmBus) {
    if fstart_driver_lenovo::eeprom::lock_rfid(smbus).is_err() {
        fstart_log::error!("lenovo-x220: EEPROM RFID lock failed");
    }
}

/// What the board read from its own hardware for the tables.
#[derive(Default)]
pub struct Identity {
    pub eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    pub ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

/// The EC options this ThinkPad has, as its coreboot `cmos.layout` lists
/// them. Their board values are coreboot's defaults.
pub static EC: fstart_options::OptionSet<fstart_driver_lenovo::options::Options> = {
    use fstart_driver_lenovo::options::*;
    fstart_options::OptionSet {
        knobs: &[
            FIRST_BATTERY,
            BLUETOOTH,
            WWAN,
            TOUCHPAD,
            WLAN,
            TRACKPOINT,
            FN_CTRL_SWAP,
            STICKY_FN,
            POWER_MANAGEMENT_BEEPS,
            USB_ALWAYS_ON,
            VOLUME,
        ],
        board: Options::DEFAULT,
    }
};

/// coreboot `h8_enable` with the X220 devicetree and the user's `ec`
/// options. Every operation runs even if an earlier EC transaction was not
/// acknowledged.
fn h8_init(wwan: bool, ec: &fstart_driver_lenovo::options::Options, resume: bool) -> bool {
    let h8 = H8::new(Ec::new(X220_H8.resources.os));
    h8.clear_out_queue();
    let [config0, config1, config2, config3] = X220_H8_CONFIG;
    let acknowledged = [
        h8.init_config0(config0),
        h8.init_controls(
            [config1, config2, config3],
            Some(H8Illumination::Thinklight),
            ec.beep_masks(X220_H8_BEEPS),
        ),
        h8.program_event_masks(&X220_H8_EVENTS),
        h8.usb_always_on(ec.usb_always_on),
        h8.wlan_enable(ec.wlan),
        h8.trackpoint_enable(ec.trackpoint),
        h8.usb_power_enable(true),
        resume || h8.volume(ec.volume),
        // No BDC detection on this board: assume the card is fitted.
        h8.bluetooth_enable(ec.bluetooth),
        h8.wwan_enable(wwan && ec.wwan),
        h8.fn_ctrl_swap(ec.fn_ctrl_swap),
        h8.sticky_fn(ec.sticky_fn),
        h8.charge_primary_first(ec.first_battery != 0),
        h8.audio_mute(false),
    ]
    .into_iter()
    .all(core::convert::identity);
    // h8_init: the keyboard controller is left quiet for the OS.
    fstart_driver_superio::quiesce_i8042_for_os();
    acknowledged
}

/// The RCE822 behind a root port: function disable mask 0x87 and the SD
/// write-protect polarity from the devicetree, plus the subsystem ID.
fn card_reader_init() {
    for func in 0..8u8 {
        let port = EcamDevice::new(0, 0x1c, func);
        if !port.is_present() || port.read8(0x0e) & 0x7f != 1 {
            continue;
        }
        let bus = port.read8(0x19);
        if bus == 0 {
            continue;
        }
        if let Ok(reader) = fstart_driver_ricoh::rce822::Rce822::probe(EcamDevice::new(bus, 0, 0)) {
            reader.disable_functions(0x87);
            reader.set_sd_write_protect_polarity(true);
            reader.set_subsystem(0x17aa, 0x21db);
            fstart_log::info!("lenovo-x220: RCE822 on bus {}", bus);
            return;
        }
    }
}

/// PMH7, H8 with the user's `ec` options and the card reader once the
/// chipset devices are up; the identity for SMBIOS on cold boots. Restores
/// the optional TPM on S3 before returning the identity and the board ACPI
/// device.
pub fn init_devices(
    pins: &mut Pins,
    smbus: &mut impl SmBus,
    ec: &fstart_driver_lenovo::options::Options,
    resume: bool,
) -> Result<(Identity, Acpi), fstart_driver_tpm::Error> {
    let pmh7 = Pmh7::new(X220_H8.resources.pmh7_base.raw());
    pmh7.backlight_enable(true);
    pmh7.dock_event_enable(true);
    pmh7.touchpad_enable(ec.touchpad);
    pmh7.trackpoint_enable(ec.trackpoint);
    pmh7.log_identity();
    // H8_HAS_WWAN_GPIO_DETECTION: GPIO70 low means a card is fitted.
    let wwan = pins.wwan_presence.is_low().unwrap_or(false);
    if !h8_init(wwan, ec, resume) {
        fstart_log::error!("lenovo-x220: H8 EC initialization incomplete");
    }
    card_reader_init();
    let mut identity = Identity::default();
    if !resume {
        // The AT24RF08C shares the SPD bus; read-only identity access never
        // touches its data or protection registers.
        identity.eeprom = fstart_driver_lenovo::eeprom::Identity::read(smbus)
            .inspect_err(|error| {
                fstart_log::error!(
                    "lenovo-x220: EEPROM identity unavailable, code {}",
                    *error as u8
                )
            })
            .ok();
        identity.ec_oem_string = H8::new(Ec::new(X220_H8.resources.os)).smbios_oem_string();
        if identity.ec_oem_string.is_none() {
            fstart_log::error!("lenovo-x220: H8 EC firmware id unavailable");
        }
    }
    Ok((
        identity,
        Acpi {
            wwan,
            tpm: init_tpm(resume)?,
        },
    ))
}

/// Identify the optional LPC TIS TPM (coreboot `drivers/pc80/tpm`) and
/// restore its saved state on S3. Cold boots leave startup to the payload.
fn init_tpm(resume: bool) -> Result<Option<fstart_driver_tpm::Family>, fstart_driver_tpm::Error> {
    use fstart_driver_tpm::tis;
    // SAFETY: the PCH decodes the TIS window to LPC; nothing else drives it.
    let mut tis = unsafe { tis::Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
    let Some(id) = tis.probe() else {
        return Ok(None);
    };
    fstart_log::info!(
        "lenovo-x220: TPM {:#x}:{:#x} revision {}",
        id.vendor,
        id.device,
        id.revision
    );
    if resume {
        fstart_driver_tpm::resume(&mut tis, id.family)?;
        fstart_log::info!("lenovo-x220: TPM S3 state restored");
    }
    Ok(Some(id.family))
}

/// Route EC events to the installed SMM handler until OSPM enables ACPI.
/// Call only on cold boot: S3 must retain the OS's SCI ownership.
pub fn legacy_ec_events() {
    use fstart_driver_intel::southbridge::smi::{ICH8_GPE0, IchSmi};
    let smi = IchSmi::new(
        fstart_platform_intel::sandybridge::BD82X6X_PMBASE,
        ICH8_GPE0,
    );
    // SAFETY: the BSP owns PCI config after MP/SMM installation completes.
    unsafe { smi.route_gpi(crate::config::EC_SCI_GPI, false) };
}

/// The board's ACPI: H8 EC surface with ThinkLight, sleep hooks, the PS/2
/// controller, the USB port descriptions and the TPM.
pub struct Acpi {
    pub wwan: bool,
    pub tpm: Option<fstart_driver_tpm::Family>,
}
