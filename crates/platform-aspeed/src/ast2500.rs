//! AST2500 stages: the bootblock, which the ARM1176 executes from SPI flash
//! in place from the reset vector, and the main stage it loads into DRAM.

use crate::layout::{
    DRAM_BASE as DRAM, FIRMWARE_WINDOW_SIZE, FLASH_ALIAS, HANDOFF_ADDR, MAIN_STAGE_BASE,
    MAIN_STAGE_SIZE, TIMESTAMPS, TIMESTAMPS_SIZE,
};
use aspeed_hal::ast2500::reset::{self, InitialReset};
use aspeed_hal::ast2500::sdmc::train::ScanError;
use aspeed_hal::ast2500::sdmc::{self, Dram, TrainError};
pub use aspeed_hal::ast2500::sdmc::{CkDrive, Ddr4Odt, DramConfig, Organization, Speed};
use aspeed_pac as pac;
use fstart_core::ffs::Compression;
use fstart_core::handoff::HANDOFF_MAX_SIZE;
use fstart_core::services::boot_media::MemoryMapped;
use fstart_core::services::memory_detect::{E820Entry, E820Kind};
use fstart_driver_uart::ns16550::{Ns16550, Ns16550Config};
use fstart_log::{Hex, StaticConsole};
use fstart_memtest::{Phase, Progress};
use fstart_payload::{Description, Handoff, Placement, Range};
use fstart_stage::boot::{Measurement, MemoryPolicy, MemoryWindow, codec, load_bootstrap};
use fstart_timestamp::{Counter, id};

/// SCU00 protection key.
const SCU_UNLOCK_KEY: u32 = 0x1688_A8A8;

/// DRAM covered per memtest progress line.
const MEMTEST_STEP: usize = 64 << 20;

/// Stop what fires without us, protect the flash, bring up the console,
/// take the first reset of a cold boot, train DRAM, test it if `memtest`
/// and it was just trained, then load the main stage into DRAM and enter
/// it. Boards pass their DRAM choices; their bootblock `main` calls this.
///
/// WDT3 resets the SoC about one second after power-on to retry the SPI
/// address mode, and the strap may arm WDT2 to switch to the alternate boot
/// flash, so both are stopped first. CE0 is made read-only before any
/// access could turn into a flash page program.
pub fn bootblock(
    slot: &'static StaticConsole<Ns16550>,
    console: Ns16550Config,
    dram: &DramConfig,
    memtest: bool,
) -> ! {
    fstart_stage::anchor::initial_stage();
    start_timestamps();
    pac::WDT3.CTRL().modify(|w| w.set_WDT_EN(false));
    pac::WDT2.CTRL().modify(|w| w.set_WDT_EN(false));
    pac::FMC
        .CE_TYPE()
        .modify(|w| w.set_CE0_DEFAULT_WRITE(false));
    pac::SCU.PROTECT().write(|w| w.set_KEY(SCU_UNLOCK_KEY));
    enable_peripheral_port();

    start_console(slot, console);
    fstart_timestamp::add(id::CONSOLE_READY);

    let initial_reset = reset::initial_reset(|| {
        fstart_log::info!("ast2500: cold boot, first SoC reset");
        fstart_log::flush();
    });

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

    if initial_reset == InitialReset::Completed {
        fstart_log::info!("ast2500: first SoC reset done");
    }

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
        self::memtest(dram.usable_bytes);
    }

    let usable = u64::from(dram.usable_bytes);
    if DRAM + usable < MAIN_STAGE_BASE + MAIN_STAGE_SIZE {
        fstart_log::error!("ast2500: usable DRAM ends below the main stage");
        fstart_arch::halt()
    }
    let entry = load_main(usable).unwrap_or_else(|reason| {
        fstart_log::error!("ast2500: main stage: {}", reason);
        fstart_arch::halt()
    });
    // SAFETY: the handoff page lies in trained, usable DRAM, below the main
    // stage and reserved from its load.
    if unsafe { fstart_stage::next_stage::write_handoff(usable, HANDOFF_ADDR) }.is_err() {
        fstart_log::error!("ast2500: no locator for the handoff");
        fstart_arch::halt()
    }
    fstart_log::info!("ast2500: entering the main stage at {}", Hex(entry));
    fstart_log::flush();
    fstart_arch::armv5te::jump_to_with_handoff(entry, HANDOFF_ADDR as usize)
}

