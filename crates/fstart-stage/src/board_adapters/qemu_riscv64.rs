//! Static fixed-flow adapter for the QEMU RISC-V `virt` board.

use fstart_driver_ns16550::{AccessMode, Ns16550, Ns16550Config};
use fstart_services::device::{Device, DeviceError};
use fstart_services::{HardwareInit, InitContext, ServiceError};
use fstart_stage_runtime::StaticBoard;

pub struct StageDevices {
    uart0: Option<Ns16550>,
}

impl StageDevices {
    fn new() -> Self {
        Self { uart0: None }
    }

    fn ensure_uart0(&mut self) -> Result<&'static Ns16550, ServiceError> {
        static UART0_CONFIG: Ns16550Config = Ns16550Config {
            regs: AccessMode::Mmio {
                base: 0x1000_0000,
                reg_shift: 0,
                reg_width: 0,
            },
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        };

        if self.uart0.is_none() {
            let mut uart = Ns16550::new(&UART0_CONFIG).map_err(device_error_to_service_error)?;
            uart.init().map_err(device_error_to_service_error)?;
            self.uart0 = Some(uart);
        }

        // SAFETY: fixed-flow stages never return from fstart_main. The console
        // object is stored inside the static board for the rest of execution.
        let uart = self.uart0.as_ref().expect("uart0 initialized");
        Ok(unsafe { &*(uart as *const Ns16550) })
    }
}

impl HardwareInit for StageDevices {
    fn console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        let uart = self.ensure_uart0()?;
        // SAFETY: uart0 is stored in StageDevices and fstart_main never returns.
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
        loop {
            core::hint::spin_loop();
        }
    }

    fn install_console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(feature = "stage-flow-console-init")]
        {
            fstart_capabilities::console_ready("static", "fixed-flow");
        }
        Ok(())
    }

    fn boot_payload(self) -> ! {
        Self::halt()
    }
}

fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
