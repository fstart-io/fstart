//! Stage code shared by the SoCs whose ARM core executes SPI flash in
//! place: the console, the boot timestamps, the CE0 read setting, the
//! memory test, loading the main stage and the whole main stage.

use crate::layout::{
    FIRMWARE_WINDOW_SIZE, FLASH_ALIAS, FLASH_CALIBRATION, FLASH_CALIBRATION_SIZE, Layout,
    TIMESTAMPS_SIZE,
};
use aspeed_hal::fmc;
use aspeed_pac as pac;
use fstart_arch::armv5te::TranslationTable;
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
pub(crate) const SCU_UNLOCK_KEY: u32 = 0x1688_A8A8;

/// DRAM covered per memtest progress line.
const MEMTEST_STEP: usize = 64 << 20;

/// The SoC a stage runs on: its log name and address map.
pub(crate) struct Soc {
    pub name: &'static str,
    pub layout: &'static Layout,
}

pub(crate) fn start_console(slot: &'static StaticConsole<Ns16550>, console: Ns16550Config) {
    let Ok(mut uart) = Ns16550::new(console) else {
        fstart_arch::halt()
    };
    if uart.init().is_err() {
        fstart_arch::halt()
    }
    slot.install(uart);
}

pub(crate) fn fail(soc: &Soc, reason: &str) -> ! {
    fstart_log::error!("{}: {}", soc.name, reason);
    fstart_arch::halt()
}

/// Timer 4 (index 3 of the per-timer registers) counts the boot timestamps
/// from the bootblock to the payload jump, free-running at 1 MHz. Timers 1
/// and 2 are the payload's; the delays use timer 3.
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
pub(crate) fn start_timestamps(soc: &Soc) {
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
    unsafe {
        fstart_timestamp::init(
            soc.layout.timestamps() as *mut u8,
            TIMESTAMPS_SIZE as usize,
            0,
        )
    };
    fstart_timestamp::add(id::BOOTBLOCK_START);
}

/// The 4 KiB of CE0 the fast read calibrates on, in its decode window.
pub(crate) const fn flash_calibration_area() -> (u32, u32) {
    (
        (FLASH_ALIAS + FLASH_CALIBRATION) as u32,
        FLASH_CALIBRATION_SIZE as u32,
    )
}

/// Log the CE0 read setting the fast read calibration chose.
pub(crate) fn log_flash_read(read: Result<fmc::Ce0Read, fmc::Error>) {
    match read {
        Ok(read) => fstart_log::info!(
            "fmc: CE0 dual fast read at HCLK/{} ({} kHz), read delay {}",
            read.divisor,
            read.spi_hz / 1000,
            read.delay.map_or(Hex(0), |d| Hex(d.into()))
        ),
        Err(e) => fstart_log::warn!(
            "fmc: CE0 stays at normal read, HCLK/16: {}",
            match e {
                fmc::Error::UniformArea => "calibration area uniform",
                fmc::Error::Dma => "DMA checksum timed out",
                fmc::Error::NoTiming => "no clock read correctly",
            }
        ),
    }
}

/// Own-address test over the usable DRAM, one progress line per 64 MiB of
/// each whole-range phase.
pub(crate) fn memtest(soc: &Soc, bytes: u32) {
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
            soc.layout.dram_base as *mut u32,
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

/// Load the main stage into the first `usable` bytes of DRAM, write its
/// handoff and enter it.
pub(crate) fn enter_main_stage(soc: &Soc, usable: u64) -> ! {
    let l = soc.layout;
    if l.dram_base + usable < l.main_stage_base + l.main_stage_size {
        fail(soc, "usable DRAM ends below the main stage");
    }
    let entry = load_main(l, usable).unwrap_or_else(|reason| {
        fstart_log::error!("{}: main stage: {}", soc.name, reason);
        fstart_arch::halt()
    });
    // SAFETY: the handoff page lies in usable DRAM, below the main stage
    // and reserved from its load.
    if unsafe { fstart_stage::next_stage::write_handoff(usable, l.handoff_addr) }.is_err() {
        fail(soc, "no locator for the handoff");
    }
    fstart_log::info!("{}: entering the main stage at {}", soc.name, Hex(entry));
    fstart_log::flush();
    fstart_arch::armv5te::jump_to_with_handoff(entry, l.handoff_addr as usize)
}

/// Authenticate the FFS root through CE0's window and load the main stage
/// it describes into `usable` bytes of DRAM; returns its entry.
fn load_main(l: &Layout, usable: u64) -> Result<u64, &'static str> {
    // SAFETY: CE0's decode window maps the boot flash, readable for the
    // whole boot.
    let media = unsafe { MemoryMapped::from_raw_addr(FLASH_ALIAS, FIRMWARE_WINDOW_SIZE as usize) };
    fstart_timestamp::add(id::VERIFY_BOOT_ROOT);
    let root = fstart_stage::root::authenticate_boot_root(fstart_stage::locator_bytes(), &media)
        .map_err(|_| "FFS root authentication failed")?;
    let main = root
        .descriptors()
        .iter()
        .flatten()
        .find(|d| d.role == fstart_ffs::root::BootstrapRole::Mainstage)
        .ok_or("no main stage in the FFS root")?;
    if main.load_addr != l.main_stage_base {
        return Err("main stage linked elsewhere");
    }
    let writable = [MemoryWindow {
        start: l.dram_base,
        size: usable,
    }];
    let reserved = [MemoryWindow {
        start: l.handoff_addr,
        size: HANDOFF_MAX_SIZE as u64,
    }];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 4,
    };
    // SAFETY: the bootblock runs from flash with its state in SRAM; the
    // usable DRAM is unused and the handoff page reserved.
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

