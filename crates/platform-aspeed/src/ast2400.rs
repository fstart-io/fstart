//! AST2400 and AST1250 stages: the bootblock, which the ARM926EJ-S
//! executes from SPI flash in place from the reset vector, and the main
//! stage it loads into DRAM.
//!
//! The bootblock boots on DRAM that is already up (SCU40\[6\]), as QEMU
//! and a warm reboot leave it; native DDR2/DDR3 init is still to come.

use crate::facts::AspeedSoc;
use crate::layout::AST2400 as LAYOUT;
use crate::stage::{self, SCU_UNLOCK_KEY, Soc};
use aspeed_hal::ast2400::fmc;
use aspeed_pac as pac;
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_log::{Hex, StaticConsole};
use fstart_timestamp::id;

const SOC: Soc = Soc {
    name: "ast2400",
    layout: &LAYOUT,
};

/// Stop what fires without us, protect the flash, bring up the console,
/// stop the AST2400's coprocessor, switch CE0 to the fastest calibrated
/// read up to `flash_max_hz`, take over the DRAM that is up, test it if
/// `memtest`, then load the main stage into DRAM and enter it. `soc` is the board's AST2400 or
/// AST1250, which the SoC cannot tell itself. With `timestamps`, the
/// stages time their milestones and report them at the payload jump.
///
/// The strap may arm WDT2 to switch to the alternate boot flash, so it is
/// stopped first. CE0 is made read-only before any access could turn into
/// a flash page program.
pub fn bootblock(
    slot: &'static StaticConsole<Ns16550>,
    console: Ns16550Config,
    soc: AspeedSoc,
    flash_max_hz: u32,
    memtest: bool,
    timestamps: bool,
) -> ! {
    fstart_stage::anchor::initial_stage();
    if timestamps {
        stage::start_timestamps(&SOC);
    }
    pac::WDT2.CTRL().modify(|w| w.set_WDT_EN(false));
    pac::FMC
        .CE_TYPE()
        .modify(|w| w.set_CE0_DEFAULT_WRITE(false));
    pac::SCU.PROTECT().write(|w| w.set_KEY(SCU_UNLOCK_KEY));

    stage::start_console(slot, console);
    fstart_timestamp::add(id::CONSOLE_READY);
    fstart_log::info!(
        "ast2400 bootblock: entered from SPI ({})",
        match soc {
            AspeedSoc::Ast2400 => "AST2400",
            AspeedSoc::Ast1250 => "AST1250",
            _ => stage::fail(&SOC, "the board's SoC is not of the AST2400 generation"),
        }
    );
    if soc == AspeedSoc::Ast2400 {
        stop_coprocessor();
    }

    let strap = pac::SCU.HWSTRAP().read();
    fstart_log::info!(
        "SCU7C silicon {}",
        Hex(pac::SCU.SILICON_REV().read().0.into())
    );
    fstart_log::info!(
        "SCU70 strap {} ({})",
        Hex(strap.0.into()),
        if strap.DRAM_CONFIG() & 1 != 0 {
            "DDR2"
        } else {
            "DDR3"
        }
    );
    fstart_log::info!(
        "SCU40 handshake {}",
        Hex(pac::SCU.VGA_HANDSHAKE0().read().0.into())
    );

    fstart_timestamp::add(id::EARLY_INIT);
    let (area, len) = stage::flash_calibration_area();
    stage::log_flash_read(fmc::fast_read(flash_max_hz, area, len));

    fstart_timestamp::add(id::INITRAM_START);
    let usable = initialised_dram(soc);
    fstart_timestamp::add(id::INITRAM_END);
    if memtest {
        fstart_timestamp::add(id::RAMINIT_MEMORY_TEST);
        stage::memtest(&SOC, usable as u32);
    }
    stage::enter_main_stage(&SOC, usable)
}

/// The DRAM main stage: take over the bootblock's locator and DRAM size,
/// mount and verify the FFS, then boot the build's payload. The board's
/// main stage `main` calls this with the handoff address and the same
/// `timestamps` choice as its bootblock.
pub fn main_stage(
    slot: &'static StaticConsole<Ns16550>,
    console: Ns16550Config,
    handoff_addr: usize,
    timestamps: bool,
) -> ! {
    stage::main_stage(&SOC, slot, console, handoff_addr, timestamps)
}

/// The usable bytes of DRAM that is already initialised: the capacity the
/// controller was set up for, less the graphics carve-out at its top on an
/// AST2400.
fn initialised_dram(soc: AspeedSoc) -> u64 {
    if !pac::SCU.VGA_HANDSHAKE0().read().DRAM_INIT_READY() {
        stage::fail(
            &SOC,
            "DRAM is not up (SCU40[6]); native DRAM init is to come",
        );
    }
    let config = pac::SDMC.CONFIG().read();
    if config.ECC_EN() {
        stage::fail(&SOC, "DRAM with ECC is not supported yet");
    }
    let capacity = (64u64 << 20) << config.CAPACITY();
    let vga = if soc.has_host_graphics() {
        (8u64 << 20) << config.VGA_APERTURE()
    } else {
        0
    };
    let usable = capacity - vga;
    fstart_log::info!(
        "dram: already initialised (SCU40), {} MiB, VGA {} MiB, usable {} MiB at {}",
        capacity >> 20,
        vga >> 20,
        usable >> 20,
        Hex(LAYOUT.dram_base)
    );
    usable
}

/// Hold the ColdFire coprocessor in reset. It is off after power-on, but
/// a reset of the ARM alone (a watchdog in ARM-only mode) leaves it
/// running, possibly from DRAM the stages overwrite. Loading its firmware
/// is the payload's job.
fn stop_coprocessor() {
    let ctrl = pac::COPROC.CTRL();
    if ctrl.read().EN() {
        fstart_log::info!("coprocessor: running, stopped");
    }
    ctrl.write(|w| w.set_RESET(true));
}
