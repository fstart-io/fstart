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

#[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
use fstart_services::device::{Device, DeviceError};
use fstart_services::{HardwareInit, InitContext, ServiceError};
use fstart_stage_runtime::StaticBoard;

struct StageDevices {
    #[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
    uart0: Option<fstart_driver_ns16550::Ns16550>,
}

impl StageDevices {
    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            #[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
            uart0: None,
        })
    }

    #[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
    fn ensure_uart0(&mut self) -> Result<&'static fstart_driver_ns16550::Ns16550, ServiceError> {
        static UART0_CONFIG: fstart_driver_ns16550::Ns16550Config =
            fstart_driver_ns16550::Ns16550Config {
                regs: fstart_driver_ns16550::AccessMode::Mmio {
                    base: 0x1000_0000,
                    reg_shift: 0,
                    reg_width: 0,
                },
                clock_freq: 3_686_400,
                baud_rate: 115_200,
            };

        if self.uart0.is_none() {
            let mut uart = fstart_driver_ns16550::Ns16550::new(&UART0_CONFIG)
                .map_err(device_error_to_service_error)?;
            uart.init().map_err(device_error_to_service_error)?;
            self.uart0 = Some(uart);
        }

        // SAFETY: fixed-flow stages never return from fstart_main. The console
        // object is stored inside the static board for the rest of execution.
        let uart = self.uart0.as_ref().expect("uart0 initialized");
        Ok(unsafe { &*(uart as *const fstart_driver_ns16550::Ns16550) })
    }
}

impl HardwareInit for StageDevices {
    fn console(&mut self, _ctx: &mut InitContext<'_>) -> Result<(), ServiceError> {
        #[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
        {
            let uart = self.ensure_uart0()?;
            // SAFETY: uart0 is stored in StageDevices and fstart_main never returns.
            unsafe { fstart_log::init(uart) };
            fstart_log::info!("fstart fixed-flow console ready");
        }
        Ok(())
    }
}

struct StageBoard {
    devices: StageDevices,
}

impl StaticBoard for StageBoard {
    type Devices = StageDevices;

    fn new() -> Result<Self, ServiceError> {
        Ok(Self {
            devices: StageDevices::new()?,
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

/// Stage entry point. Called by the platform's `_start` after register setup,
/// BSS clearing, and stack pointer initialization.
#[no_mangle]
pub extern "Rust" fn fstart_main(_handoff_ptr: usize) -> ! {
    fstart_stage_runtime::run_fixed_flow::<StageBoard>()
}

#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.keep")]
static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;

#[cfg(all(fstart_board = "qemu-riscv64", feature = "ns16550"))]
fn device_error_to_service_error(_err: DeviceError) -> ServiceError {
    ServiceError::HardwareError
}
