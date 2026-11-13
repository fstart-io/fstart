//! Lenovo ThinkPad X201 mainboard work the stage `main`s call: the EHCI
//! debug console, the clock generator, H8/PMH7 policy, the dock
//! connection, the identity EEPROM and the X201T digitizer.

use crate::ec as x201;
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_intel::southbridge::gpio_ich::{IchGpio, IchPin};

/// The EHCI debug port of EHCI2 (00:1a.0): coreboot's X201
/// `USBDEBUG_HCD_INDEX=2`.
pub const fn console() -> fstart_driver_usbdebug::Config {
    crate::config::EHCI_CONSOLE
}

/// coreboot `bootblock_mainboard_early_init()`: the EC powers the USB
/// ports before the debug console needs one.
pub fn early_usb_power() {
    // No console yet to report a failure; the console init reports a dead
    // port instead.
    let _ = x201::early_usb_power(&crate::config::X201_H8);
}

/// coreboot `set_fsb_frequency()` from `mainboard_pre_raminit()`: the
/// clock generator's first two registers hold the FSB word. Like coreboot,
/// the first five registers are written back.
pub fn set_fsb_frequency(smbus: &mut impl SmBus) -> Result<(), ServiceError> {
    let mut block = [0u8; 32];
    let count = smbus.block_read(crate::config::CLOCKGEN_ADDRESS, 0, &mut block)?;
    if count < 5 {
        return Err(ServiceError::HardwareError);
    }
    block[..2].copy_from_slice(&crate::config::CLOCKGEN_FSB_FREQUENCY.to_le_bytes());
    smbus.block_write(crate::config::CLOCKGEN_ADDRESS, 0, &block[..5])
}

/// The PCH GPIOs the board's own devices hang off.
pub struct Pins {
    /// Bluetooth daughter card fitted (active low).
    pub bluetooth_card: IchPin,
    /// Dock ID straps GPIO3..5; 7 means no dock.
    pub dock_id: [IchPin; 3],
    /// Connects a present dock.
    pub dock_connect: IchPin,
}

impl Pins {
    pub const fn of(gpio: IchGpio) -> Self {
        let [a, b, c] = x201::DOCK_ID_GPIOS;
        Self {
            bluetooth_card: gpio.pin(x201::BDC_PRESENCE_GPIO),
            dock_id: [gpio.pin(a), gpio.pin(b), gpio.pin(c)],
            dock_connect: gpio.pin(x201::DOCK_CONNECT_GPIO),
        }
    }
}

