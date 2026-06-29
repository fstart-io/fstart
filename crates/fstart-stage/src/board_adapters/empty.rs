//! Empty static adapter for boards whose fixed-flow device container has not yet
//! been moved into `fstart-stage`.

use fstart_services::{HardwareInit, InitContext, ServiceError};
use fstart_stage_runtime::StaticBoard;

pub struct StageDevices;

impl StageDevices {
    fn new() -> Self {
        Self
    }
}

impl HardwareInit for StageDevices {}

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
        Ok(())
    }

    fn boot_payload(self) -> ! {
        Self::halt()
    }
}
