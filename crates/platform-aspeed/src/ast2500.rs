//! AST2500 stages: the bootblock, which the ARM1176 executes from SPI flash
//! in place from the reset vector, and the main stage it loads into DRAM.

use crate::layout::AST2500 as LAYOUT;
use crate::stage::{self, SCU_UNLOCK_KEY, Soc};
pub use aspeed_hal::ast2500::mac::{Mac, MacConfig};
use aspeed_hal::ast2500::reset::{self, InitialReset};
use aspeed_hal::ast2500::sdmc::train::ScanError;
use aspeed_hal::ast2500::sdmc::{self, Dram, TrainError};
pub use aspeed_hal::ast2500::sdmc::{CkDrive, Ddr4Odt, DramConfig, Organization, Speed};
use aspeed_hal::ast2500::{fmc, mac};
use aspeed_pac as pac;
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_log::{Hex, StaticConsole};
use fstart_timestamp::id;

const SOC: Soc = Soc {
    name: "ast2500",
    layout: &LAYOUT,
};

/// Stop what fires without us, protect the flash, bring up the console,
/// stop the coprocessor, take the first reset of a cold boot, switch CE0
/// to the fastest calibrated read up to `flash_max_hz`, set up the MAC
/// clocks and pins for `macs`, train DRAM, test it if `memtest` and it
/// was just trained, then load the main stage into DRAM and enter it.
/// With `timestamps`, the stages time their milestones and report them at
/// the payload jump. Boards pass their flash, MAC and DRAM choices; their
/// bootblock `main` calls this.
///
/// WDT3 resets the SoC about one second after power-on to retry the SPI
/// address mode, and the strap may arm WDT2 to switch to the alternate boot
/// flash, so both are stopped first. CE0 is made read-only before any
/// access could turn into a flash page program.
pub fn bootblock(
    slot: &'static StaticConsole<Ns16550>,
    console: Ns16550Config,
    flash_max_hz: u32,
    macs: &MacConfig,
    dram: &DramConfig,
    memtest: bool,
    timestamps: bool,
) -> ! {
    fstart_stage::anchor::initial_stage();
    if timestamps {
        stage::start_timestamps(&SOC);
    }
    pac::WDT3.CTRL().modify(|w| w.set_WDT_EN(false));
    pac::WDT2.CTRL().modify(|w| w.set_WDT_EN(false));
    pac::FMC
        .CE_TYPE()
        .modify(|w| w.set_CE0_DEFAULT_WRITE(false));
    pac::SCU.PROTECT().write(|w| w.set_KEY(SCU_UNLOCK_KEY));
    enable_peripheral_port();

    stage::start_console(slot, console);
    fstart_timestamp::add(id::CONSOLE_READY);
    fstart_log::info!("ast2500 bootblock: entered from SPI");
    stop_coprocessor();

    let initial_reset = reset::initial_reset(|| {
        fstart_log::info!("ast2500: cold boot, first SoC reset");
        fstart_log::flush();
    });

    let strap = pac::SCU.HWSTRAP().read();
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

    if initial_reset == InitialReset::Completed {
        fstart_log::info!("ast2500: first SoC reset done");
    }

    fstart_timestamp::add(id::EARLY_INIT);
    let (area, len) = stage::flash_calibration_area();
    stage::log_flash_read(fmc::fast_read(flash_max_hz, area, len));
    mac::init(macs);
    fstart_log::info!(
        "mac: D2-PLL 250 MHz, MAC1 {}, MAC2 {}",
        if strap.MAC1_RGMII() { "RGMII" } else { "RMII" },
        if strap.MAC2_RGMII() { "RGMII" } else { "RMII" }
    );

    fstart_timestamp::add(id::INITRAM_START);
    let dram = match sdmc::init(dram, log_retry) {
        Ok(dram) => dram,
        Err(e) => {
            fstart_log::error!(
                "dram: {}",
                match e {
                    sdmc::Error::Ddr3Speed => "DDR3L runs 1333 or 1600 MT/s only",
                    sdmc::Error::Organization => "organisation does not fit the strapped type",
                    sdmc::Error::CkDrive => "CK drive is DDR4-only, codes 0 to 15",
                    sdmc::Error::EccRange => "ECC range does not fit",
                }
            );
            fstart_arch::halt()
        }
    };
    log_dram(&dram);
    fstart_timestamp::add(id::INITRAM_END);
    if memtest && dram.training.is_some() {
        fstart_timestamp::add(id::RAMINIT_MEMORY_TEST);
        stage::memtest(&SOC, dram.usable_bytes);
    }

    stage::enter_main_stage(&SOC, u64::from(dram.usable_bytes))
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

/// Hold the ColdFire coprocessor in reset. It is off after power-on, but
/// a reset of the ARM alone (a watchdog in ARM-only mode, or one whose
/// reset mask leaves the coprocessor out) leaves it running from DRAM,
/// which DRAM init retrains and the stages overwrite. Loading its
/// firmware is the payload's job.
fn stop_coprocessor() {
    let ctrl = pac::COPROC.CTRL();
    if ctrl.read().EN() {
        fstart_log::info!("coprocessor: running, stopped");
    }
    ctrl.write(|w| w.set_RESET(true));
}

/// Route `0x0000_0000..0x8000_0000` (flash and I/O) through the ARM1176
/// peripheral port, leaving DRAM on the main AXI port (CP15 c15
/// peripheral port remap).
fn enable_peripheral_port() {
    // SAFETY: ARM1176 peripheral port remap: base 0, size code 0x16 (2 GiB).
    unsafe {
        core::arch::asm!(
            "mcr p15, 0, {r}, c15, c2, 4",
            r = in(reg) 0x16u32,
            options(nostack, preserves_flags)
        )
    };
}

fn log_retry(e: TrainError) {
    match e {
        TrainError::PhyInit => fstart_log::warn!("dram: PHY init failed, retrying"),
        TrainError::PhyVref(s) => log_scan("PHY", s),
        TrainError::DramVref(s) => log_scan("DRAM", s),
        TrainError::Margin { gate, eye } => fstart_log::warn!(
            "dram: read margin gate {} eye {}, retrying",
            Hex(gate.into()),
            Hex(eye.into())
        ),
        TrainError::Test => fstart_log::warn!("dram: engine test failed, retrying"),
    }
}

fn log_scan(side: &str, e: ScanError<()>) {
    match e {
        ScanError::Margin(w) => fstart_log::warn!(
            "dram: {} Vref passes {}..{} ({}), retrying",
            side,
            Hex(w.first.into()),
            Hex(w.last.into()),
            w.passes
        ),
        _ => fstart_log::warn!("dram: {} Vref never passed, retrying", side),
    }
}

fn log_dram(dram: &Dram) {
    match &dram.training {
        Some(t) => {
            fstart_log::info!(
                "dram: trained in {} ms after {} retries",
                t.elapsed_us / 1000,
                t.retries
            );
            if let (Some(phy), Some(dram)) = (t.phy_vref, t.dram_vref) {
                fstart_log::info!(
                    "dram: PHY Vref {} ({} passes), DRAM Vref {} ({}..{})",
                    Hex(phy.best.into()),
                    phy.passes,
                    Hex(dram.center().into()),
                    Hex(dram.first.into()),
                    Hex(dram.last.into())
                );
            }
            fstart_log::info!(
                "dram: gate window {} eye window {}",
                Hex(t.gate.into()),
                Hex(t.eye.into())
            );
        }
        None => fstart_log::info!("dram: already initialised (SCU40), training skipped"),
    }
    fstart_log::info!(
        "dram: {} MiB, VGA {} MiB, usable {} MiB at {}",
        dram.capacity.bytes() >> 20,
        dram.vga_bytes >> 20,
        dram.usable_bytes >> 20,
        Hex(LAYOUT.dram_base)
    );
}
