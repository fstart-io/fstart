//! Static fixed-flow adapter for QEMU Q35.

use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::device::DeviceError;
use fstart_services::{Device, HardwareInit, InitContext, ServiceError};
use fstart_stage_runtime::StaticBoard;

pub struct StageDevices {
    uart0: Option<Ns16550>,
}

impl StageDevices {
    fn new() -> Self {
        Self { uart0: None }
    }

    fn ensure_uart0(&mut self) -> Result<&mut Ns16550, ServiceError> {
        static UART0_CONFIG: Ns16550Config = Ns16550Config {
            regs: AccessMode::Pio { base: 0x3f8 },
            clock_freq: 1_843_200,
            baud_rate: 115_200,
        };

        if self.uart0.is_none() {
            let uart = Ns16550::new(&UART0_CONFIG).map_err(device_error_to_service_error)?;
            self.uart0 = Some(uart);
        }

        Ok(self.uart0.as_mut().expect("uart0 constructed"))
    }
}

impl HardwareInit for StageDevices {
    fn console(&mut self, ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let uart = self.ensure_uart0()?;
        uart.console(ctx)?;
        // SAFETY: uart0 is stored in StageDevices and fstart_main never returns.
        let uart = unsafe { &*(uart as *const Ns16550) };
        unsafe { fstart_log::init(uart) };
        fstart_log::info!("fstart fixed-flow console ready");
        Ok(())
    }
}

pub struct StageBoard {
    devices: StageDevices,
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new(),
        })
    }

    fn devices_mut(&mut self) -> &mut Self::Devices {
        &mut self.devices
    }

    fn halt() -> ! {
        fstart_platform::halt()
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        fstart_capabilities::console_ready("uart0", "ns16550");
        Ok(())
    }

    fn mount_firmware_volume(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        Ok(())
    }

    fn boot_payload(self) -> ! {
        fstart_log::warn!("qemu-q35 fixed-flow payload handoff is not implemented yet");
        fstart_platform::halt()
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
