//! AST2500 bootblock: the ARM1176 executes SPI flash in place from the reset
//! vector.

use aspeed_hal::ast2500::reset::{self, InitialReset};
use aspeed_hal::ast2500::sdmc::train::ScanError;
use aspeed_hal::ast2500::sdmc::{self, DRAM_BASE, Dram, TrainError};
pub use aspeed_hal::ast2500::sdmc::{CkDrive, Ddr4Odt, DramConfig, Organization, Speed};
use aspeed_pac as pac;
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_log::{Hex, StaticConsole};
use fstart_memtest::{Phase, Progress};

/// SCU00 protection key.
const SCU_UNLOCK_KEY: u32 = 0x1688_A8A8;

/// DRAM covered per memtest progress line.
const MEMTEST_STEP: usize = 64 << 20;

/// Stop what fires without us, protect the flash, bring up the console,
/// take the first reset of a cold boot, then train DRAM and test all of
/// the usable DRAM. Boards pass their DRAM choices; their bootblock `main`
/// calls this.
///
/// WDT3 resets the SoC about one second after power-on to retry the SPI
/// address mode, and the strap may arm WDT2 to switch to the alternate boot
/// flash, so both are stopped first. CE0 is made read-only before any
/// access could turn into a flash page program.
pub fn bootblock(
    slot: &'static StaticConsole<Ns16550>,
    console: Ns16550Config,
    dram: &DramConfig,
) -> ! {
    pac::WDT3.CTRL().modify(|w| w.set_WDT_EN(false));
    pac::WDT2.CTRL().modify(|w| w.set_WDT_EN(false));
    pac::FMC
        .CE_TYPE()
        .modify(|w| w.set_CE0_DEFAULT_WRITE(false));
    pac::SCU.PROTECT().write(|w| w.set_KEY(SCU_UNLOCK_KEY));
    enable_peripheral_port();

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

    if strap.ICE_FAST_RESET() {
        fstart_log::info!("ast2500: fast-reset flag set, first SoC reset");
    }
    if reset::initial_reset() == InitialReset::Completed {
        fstart_log::info!("ast2500: first SoC reset done");
    }

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

    memtest(dram.usable_bytes);
    fstart_log::info!("ast2500: halting");
    fstart_arch::halt()
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
        Hex(DRAM_BASE.into())
    );
}

/// Own-address test over the usable DRAM, one progress line per 64 MiB of
/// each whole-range phase.
fn memtest(bytes: u32) {
    let progress = |p: Progress| {
        fstart_log::info!(
            "memtest: pass {} {} {} MiB",
            p.pass,
            match p.phase {
                Phase::Write => "written",
                Phase::Verify => "verified",
            },
            (p.words * 4) >> 20
        )
    };
    // SAFETY: freshly trained DRAM below the usable limit; nothing lives
    // there yet (stack and data are in SRAM).
    let result = unsafe {
        fstart_memtest::own_address(
            DRAM_BASE as *mut u32,
            bytes as usize / 4,
            MEMTEST_STEP / 4,
            progress,
        )
    };
    match result {
        Ok(()) => fstart_log::info!("memtest: PASS"),
        Err(f) => {
            fstart_log::error!(
                "memtest: FAIL at {}: expected {} got {}",
                Hex(f.addr as u64),
                Hex(f.expected.into()),
                Hex(f.actual.into())
            );
            fstart_arch::halt()
        }
    }
}
