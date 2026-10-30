// SPDX-License-Identifier: MIT OR Apache-2.0
//! MJ11 reset image: the PSP trained DRAM and released the BSP here; one
//! stage runs to the payload.

#![no_std]
#![no_main]

use fstart_board_gigabyte_mj11::{self as mj11, CONFIG};
use fstart_driver_amd::soc::zeppelin::Zeppelin;
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use fstart_platform_amd::noncar::Reset;
use fstart_platform_amd::zeppelin::Init;

static CONSOLE: StaticConsole<Ns16550> = StaticConsole::new();

fstart_platform_amd::entry!(main);

fn main() -> ! {
    let reset = Reset::<Zeppelin>::enter(&CONFIG);
    mj11::early_init(&reset.lpc());
    let stage = reset.console(&CONSOLE, mj11::console());
    let silicon = (|| {
        let init = Init::start(&stage)?;
        let ctx = init.ctx();
        mj11::fch_reset_done(&ctx.gpio(), ctx.fch, ctx.clock);
        let dxio = init.train_dxio()?;
        mj11::dxio_training_done(&ctx.dies[0], &ctx.gpio(), ctx.clock);
        dxio.finish()
    })();
    let mut silicon = silicon.unwrap_or_else(|why| stage.stop(why));
    let pci = stage.enumerate(&mut silicon);
    let ctx = stage.ctx();
    mj11::mainboard_final(&ctx.dies[0], &ctx.gpio(), ctx.fch, ctx.clock);
    stage.ready_to_boot(&mut silicon, &pci);
    mj11::ready_to_boot(&ctx.gpio(), &ctx.fch());
    stage.boot(silicon, pci, mj11::acpi())
}
