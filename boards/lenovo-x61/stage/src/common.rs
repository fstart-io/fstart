use core::mem::MaybeUninit;

use fstart_board_lenovo_x61_facts as facts;
use fstart_driver_intel_gm965::{IntelGm965, IntelGm965Config};
use fstart_driver_intel_ich8::{IntelIch8, IntelIch8Config};
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_mainboard_lenovo_x61::{LenovoX61Mainboard, LenovoX61MainboardConfig};
use fstart_services::{Device, DeviceError, ServiceError};

static mut GM965_CONFIG: MaybeUninit<IntelGm965Config> = MaybeUninit::uninit();
static mut ICH8_CONFIG: MaybeUninit<IntelIch8Config> = MaybeUninit::uninit();
static mut MAINBOARD_CONFIG: MaybeUninit<LenovoX61MainboardConfig> = MaybeUninit::uninit();

pub static UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio {
        base: facts::UART0_PIO_BASE as u64,
    },
    clock_freq: facts::UART0_CLOCK_FREQ,
    baud_rate: facts::UART0_BAUD_RATE,
};

pub fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}

pub fn gm965_config() -> &'static IntelGm965Config {
    unsafe { (*core::ptr::addr_of_mut!(GM965_CONFIG)).write(facts::gm965_config()) }
}

pub fn ich8_config() -> &'static IntelIch8Config {
    unsafe { (*core::ptr::addr_of_mut!(ICH8_CONFIG)).write(facts::ich8_config()) }
}

pub fn mainboard_config() -> &'static LenovoX61MainboardConfig {
    unsafe { (*core::ptr::addr_of_mut!(MAINBOARD_CONFIG)).write(facts::x61_mainboard_config()) }
}

pub fn new_gm965() -> Result<IntelGm965, ServiceError> {
    IntelGm965::new(gm965_config()).map_err(device_error_to_service_error)
}

pub fn new_ich8() -> Result<IntelIch8, ServiceError> {
    IntelIch8::new(ich8_config()).map_err(device_error_to_service_error)
}

pub fn new_mainboard() -> Result<LenovoX61Mainboard, ServiceError> {
    LenovoX61Mainboard::new(mainboard_config()).map_err(device_error_to_service_error)
}
