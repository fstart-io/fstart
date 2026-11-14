//! The Ethanol-X stage: from the PSP's release of the BSP to the
//! payload, on Milan with the SP3 silicon steps.

#![no_std]
#![no_main]

use fstart_board_amd_ethanolx::{self as ethanolx, Board};
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
    let stage = id.init_fch(ethanolx::policy(generation));
    if ethanolx::early_init(stage.fch()).is_err() {
        stage.fail("board early init");
    }
    let mut stage = stage.console(&CONSOLE, ethanolx::CONSOLE).init_fch();
    stage.program_gpio(&ethanolx::GPIO_PROGRAM);
    let mut layout = ethanolx::dxio_layout(stage.fch(), stage.timer())
        .unwrap_or_else(|_| stage.fail("board DXIO devices"));
    stage.resolve_options(&ethanolx::dxio::KNOBS, &mut layout, ethanolx::dxio::check);
    let engines = ethanolx::dxio_engines(&layout, stage.timer())
        .unwrap_or_else(|_| stage.fail("board DXIO devices"));
    let mut stage = stage
        .train_dxio(&engines)
        .init_chipset(())
        .start_cpus()
        .scan_bus();
    // This board's BMC owns the AST2500 and its initialized shared DRAM.
    // Use its native host display driver; never execute a VGA ROM or reset it.
    let pci = stage.pci();
    let address = pci
        .devices()
        .find(|&a| {
            pci.device(a).is_some_and(|d| {
                d.vendor_id() == gfx::ASPEED_VENDOR_ID && d.device_id() == gfx::AST_VGA_DEVICE_ID
            })
        })
        .unwrap_or_else(|| stage.fail("ASPEED VGA device"));
    // SAFETY: BSP owns this board's enumerated GPU and its identity-mapped
    // allocated BARs; no payload display driver is running yet.
    let framebuffer =
        unsafe { gfx::initialize(pci, address, &stage.clock()) }.unwrap_or_else(|error| {
            use fstart_driver_aspeed::gfx::Error;
            fstart_log::error!(
                "ethanolx: ASPEED display error {}",
                match error {
                    Error::Device => "device",
                    Error::Resources => "resources",
                    Error::BmcDramNotReady => "BMC DRAM not ready",
                }
            );
            stage.fail("ASPEED display")
        });
    stage.set_framebuffer(framebuffer);
    stage
        .ready_to_boot()
        .emit_tables(&ethanolx::smbios_identity(), ethanolx::acpi_board())
        .boot()
}
