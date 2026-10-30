//! The S8030 reset image: from the PSP's release of the BSP to the payload,
//! with the running CPU's generation's silicon steps.

#![no_std]
#![no_main]

use fstart_board_tyan_s8030::{Board, platform};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_sp3::{Flow, Reset, Silicon};

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_sp3::entry!(main);

fn main() -> ! {
    fstart_platform_sp3::start::<Board, BoardFlow>()
}

struct BoardFlow;

impl Flow<Board> for BoardFlow {
    fn run<S: Silicon>(reset: Reset<Board, S>) -> ! {
        if platform::early_init(reset.fch()).is_err() {
            reset.fail("board early init");
        }
        let stage = reset.console(&CONSOLE);
        let engines = match platform::dxio_engines(
            stage.generation(),
            stage.pad_levels(),
            stage.fch(),
            stage.timer(),
        ) {
            Ok(engines) => engines,
            Err(_) => stage.fail("board DXIO devices"),
        };
        stage.finish(&engines)
    }
}
