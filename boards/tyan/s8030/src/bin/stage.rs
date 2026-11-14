//! The S8030 stage: from the PSP's release of the BSP to the payload.
//! Rome and Milan run the same flow with the SP3 silicon steps.

#![no_std]
#![no_main]

use fstart_board_tyan_s8030::{Board, bmc, dxio, platform};
use fstart_driver_aspeed::gfx;
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::sp3::{Identified, Silicon, Start};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    match fstart_platform_amd::sp3::start::<Board>() {
        Start::RomeMilan(id) => flow(id),
        Start::Naples(id) => id.unsupported(),
    }
}

fn flow<F: Silicon>(id: Identified<Board, F>) -> ! {
    let generation = id.generation();
    let stage = id.init_fch(platform::policy(generation));
    if platform::early_init(stage.fch()).is_err() {
        stage.fail("board early init");
    }
    let mut stage = stage.console(&CONSOLE, platform::CONSOLE).init_fch();
    let levels = stage.read_pads(platform::detect_pads(generation));
    stage.program_gpio(&platform::gpio(generation));
    let detected = platform::detect(&levels, stage.timer());
    let mut options = dxio::Options::BUILD;
    stage.resolve_options(&dxio::KNOBS, &mut options, |o| {
        dxio::check(generation, o, &detected)
    });
    let engines = platform::dxio_engines(generation, &options, &detected);
    let mut stage = stage
        .train_dxio(&engines)
        .init_chipset(())
        .start_cpus()
        .scan_bus();
    // Disabling onboard VGA disables its PCIe link too. Leave the display
    // untouched in that case; the serial console still serves the payload.
    if options.onboard_vga {
        let pci = stage.pci();
        let address = pci
            .devices()
            .find(|&a| {
                pci.device(a).is_some_and(|d| {
                    d.vendor_id() == gfx::ASPEED_VENDOR_ID
                        && d.device_id() == gfx::AST_VGA_DEVICE_ID
                })
            })
            .unwrap_or_else(|| stage.fail("ASPEED VGA device"));
        // SAFETY: BSP exclusively owns the enumerated GPU and its allocated,
        // identity-mapped BARs. The BMC retains ownership of its shared DRAM;
        // this driver neither resets the BMC nor executes a VGA ROM.
        let framebuffer =
            unsafe { gfx::initialize(pci, address, &stage.clock()) }.unwrap_or_else(|error| {
                fstart_log::error!(
                    "s8030: ASPEED display error {}",
                    match error {
                        gfx::Error::Device => "device",
                        gfx::Error::Resources => "resources",
                        gfx::Error::BmcDramNotReady => "BMC DRAM not ready",
                    }
                );
                stage.fail("ASPEED display")
            });
        stage.set_framebuffer(framebuffer);
    }
    stage
        .ready_to_boot()
        .emit_tables(&platform::smbios_identity(), bmc::acpi_board())
        .boot()
}
