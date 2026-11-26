//! GIGABYTE MD25-CG0 stage: from the PSP's release of the BSP to the payload,
//! on AMD Venice (SP7).

#![no_std]
#![no_main]

use fstart_board_gigabyte_md25_cg0::{self as md25, Board};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::flow::Cpu;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let id = match fstart_platform_amd::flow::start::<Board>() {
        Cpu::Venice(id) => id,
        cpu => cpu.unsupported(),
    };
    let mut stage = id.enter(md25::config());
    let kcs_decoded = md25::early_init(&stage.hw().fch);
    stage.config_mut().kcs_decoded = kcs_decoded;
    stage
        .console(&CONSOLE, md25::CONSOLE)
        .init_fch()
        .train_dxio(md25::wiring::topology(true))
        .init_chipset(())
        .start_cpus()
        .scan_bus()
        .ready_to_boot()
        .emit_tables(&md25::smbios_identity())
        .boot()
}
