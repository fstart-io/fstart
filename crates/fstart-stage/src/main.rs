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

#[cfg(feature = "aarch64")]
extern crate fstart_platform_aarch64 as fstart_platform;
#[cfg(feature = "armv7")]
extern crate fstart_platform_armv7 as fstart_platform;
#[cfg(feature = "riscv64")]
extern crate fstart_platform_riscv64 as fstart_platform;
#[cfg(feature = "x86_64")]
extern crate fstart_platform_x86_64 as fstart_platform;

extern crate fstart_runtime;

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
        {
            raw_boot_banner();
            fstart_capabilities::console_ready("static", "fixed-flow");
        }
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

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;

#[cfg(all(feature = "riscv64", feature = "stage-flow-console-init"))]
fn raw_boot_banner() {
    const UART0: *mut u8 = 0x1000_0000 as *mut u8;
    for byte in b"fstart fixed-flow\r\n" {
        // SAFETY: QEMU virt exposes an NS16550-compatible UART at 0x1000_0000.
        unsafe { core::ptr::write_volatile(UART0, *byte) };
    }
}

#[cfg(not(all(feature = "riscv64", feature = "stage-flow-console-init")))]
fn raw_boot_banner() {}
