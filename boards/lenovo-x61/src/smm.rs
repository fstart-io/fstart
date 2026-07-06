//! Lenovo ThinkPad X61 mainboard SMM policy.

use fstart_driver_intel_ich8::smm::Ich8SmmHandler;
use fstart_smm_runtime::{SmmBoardHandler, SmmContext, SMM_PLATFORM_INTEL_ICH};

use crate::{mainboard::dock, Board};

/// ACPI/SMM command byte for dock connect, shared with generated ACPI.
pub const SMI_DOCK_CONNECT: u8 = 0x01;
/// ACPI/SMM command byte for dock disconnect, shared with generated ACPI.
pub const SMI_DOCK_DISCONNECT: u8 = 0x02;

/// Board-specific X61 SMM handler.
pub struct LenovoX61SmmHandler;

impl SmmBoardHandler for LenovoX61SmmHandler {
    #[allow(unused_unsafe)]
    unsafe fn on_tco_command(_ctx: &mut SmmContext<'_>, command: u8) -> Option<u8> {
        // SAFETY: SMM dispatch runs with the platform I/O decode active.
        unsafe {
            match command {
                SMI_DOCK_CONNECT => Some(if dock::dock_connect().is_ok() { 1 } else { 0 }),
                SMI_DOCK_DISCONNECT => {
                    dock::dock_disconnect();
                    Some(1)
                }
                _ => Some(0),
            }
        }
    }
}

fstart_smm_stage::smm_bin!(
    Board,
    SMM_PLATFORM_INTEL_ICH,
    Ich8SmmHandler<LenovoX61SmmHandler>
);