/// The DRAM main stage: take over the bootblock's locator and DRAM size,
/// mount and verify the FFS, then boot the build's payload.
pub(crate) fn main_stage(
    soc: &Soc,
    slot: &'static StaticConsole<Ns16550>,
    console: Ns16550Config,
    handoff_addr: usize,
    timestamps: bool,
) -> ! {
    let l = soc.layout;
    if timestamps {
        fstart_timestamp::set_counter(&TIMESTAMP_COUNTER);
        // SAFETY: the bootblock started the table there; nothing else uses
        // the end of SRAM.
        unsafe { fstart_timestamp::attach(l.timestamps() as *mut u8, TIMESTAMPS_SIZE as usize) };
    }
    fstart_timestamp::add(id::RAMSTAGE_START);
    start_console(slot, console);
    fstart_log::info!("{} mainstage: entered", soc.name);
    if handoff_addr != l.handoff_addr as usize {
        fail(soc, "handoff at an unexpected address");
    }
    // SAFETY: the bootblock wrote this reserved page before jumping here.
    let Some(handoff) = (unsafe { fstart_stage::handoff::try_deserialize(handoff_addr) }) else {
        fail(soc, "no handoff");
    };
    let dram_size = handoff.dram_size.get();
    enable_dcache(l, dram_size);
    // SAFETY: single-threaded stage entry; the capacity is CE0's firmware
    // window, not taken from the locator.
    if unsafe { fstart_stage::anchor::install_locator(handoff.locator, FIRMWARE_WINDOW_SIZE) }
        .is_err()
    {
        fail(soc, "bad locator");
    }

    let Ok(running) = fstart_stage::boot::running_stage_windows() else {
        fail(soc, "no stage windows");
    };
    let writable = [MemoryWindow {
        start: l.dram_base,
        size: dram_size,
    }];
    let reserved = [
        running[0],
        running[1],
        MemoryWindow {
            start: l.handoff_addr,
            size: HANDOFF_MAX_SIZE as u64,
        },
    ];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 4,
    };
    // SAFETY: usable DRAM with this stage, its allocator and the handoff
    // excluded; nothing does DMA at this point.
    if unsafe {
        fstart_stage::directory::set_load_policy(&policy, const { codec(Compression::None) })
    }
    .is_err()
    {
        fail(soc, "load policy");
    }
    let image_size = fstart_stage::anchor::image_size();
    let ffs = fstart_stage::fixed_helpers::MemoryMappedFfs::new(FLASH_ALIAS, image_size);
    fstart_timestamp::add(id::MOUNT_BOOT_MEDIA);
    if ffs.mount().is_err() {
        fail(soc, "FFS mount");
    }
    fstart_timestamp::add(id::VERIFY_BOOT_MEDIA);
    if ffs.verify().is_err() {
        fail(soc, "FFS verification");
    }

    let memory = [E820Entry::new(l.dram_base, dram_size, E820Kind::Ram)];
    let description = Description::new(&memory, Range::new(FLASH_ALIAS, image_size as u64));
    fstart_timestamp::add(id::LOAD_PAYLOAD);
    fstart_payload::boot(Handoff::new(description, Placement::default()), ())
}

/// Cache DRAM and CE0's firmware window from here to the payload jump: the
/// payload copy and its hash otherwise wait on every DRAM and flash access.
fn enable_dcache(l: &Layout, dram_size: u64) {
    static mut TRANSLATION: TranslationTable = TranslationTable::new();
    let table = &raw mut TRANSLATION;
    // SAFETY: the main stage enters once and takes the table only here.
    let table = unsafe { &mut *table };
    // SAFETY: usable DRAM and the flash window are plain memory; nothing
    // does DMA into them before the payload.
    unsafe {
        fstart_arch::armv5te::enable_dcache(
            table,
            &[
                (l.dram_base, dram_size),
                (FLASH_ALIAS, FIRMWARE_WINDOW_SIZE),
            ],
        )
    };
}
