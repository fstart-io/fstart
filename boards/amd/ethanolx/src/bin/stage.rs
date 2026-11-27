//! The Ethanol-X stage: from the PSP's release of the BSP to the
//! payload, on Milan.

#![no_std]
#![no_main]

use fstart_board_amd_ethanolx::{self as ethanolx, Board};
use fstart_driver_aspeed::gfx;
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::flow::{Cpu, Identified};
use fstart_platform_amd::sp3::Socket;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    match fstart_platform_amd::flow::start::<Board>() {
        Cpu::Milan(id) => flow(id),
        cpu => cpu.unsupported(),
    }
}

fn flow<S: Socket>(id: Identified<Board, S>) -> ! {
    let generation = id.generation();
    let stage = id.enter(ethanolx::config(generation));
    if ethanolx::early_init(stage.fch()).is_err() {
        stage.fail("board early init");
    }
    let mut stage = stage.console(&CONSOLE, ethanolx::CONSOLE).init_fch();
    stage.program_gpio(&ethanolx::GPIO_PROGRAM);
    let mut layout = ethanolx::dxio_layout(stage.fch(), stage.timer())
        .unwrap_or_else(|_| stage.fail("board DXIO devices"));
    // From what the board detected, not the slots' undetected values.
    stage.resolve_options(
        ethanolx::dxio::SETUP.knobs,
        &mut layout,
        ethanolx::dxio::check,
    );
    let engines = ethanolx::dxio_engines(&layout, stage.timer())
        .unwrap_or_else(|_| stage.fail("board DXIO devices"));
    let mut stage = stage
        .train_dxio(&engines)
        .init_chipset(())
        .start_cpus()
        .scan_bus();
    // This board's BMC owns the AST2500 and its initialized shared DRAM.
    // Use its native host display driver; never execute a VGA ROM or reset it.
    let address = gfx::find_device(stage.pci()).unwrap_or_else(|| stage.fail("ASPEED VGA device"));
    // SAFETY: BSP owns this board's enumerated GPU and its identity-mapped
    // allocated BARs; no payload display driver is running yet.
    let framebuffer = unsafe { gfx::initialize(stage.pci(), address, &stage.clock()) }
        .unwrap_or_else(|error| {
            fstart_log::error!("ethanolx: ASPEED display error ({})", error.name());
            stage.fail("ASPEED display")
        });
    stage.set_framebuffer(framebuffer);
    stage
        .ready_to_boot()
        .emit_tables(&ethanolx::smbios_identity())
        .boot()
}
