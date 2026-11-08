//! AST2500 bootblock: the ARM1176 executes SPI flash in place from the reset
//! vector.

use aspeed_pac as pac;
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_log::{Hex, StaticConsole};

/// SCU00 protection key.
const SCU_UNLOCK_KEY: u32 = 0x1688_A8A8;

/// Stop what fires without us, protect the flash, then report the state the
/// SoC came up in. Boards have no work here; their bootblock `main` calls
/// this.
///
/// WDT3 resets the SoC about one second after power-on to retry the SPI
/// address mode, and the strap may arm WDT2 to switch to the alternate boot
/// flash, so both are stopped first. CE0 is made read-only before any
/// access could turn into a flash page program.
pub fn bootblock(slot: &'static StaticConsole<Ns16550>, console: Ns16550Config) -> ! {
    pac::WDT3.CTRL().modify(|w| w.set_WDT_EN(false));
    pac::WDT2.CTRL().modify(|w| w.set_WDT_EN(false));
    pac::FMC
        .CE_TYPE()
        .modify(|w| w.set_CE0_DEFAULT_WRITE(false));
    pac::SCU.PROTECT().write(|w| w.set_KEY(SCU_UNLOCK_KEY));

    let Ok(mut uart) = Ns16550::new(console) else {
        fstart_arch::halt()
    };
    if uart.init().is_err() {
        fstart_arch::halt()
    }
    slot.install(uart);

    let strap = pac::SCU.HWSTRAP().read();
    fstart_log::info!("ast2500: bootblock from SPI");
    fstart_log::info!(
        "SCU7C silicon {}",
        Hex(pac::SCU.SILICON_REV().read().0.into())
    );
    fstart_log::info!(
        "SCU70 strap {} ({})",
        Hex(strap.0.into()),
        if strap.DDR4() { "DDR4" } else { "DDR3" }
    );
    fstart_log::info!(
        "SCU40 handshake {}",
        Hex(pac::SCU.VGA_HANDSHAKE0().read().0.into())
    );
    fstart_log::info!(
        "FMC00 {} FMC04 {} FMC10 {}",
        Hex(pac::FMC.CE_TYPE().read().0.into()),
        Hex(pac::FMC.CE_CTRL().read().0.into()),
        Hex(pac::FMC.CE_N_CTRL(0).read().0.into())
    );
    fstart_log::info!("ast2500: halting");
    fstart_arch::halt()
}
