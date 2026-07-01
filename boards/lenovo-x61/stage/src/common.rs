use fstart_board_lenovo_x61 as board;
use fstart_board_lenovo_x61::{x61_mainboard_config, LenovoX61Mainboard};
use fstart_driver_intel_gm965::IntelGm965;
use fstart_driver_intel_ich8::IntelIch8;
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_services::{Device, DeviceError, ServiceError};

pub const UART0_CONFIG: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio {
        base: board::UART0_PIO_BASE as u64,
    },
    clock_freq: board::UART0_CLOCK_FREQ,
    baud_rate: board::UART0_BAUD_RATE,
};

pub fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}

pub fn new_gm965() -> Result<IntelGm965, ServiceError> {
    IntelGm965::new(board::gm965_ich8_config().gm965).map_err(device_error_to_service_error)
}

pub fn new_ich8() -> Result<IntelIch8, ServiceError> {
    IntelIch8::new(board::gm965_ich8_config().ich8).map_err(device_error_to_service_error)
}

pub fn new_mainboard() -> Result<LenovoX61Mainboard, ServiceError> {
    LenovoX61Mainboard::new(x61_mainboard_config()).map_err(device_error_to_service_error)
}
