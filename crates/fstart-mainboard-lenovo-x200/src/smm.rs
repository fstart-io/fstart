//! Lenovo ThinkPad X200 mainboard SMM policy.

use fstart_smm_runtime::{SmmBoardHandler, SmmContext};

use crate::dock;

/// ACPI/SMM command byte for dock connect, shared with generated ACPI.
pub const SMI_DOCK_CONNECT: u8 = 0x01;
/// ACPI/SMM command byte for dock disconnect, shared with generated ACPI.
pub const SMI_DOCK_DISCONNECT: u8 = 0x02;

/// Board-specific X200 SMM handler.
pub struct LenovoX200SmmHandler;

impl SmmBoardHandler for LenovoX200SmmHandler {
    unsafe fn on_tco_command(_ctx: &mut SmmContext<'_>, command: u8) -> Option<u8> {
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
