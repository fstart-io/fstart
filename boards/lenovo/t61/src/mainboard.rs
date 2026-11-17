//! T61 EC, Mini Dock and onboard Ultrabay integration.

use embedded_hal::digital::{InputPin, OutputPin};
use fstart_core::services::{ServiceError, SmBus};
use fstart_driver_intel::southbridge::gpio_ich::{IchGpio, IchPin};
use fstart_driver_lenovo::{
    ec::Ec,
    h8::H8,
    t6::{self, dock},
};
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

pub struct Pins {
    pub bluetooth_card: IchPin,
    pub smbus_mux: IchPin,
}
impl Pins {
    pub const fn of(gpio: IchGpio) -> Self {
        Self {
            bluetooth_card: gpio.pin(t6::BDC_PRESENCE_GPIO),
            smbus_mux: gpio.pin(crate::SMBUS_MUX.pin),
        }
    }
}

pub fn setup_dock_console() {
    if dock::setup_dock_console(&crate::T61_H8).is_err() {
        fstart_log::error!("T61: Mini Dock initialization failed; console may be absent");
    }
}

fn os_ec() -> Ec {
    Ec::new(crate::T61_H8.resources.os)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SmbusBranch {
    SpdAndClock,
    Eeprom,
}

pub fn select_smbus(mux: &mut impl OutputPin, branch: SmbusBranch) -> Result<(), ServiceError> {
    let result = match branch {
        SmbusBranch::SpdAndClock => mux.set_high(),
        SmbusBranch::Eeprom => mux.set_low(),
    };
    result.map_err(|_| ServiceError::IoError)
}

pub fn init_ck505(bus: &mut impl SmBus) -> Result<(), ServiceError> {
    use fstart_driver_intel::generic::ck505::I2cCk505;
    I2cCk505::new_at_address(crate::ck505_config(), 0x69)?
        .init_on_smbus(bus)
        .map_err(|_| ServiceError::HardwareError)
}

/// Unlike the X61, the T61's PATA bay is inside the laptop, not in its dock.
pub fn ultrabay_populated() -> bool {
    H8::new(os_ec()).ultrabay_device_present().unwrap_or(false)
}

pub static EC_KNOBS: [fstart_options::Knob<fstart_driver_lenovo::options::Options>; 10] = {
    use fstart_driver_lenovo::options::*;
    [
        TOUCHPAD,
        TRACKPOINT,
        BLUETOOTH,
        WWAN,
        WLAN,
        STICKY_FN,
        POWER_MANAGEMENT_BEEPS,
        LOW_BATTERY_BEEP,
        VOLUME,
        FIRST_BATTERY,
    ]
};
pub static CPU_KNOBS: [fstart_options::Knob<fstart_driver_intel::cpu::options::Options>; 1] =
    [fstart_driver_intel::cpu::options::UNDERVOLT];

#[derive(Default)]
pub struct Identity {
    pub eeprom: Option<fstart_driver_lenovo::eeprom::Identity>,
    pub ec_oem_string: Option<fstart_driver_lenovo::h8::EcOemString>,
}

pub fn init_devices(
    bdc: &mut impl InputPin,
    smbus: &mut impl SmBus,
    options: &fstart_driver_lenovo::options::Options,
    resume: bool,
) -> Identity {
    program_subsystem_ids();
    if !t6::ec_init(t6::Model::T61, bdc, &crate::T61_H8, options, resume) {
        fstart_log::error!("T61: H8 initialization incomplete");
    }
    if !dock::mainstage_power_policy(os_ec(), &crate::T61_H8, false) {
        fstart_log::error!("T61: Ultrabay power/LED setup incomplete");
    }
    let mut identity = Identity::default();
    if !resume {
        identity.eeprom = fstart_driver_lenovo::eeprom::Identity::read(smbus).ok();
        identity.ec_oem_string = H8::new(os_ec()).smbios_oem_string();
    }
    // The R5C822 is function 2 behind the enumerated ICH8 conventional bridge.
    use tock_registers::interfaces::Readable;
    let bridge = fstart_pci::EcamDevice::new(0, 0x1e, 0);
    // SAFETY: ECAM is live and this is the fixed ICH8 Type 1 bridge.
    let bus = unsafe { bridge.regs::<fstart_pci::PciType1Config>() }
        .secondary_bus
        .get();
    if bus != 0
        && bus != 0xff
        && fstart_driver_ricoh::r5c822::configure_sd_write_protect(
            fstart_pci::EcamDevice::new(bus, 0, 2),
            true,
        )
        .is_err()
    {
        fstart_log::error!("T61: SD write-protect setup failed");
    }
    identity
}

/// Board identity belongs here; the PCI helper handles Type 0 versus SSVID
/// capability registers on bridges. Do not touch absent/disabled functions.
fn program_subsystem_ids() {
    for (device, function, subsystem) in [
        (0x00, 0, 0x20b1),
        (0x01, 0, 0x20b2),
        (0x19, 0, 0x20b9),
        (0x1a, 0, 0x20aa),
        (0x1a, 1, 0x20aa),
        (0x1a, 7, 0x20ab),
        (0x1b, 0, 0x20ac),
        (0x1c, 0, 0x20ad),
        (0x1c, 1, 0x20ad),
        (0x1c, 2, 0x20ad),
        (0x1c, 3, 0x20ad),
        (0x1c, 4, 0x20ad),
        (0x1d, 0, 0x20aa),
        (0x1d, 1, 0x20aa),
        (0x1d, 2, 0x20aa),
        (0x1d, 7, 0x20ab),
        (0x1e, 0, 0x20ae),
        (0x1f, 1, 0x20a6),
        (0x1f, 2, 0x20a7),
        (0x1f, 3, 0x20a9),
    ] {
        let pci = fstart_pci::EcamDevice::new(0, device, function);
        if pci.is_present() {
            let _ = pci.set_subsystem((subsystem << 16) | 0x17aa);
        }
    }
}

pub fn handoff_ec_events(resume: bool) {
    use fstart_driver_intel::southbridge::smi::{
        APM_CNT, APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE,
    };
    // SAFETY: permanent SMM is installed and owns this port.
    unsafe {
        fstart_core::pio::outb(
            APM_CNT,
            if resume {
                APM_CNT_ACPI_ENABLE
            } else {
                APM_CNT_ACPI_DISABLE
            },
        )
    };
}

pub struct Acpi;