/// The DRAM main stage: take over the bootblock's locator and DRAM size,
/// mount and verify the FFS, then boot the build's payload. The board's
/// main stage `main` calls this with the handoff address.
pub fn main_stage(
    slot: &'static StaticConsole<Ns16550>,
    console: Ns16550Config,
    handoff_addr: usize,
) -> ! {
    fstart_timestamp::set_counter(&TIMESTAMP_COUNTER);
    // SAFETY: the bootblock started the table there; nothing else uses the
    // end of SRAM.
    unsafe { fstart_timestamp::attach(TIMESTAMPS as *mut u8, TIMESTAMPS_SIZE as usize) };
    fstart_timestamp::add(id::RAMSTAGE_START);
    start_console(slot, console);
    fstart_log::info!("ast2500: main stage");
    if handoff_addr != HANDOFF_ADDR as usize {
        fail("handoff at an unexpected address");
    }
    // SAFETY: the bootblock wrote this reserved page before jumping here.
    let Some(handoff) = (unsafe { fstart_stage::handoff::try_deserialize(handoff_addr) }) else {
        fail("no handoff");
    };
    let dram_size = handoff.dram_size.get();
    // SAFETY: single-threaded stage entry; the capacity is CE0's firmware
    // window, not taken from the locator.
    if unsafe { fstart_stage::anchor::install_locator(handoff.locator, FIRMWARE_WINDOW_SIZE) }
        .is_err()
    {
        fail("bad locator");
    }

    let Ok(running) = fstart_stage::boot::running_stage_windows() else {
        fail("no stage windows");
    };
    let writable = [MemoryWindow {
        start: DRAM,
        size: dram_size,
    }];
    let reserved = [
        running[0],
        running[1],
        MemoryWindow {
            start: HANDOFF_ADDR,
            size: HANDOFF_MAX_SIZE as u64,
        },
    ];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 4,
    };
    // SAFETY: trained DRAM with this stage, its allocator and the handoff
    // excluded; nothing on the AST2500 does DMA at this point.
    if unsafe {
        fstart_stage::directory::set_load_policy(&policy, const { codec(Compression::None) })
    }
    .is_err()
    {
        fail("load policy");
    }
    let image_size = fstart_stage::anchor::image_size();
    let ffs = fstart_stage::fixed_helpers::MemoryMappedFfs::new(FLASH_ALIAS, image_size);
    fstart_timestamp::add(id::MOUNT_BOOT_MEDIA);
    if ffs.mount().is_err() {
        fail("FFS mount");
    }
    fstart_timestamp::add(id::VERIFY_BOOT_MEDIA);
    if ffs.verify().is_err() {
        fail("FFS verification");
    }

    let memory = [E820Entry::new(DRAM, dram_size, E820Kind::Ram)];
    let description = Description::new(&memory, Range::new(FLASH_ALIAS, image_size as u64));
    fstart_timestamp::add(id::LOAD_PAYLOAD);
    fstart_payload::boot(Handoff::new(description, Placement::default()), ())
}

/// Timer 4 (index 3 of the per-timer registers) counts the boot timestamps
/// from the bootblock to the payload jump, free-running at 1 MHz. Timers 1
/// and 2 are the payload's; the DRAM init delays use timer 3.
const TIMESTAMP_TIMER: usize = 3;

static TIMESTAMP_COUNTER: Counter = Counter {
    read: || u64::from(u32::MAX - pac::TIMER.STATUS(TIMESTAMP_TIMER).read().VALUE()),
    hz: 1_000_000,
    stop: || {
        pac::TIMER.CTRL().modify(|w| {
            w.set_T4_EN(false);
            w.set_T4_CLK_SEL(false);
        })
    },
};

/// Start timer 4 and an empty timestamp table at the end of SRAM.
fn start_timestamps() {
    pac::TIMER
        .RELOAD(TIMESTAMP_TIMER)
        .write(|w| w.set_VALUE(u32::MAX));
    pac::TIMER.CTRL().modify(|w| {
        w.set_T4_EN(true);
        w.set_T4_CLK_SEL(true);
    });
    fstart_timestamp::set_counter(&TIMESTAMP_COUNTER);
    // SAFETY: the table span at the end of SRAM is outside the bootblock's
    // data, heap and stack, and 8-byte aligned.
    unsafe { fstart_timestamp::init(TIMESTAMPS as *mut u8, TIMESTAMPS_SIZE as usize, 0) };
    fstart_timestamp::add(id::BOOTBLOCK_START);
}

fn start_console(slot: &'static StaticConsole<Ns16550>, console: Ns16550Config) {
    let Ok(mut uart) = Ns16550::new(console) else {
        fstart_arch::halt()
    };
    if uart.init().is_err() {
        fstart_arch::halt()
    }
    slot.install(uart);
}

fn fail(reason: &str) -> ! {
    fstart_log::error!("ast2500: {}", reason);
    fstart_arch::halt()
}

/// Authenticate the FFS root through CE0's window and load the main stage
/// it describes into `usable` bytes of DRAM; returns its entry.
fn load_main(usable: u64) -> Result<u64, &'static str> {
    // SAFETY: CE0's decode window maps the boot flash, readable for the
    // whole boot.
    let media = unsafe { MemoryMapped::from_raw_addr(FLASH_ALIAS, FIRMWARE_WINDOW_SIZE as usize) };
    fstart_timestamp::add(id::VERIFY_BOOT_ROOT);
    let root =
        fstart_stage::root::authenticate_boot_root(fstart_stage::fstart_anchor_bytes(), &media)
            .map_err(|_| "FFS root authentication failed")?;
    let main = root
        .descriptors()
        .iter()
        .flatten()
        .find(|d| d.role == fstart_ffs::root::BootstrapRole::Mainstage)
        .ok_or("no main stage in the FFS root")?;
    if main.load_addr != MAIN_STAGE_BASE {
        return Err("main stage linked elsewhere");
    }
    let writable = [MemoryWindow {
        start: DRAM,
        size: usable,
    }];
    let reserved = [MemoryWindow {
        start: HANDOFF_ADDR,
        size: HANDOFF_MAX_SIZE as u64,
    }];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 4,
    };
    // SAFETY: the bootblock runs from flash with its state in SRAM; the
    // trained DRAM is unused and the handoff page reserved.
    fstart_timestamp::add(id::LOAD_RAMSTAGE);
    let main = unsafe {
        load_bootstrap(
            &media,
            main,
            &policy,
            const { codec(Compression::None) },
            &Measurement::NONE,
        )
    }
    .map_err(|_| "load or verification failed")?;
    Ok(main.entry())
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
        Hex(DRAM)
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
            DRAM as *mut u32,
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