/// What the board read from its own hardware for the tables.
#[derive(Default)]
pub struct Identity {
    pub eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    pub ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

/// The EC options this ThinkPad has, as its coreboot `cmos.layout` lists
/// them.
pub static EC_KNOBS: [fstart_options::Knob<fstart_driver_lenovo::options::Options>; 12] = {
    use fstart_driver_lenovo::options::*;
    [
        FIRST_BATTERY,
        BLUETOOTH,
        WWAN,
        TOUCHPAD,
        WLAN,
        TRACKPOINT,
        FN_CTRL_SWAP,
        STICKY_FN,
        POWER_MANAGEMENT_BEEPS,
        LOW_BATTERY_BEEP,
        USB_ALWAYS_ON,
        VOLUME,
    ]
};

/// EC/PMH7 with the user's `ec` options, the dock, the identity EEPROM and
/// the digitizer, once the chipset devices are up. Restores the optional
/// TPM on S3 before returning the identity and the board ACPI device.
pub fn init_devices(
    pins: &mut Pins,
    smbus: &mut impl SmBus,
    ec: &fstart_driver_lenovo::options::Options,
    resume: bool,
) -> Result<(Identity, Acpi), fstart_driver_tpm::Error> {
    let mut identity = Identity::default();
    let (bluetooth, acknowledged) = x201::ec_init(
        &mut pins.bluetooth_card,
        &mut pins.dock_id,
        &mut pins.dock_connect,
        &crate::config::X201_H8,
        ec,
        resume,
    );
    if !acknowledged {
        fstart_log::error!("lenovo-x201: H8 EC initialization incomplete");
    }
    if !resume {
        // The AT24RF08C shares the SPD bus; read-only identity access never
        // touches its data or protection registers.
        identity.eeprom = fstart_driver_lenovo::eeprom::Identity::read(smbus)
            .inspect_err(|error| {
                fstart_log::error!(
                    "lenovo-x201: EEPROM identity unavailable, code {}",
                    *error as u8
                )
            })
            .ok();
        identity.ec_oem_string = fstart_driver_lenovo::h8::H8::new(
            fstart_driver_lenovo::ec::Ec::new(crate::config::X201_H8.resources.os),
        )
        .smbios_oem_string();
        if identity.ec_oem_string.is_none() {
            fstart_log::error!("lenovo-x201: H8 EC firmware id unavailable");
        }
    }
    // The digitizer's SuperIO is programmed on every boot; whether it is a
    // tablet only matters for the tables a cold boot builds.
    let digitizer = fstart_driver_lenovo::thinkpad::digitizer_init(
        identity.eeprom.as_ref().map(|e| e.part_number()),
    );
    Ok((
        identity,
        Acpi {
            bluetooth,
            digitizer,
            tpm: init_tpm(resume)?,
        },
    ))
}

/// Identify the optional LPC TIS TPM and restore its saved state on S3.
/// Cold boots leave startup to the payload, as before.
fn init_tpm(resume: bool) -> Result<Option<fstart_driver_tpm::Family>, fstart_driver_tpm::Error> {
    use fstart_driver_tpm::tis;
    // SAFETY: the PCH decodes the TIS window to LPC; nothing else drives it.
    let mut tis = unsafe { tis::Tis::new(tis::BASE, fstart_arch::x86::Udelay) };
    let Some(id) = tis.probe() else {
        return Ok(None);
    };
    fstart_log::info!(
        "lenovo-x201: TPM {:#x}:{:#x} revision {}",
        id.vendor,
        id.device,
        id.revision
    );
    if resume {
        fstart_driver_tpm::resume(&mut tis, id.family)?;
        fstart_log::info!("lenovo-x201: TPM S3 state restored");
    }
    Ok(Some(id.family))
}

/// Route EC events to the installed SMM handler until OSPM enables ACPI.
/// Call only on cold boot: S3 must retain the OS's SCI ownership.
pub fn legacy_ec_events() {
    use fstart_driver_intel::southbridge::smi::{ICH8_GPE0, IchSmi};
    let smi = IchSmi::new(fstart_platform_intel::ironlake::IBEXPEAK_PMBASE, ICH8_GPE0);
    // SAFETY: the BSP owns PCI config after MP/SMM installation completes.
    unsafe { smi.route_gpi(x201::EC_SCI_GPI, false) };
}

/// The board's ACPI: H8 EC surface, sleep hooks, ThinkLight, dock, the
/// keyboard controller, the X201T digitizer and the TPM.
pub struct Acpi {
    pub bluetooth: bool,
    pub digitizer: bool,
    pub tpm: Option<fstart_driver_tpm::Family>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A clock generator answering with `count` registers.
    struct Clockgen {
        registers: [u8; 8],
        count: usize,
        written: std::vec::Vec<u8>,
    }

    impl SmBus for Clockgen {
        fn read_byte(&mut self, _: u8, _: u8) -> Result<u8, ServiceError> {
            Err(ServiceError::NotSupported)
        }
        fn write_byte(&mut self, _: u8, _: u8, _: u8) -> Result<(), ServiceError> {
            Err(ServiceError::NotSupported)
        }
        fn block_read(&mut self, _: u8, _: u8, data: &mut [u8]) -> Result<usize, ServiceError> {
            data[..self.count].copy_from_slice(&self.registers[..self.count]);
            Ok(self.count)
        }
        fn block_write(&mut self, _: u8, _: u8, data: &[u8]) -> Result<(), ServiceError> {
            self.written = data.to_vec();
            Ok(())
        }
    }

    #[test]
    fn fsb_word_replaces_the_first_two_of_five_registers() {
        let mut clockgen = Clockgen {
            registers: [1, 2, 3, 4, 5, 6, 7, 8],
            count: 8,
            written: std::vec::Vec::new(),
        };
        set_fsb_frequency(&mut clockgen).unwrap();
        assert_eq!(clockgen.written, [0x9f, 0xf5, 3, 4, 5]);

        clockgen.count = 4;
        assert_eq!(
            set_fsb_frequency(&mut clockgen),
            Err(ServiceError::HardwareError)
        );
    }
}
