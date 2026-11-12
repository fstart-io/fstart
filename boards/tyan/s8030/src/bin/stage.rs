//! The S8030 stage: from the PSP's release of the BSP to the payload.
//! Rome and Milan run the same flow with the SP3 silicon steps.

#![no_std]
#![no_main]

use fstart_board_tyan_s8030::{Board, bmc, dxio, platform};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::sp3::{Identified, Silicon, Start};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let Start::RomeMilan(id) = fstart_platform_amd::sp3::start::<Board>();
    flow(id)
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
    let lpc_aml = bmc::lpc_aml();
    stage
        .train_dxio(&engines)
        .init_chipset(())
        .start_cpus()
        .scan_bus()
        .ready_to_boot()
        .emit_tables(&platform::smbios_identity(), bmc::acpi_board(&lpc_aml))
        .boot()
}
