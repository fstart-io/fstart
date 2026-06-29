//! fstart-stage: the single firmware stage binary crate.
//!
//! This crate links a fixed handwritten stage executor. Board/device
//! participation is selected by Cargo features and Rust board metadata at build
//! time; no Rust stage source is generated.
//!
//! To build for a specific board:
//!   FSTART_RUST_BOARD=qemu-riscv64 \
//!     cargo build -p fstart-stage --target riscv64gc-unknown-none-elf \
//!     --features riscv64,ns16550 -Z build-std=core

#![no_std]
#![no_main]

use core::panic::PanicInfo;

// When a feature requiring heap allocation is active, pull in fstart-alloc
// to register the global allocator.  Without this explicit extern crate,
// the linker would not include it (nothing else references the crate by
// symbol).
#[cfg(any(
    feature = "acpi",
    feature = "pci-ecam",
    feature = "q35-hostbridge",
    feature = "crabefi"
))]
extern crate fstart_alloc;

use fstart_services::{HardwareInit, InitContext, ServiceError};
use fstart_stage_runtime::StaticBoard;

struct StageDevices;

impl HardwareInit for StageDevices {}

struct StageBoard {
    devices: StageDevices,
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices,
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
        fstart_capabilities::console_ready("static", "fixed-flow");
        Ok(())
    }

    fn boot_payload(self) -> ! {
        Self::halt()
    }
}

/// Stage entry point. Called by the platform's `_start` after register setup,
/// BSS clearing, and stack pointer initialization.
#[no_mangle]
pub extern "Rust" fn fstart_main(_handoff_ptr: usize) -> ! {
    fstart_stage_runtime::run_fixed_flow::<StageBoard>()
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    StageBoard::halt()
}
