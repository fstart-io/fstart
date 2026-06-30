use core::mem::MaybeUninit;

use fstart_board_foxconn_d41s_facts as facts;
#[cfg(feature = "acpi")]
use fstart_driver_i2c_ck505::I2cCk505Config;
use fstart_driver_intel_ich7::{IntelIch7, IntelIch7Config};
use fstart_driver_intel_pineview::{IntelPineview, IntelPineviewConfig};
use fstart_driver_ite8721f::Ite8721f;
use fstart_services::{BusDevice, Console, Device, DeviceError, ServiceError};
use fstart_superio::{LpcBaseProvider, SuperIoConfig};
use fstart_types::BusAddress;

static mut PINEVIEW_CONFIG: MaybeUninit<IntelPineviewConfig> = MaybeUninit::uninit();
static mut ICH7_CONFIG: MaybeUninit<IntelIch7Config> = MaybeUninit::uninit();
static mut SUPERIO_CONFIG: MaybeUninit<SuperIoConfig> = MaybeUninit::uninit();
#[cfg(feature = "acpi")]
static mut CK505_CONFIG: MaybeUninit<I2cCk505Config> = MaybeUninit::uninit();

pub fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}

pub fn pineview_config() -> &'static IntelPineviewConfig {
    unsafe { (*core::ptr::addr_of_mut!(PINEVIEW_CONFIG)).write(facts::pineview_config()) }
}

pub fn ich7_config() -> &'static IntelIch7Config {
    unsafe { (*core::ptr::addr_of_mut!(ICH7_CONFIG)).write(facts::ich7_config()) }
}

pub fn superio_config() -> &'static SuperIoConfig {
    unsafe { (*core::ptr::addr_of_mut!(SUPERIO_CONFIG)).write(facts::superio_config()) }
}

#[cfg(feature = "acpi")]
pub fn ck505_config() -> &'static I2cCk505Config {
    unsafe { (*core::ptr::addr_of_mut!(CK505_CONFIG)).write(facts::ck505_config()) }
}

pub fn new_pineview() -> Result<IntelPineview, ServiceError> {
    IntelPineview::new(pineview_config()).map_err(device_error_to_service_error)
}

pub fn new_ich7() -> Result<IntelIch7, ServiceError> {
    IntelIch7::new(ich7_config()).map_err(device_error_to_service_error)
}

pub fn init_superio(
    config: &'static SuperIoConfig,
    southbridge: &mut IntelIch7,
) -> Result<Ite8721f, ServiceError> {
    let mut superio = Ite8721f::new_on_bus_at(
        config,
        southbridge as &dyn LpcBaseProvider,
        Some(BusAddress::Lpc(0x2e)),
    )
    .map_err(device_error_to_service_error)?;
    superio.init().map_err(device_error_to_service_error)?;
    Ok(superio)
}

pub fn install_superio_console(console: &Ite8721f) -> Result<(), ServiceError> {
    // Make sure the selected SuperIO logical COM port is usable as a console.
    console.write_byte(b'\r')?;
    unsafe { fstart_log::init(console) };
    fstart_stage::fixed_helpers::console_ready(facts::SUPERIO_NODE, "ite8721f/com1");
    Ok(())
}
