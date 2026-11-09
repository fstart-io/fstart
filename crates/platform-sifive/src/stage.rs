//! The FU740 stages. The LIM bootblock, from the reset vector to the
//! authenticated DRAM mainstage, as phases:
//!
//! ```ignore
//! Fu740::enter(&CONFIG)            // PRCI: core, DDR and peripheral clocks
//!     .console(&CONSOLE, &UART)
//!     .train_memory()
//!     .boot_next()                  // load the mainstage and jump to it
//! ```
//!
//! The DRAM mainstage starts at [`main_stage`]: it takes the bootblock's
//! handoff, retires the LIM aperture by enabling every L2 way for
//! caching, mounts the verified FFS and boots the payload.

use core::marker::PhantomData;

use fstart_core::description::Range;
use fstart_core::layout::{Layout, Region, RegionKind};
use fstart_core::services::{MemoryInit, ServiceError};
use fstart_driver_sifive::fu740_ddr::{FU740_DRAM_BASE, Fu740Ddr};
use fstart_driver_sifive::fu740_prci::Fu740Prci;
use fstart_driver_uart::sifive::{SifiveUart, SifiveUartConfig};
use fstart_log::StaticConsole;
use fstart_payload::{Description, Handoff, PciRootInfo, Placement};
use fstart_stage::{fail, step};

use crate::config::{FU740_ECAM_BASE, FU740_MMIO, Fu740Config};

/// Route UART0 to its pads: GPIO 16 (RX) and 17 (TX) on IOF0. Nothing
/// in the reset state does this; a previous boot stage having done it
/// is not something to rely on.
fn route_console_uart() {
    // SAFETY: the FU740 GPIO controller's fixed register block.
    let gpio = unsafe {
        fstart_driver_sifive::gpio::Gpio::new(fstart_driver_sifive::gpio::FU740_GPIO_BASE)
    };
    gpio.route_iof(16, false);
    gpio.route_iof(17, false);
}

/// The clocks run.
pub struct Clocks;
/// The console is up.
pub struct Console;
/// DRAM is trained.
pub struct Dram;

/// The FU740 stage, in phase `S`.
pub struct Fu740<S> {
    config: &'static Fu740Config,
    prci: Fu740Prci,
    console: Option<&'static SifiveUart>,
    dram: Range,
    _phase: PhantomData<S>,
}

impl<S> Fu740<S> {
    fn into<T>(self) -> Fu740<T> {
        Fu740 {
            config: self.config,
            prci: self.prci,
            console: self.console,
            dram: self.dram,
            _phase: PhantomData,
        }
    }
}

impl Fu740<Clocks> {
    /// Enter the stage and bring up the core, DDR and peripheral clocks.
    pub fn enter(config: &'static Fu740Config) -> Self {
        fstart_stage::anchor::initial_stage();
        let Ok(mut prci) = Fu740Prci::new(&config.prci) else {
            fstart_arch::riscv64::halt()
        };
        if prci.init().is_err() {
            fstart_arch::riscv64::halt();
        }
        Self {
            config,
            prci,
            console: None,
            dram: Range::default(),
            _phase: PhantomData,
        }
    }

    /// Clock the Ethernet MAC and release it from reset; the board resets
    /// its PHY first.
    pub fn enable_ethernet(self) -> Self {
        if self.prci.enable_ethernet().is_err() {
            fstart_arch::riscv64::halt();
        }
        self
    }

    /// Bring the UART up in `slot` as the log console.
    pub fn console(
        mut self,
        slot: &'static StaticConsole<SifiveUart>,
        uart: &'static SifiveUartConfig,
    ) -> Fu740<Console> {
        route_console_uart();
        let Ok(mut console) = SifiveUart::new(uart) else {
            fstart_arch::riscv64::halt()
        };
        if console.init().is_err() {
            fstart_arch::riscv64::halt();
        }
        self.console = Some(slot.install(console));
        fstart_log::info!("fu740: SiFive UART console ready");
        self.into()
    }
}

impl Fu740<Console> {
    /// Train the board's DDR.
    pub fn train_memory(mut self) -> Fu740<Dram> {
        let mut ddr = step("DDR", || Fu740Ddr::new(&self.config.ddr));
        self.dram = step("DDR training", || ddr.bring_up(()));
        fstart_log::info!("fu740: {} MiB DRAM", self.dram.size >> 20);
        self.into()
    }
}

impl Fu740<Dram> {
    /// Load the pinned mainstage into trained DRAM, leave the handoff
    /// below it and jump. Nothing after this needs the LIM.
    pub fn boot_next(self) -> ! {
        let entry = step("mainstage load", || load_mainstage(self.config, self.dram));
        let handoff = step("layout", || region(RegionKind::Reserved));
        // SAFETY: load_mainstage kept the handoff window out of the
        // mainstage's load footprint.
        step("handoff", || unsafe {
            fstart_stage::next_stage::write_handoff(self.dram.size, handoff.base)
                .map_err(|_| ServiceError::InvalidParam)
        });
        fstart_arch::riscv64::jump_to_with_handoff(entry, handoff.base as usize)
    }
}

fn regions() -> Result<Layout<'static>, ServiceError> {
    fstart_stage::layout::current().map_err(|_| ServiceError::InvalidParam)
}

/// One required role from this stage's layout descriptor.
fn region(kind: RegionKind) -> Result<Region, ServiceError> {
    regions()?.region(kind).ok_or(ServiceError::InvalidParam)
}

fn window(region: Region) -> fstart_stage::boot::MemoryWindow {
    fstart_stage::boot::MemoryWindow {
        start: region.base,
        size: region.size,
    }
}

/// Load only the descriptor pinned into this bootblock, from the SPI
/// flash the firmware window maps. The pin detects mainstage
/// corruption, not whole-image replacement: the board has no
/// established protected verifier for the initial image.
fn load_mainstage(config: &Fu740Config, dram: Range) -> Result<u64, ServiceError> {
    use fstart_core::services::boot_media::MemoryMapped;
    use fstart_stage::boot::MemoryPolicy;

    fstart_log::info!(
        "fu740: development integrity; no hardware secure boot or rollback enforcement"
    );
    let descriptor = fstart_stage::next_stage::pinned_bootstrap_descriptor()
        .map_err(|_| ServiceError::InvalidParam)?;
    let destination = region(RegionKind::BootstrapMainstage)?;
    let handoff = region(RegionKind::Reserved)?;
    if descriptor.load_addr != destination.base
        || descriptor
            .loaded_size
            .checked_add(descriptor.scratch_size)
            .is_none_or(|size| size > destination.size)
        || dram.size == 0
    {
        return Err(ServiceError::InvalidParam);
    }
    let writable = [fstart_stage::boot::MemoryWindow {
        start: dram.base,
        size: dram.size,
    }];
    let reserved = [window(handoff)];
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 4,
    };
    // SAFETY: the firmware window is the board's mapped SPI flash. DDR
    // training completed; the loader authenticates the mainstage before
    // granting its entry, within the bounded DRAM policy above. This
    // stage's code, stack and heap live in LIM, disjoint from DRAM.
    let executable = unsafe {
        let media = MemoryMapped::from_raw_addr(
            config.firmware_base,
            usize::try_from(config.firmware_size).map_err(|_| ServiceError::InvalidParam)?,
        );
        fstart_stage::boot::load_bootstrap(
            &media,
            &descriptor,
            &policy,
            const { fstart_stage::boot::codec(crate::compression::MAIN_STAGE) },
            &fstart_stage::boot::Measurement::NONE,
        )
    }
    .map_err(|_| ServiceError::InvalidParam)?;
    Ok(executable.entry())
}

/// The DRAM mainstage: take the bootblock's handoff, retire the LIM,
/// mount the verified FFS and boot the payload.
pub fn main_stage(
    config: &'static Fu740Config,
    slot: &'static StaticConsole<SifiveUart>,
    uart: &'static SifiveUartConfig,
    handoff_addr: usize,
) -> ! {
    route_console_uart();
    let Ok(mut console) = SifiveUart::new(uart) else {
        fstart_arch::riscv64::halt()
    };
    if console.init().is_err() {
        fstart_arch::riscv64::halt();
    }
    let console = slot.install(console);
    let handoff = step("handoff window", || region(RegionKind::Reserved));
    if handoff_addr != handoff.base as usize {
        fail("handoff address");
    }
    // SAFETY: the bootblock serialized the handoff into this reserved
    // window before jumping here, and kept it out of this stage's load.
    let inherited = match unsafe { fstart_stage::handoff::try_deserialize(handoff.base as usize) } {
        Some(inherited) => inherited,
        None => fail("handoff"),
    };
    // Only the bounded locator is inherited, not predecessor trust.
    let locator = inherited.locator;
    if locator.image_offset.get() != 0 || !locator.media().validate(config.firmware_size) {
        fail("locator");
    }
    // SAFETY: single-hart entry, before any directory is mounted.
    if unsafe { fstart_stage::anchor::install_locator(locator, config.firmware_size) }.is_err() {
        fail("locator");
    }
    // Never grant more RAM than the trusted DDR profile initialized.
    let dram_size = inherited.dram_size.get().min(config.ddr.dram_size);
    // This stage runs from DRAM; nothing live is left in LIM.
    fstart_driver_sifive::fu740_l2::enable_all_ways();
    step("boot media", || {
        install_boot_context(config, dram_size, handoff_addr)
    });
    fstart_log::info!("fu740 mainstage: ready for payload");
    hand_off(config, console, |handoff| fstart_payload::boot(handoff, ()))
}

/// OpenSBI's return into S-mode: launch CrabEFI with the console in `slot`.
#[cfg(feature = "crabefi")]
pub fn resume_sbi(
    config: &'static Fu740Config,
    slot: &'static StaticConsole<SifiveUart>,
    uart: &'static SifiveUartConfig,
    hart_id: u64,
    dtb_addr: u64,
) -> ! {
    let console = match slot.get() {
        Some(console) => console,
        None => {
            let Ok(mut console) = SifiveUart::new(uart) else {
                fstart_arch::riscv64::halt()
            };
            if console.init().is_err() {
                fstart_arch::riscv64::halt();
            }
            slot.install(console)
        }
    };
    // SAFETY: the console is a static.
    unsafe { fstart_log::replace_console(console) };
    hand_off(config, console, |handoff| {
        fstart_payload::Riscv64UefiPayload::resume_sbi(handoff, hart_id, dtb_addr)
    })
}

/// Without CrabEFI nothing returns from OpenSBI here.
#[cfg(not(feature = "crabefi"))]
pub fn resume_sbi(
    _: &'static Fu740Config,
    _: &'static StaticConsole<SifiveUart>,
    _: &'static SifiveUartConfig,
    _: u64,
    _: u64,
) -> ! {
    fstart_arch::riscv64::halt()
}

/// Describe the board to `boot` and run it. It has no boot device tree: the
/// Linux launcher loads the board's from the FFS.
fn hand_off(
    config: &'static Fu740Config,
    console: &'static SifiveUart,
    boot: impl FnOnce(Handoff<'_>) -> !,
) -> ! {
    use fstart_core::services::memory_detect::{E820Entry, E820Kind};
    let memory = [E820Entry::new(
        FU740_DRAM_BASE,
        config.ddr.dram_size,
        E820Kind::Ram,
    )];
    let mut description = Description::new(
        &memory,
        Range::new(config.firmware_base, config.firmware_size),
    );
    description.console = Some(console);
    description.mmio = &FU740_MMIO;
    description.pci_root = Some(PciRootInfo {
        segment: 0,
        ecam_base: FU740_ECAM_BASE,
        bus_start: 0,
        bus_end: 0xff,
    });
    let placement = Placement {
        kernel: config.kernel_addr,
        device_tree: config.dtb_addr,
        firmware: Range::new(config.firmware_addr, 0x20_0000),
        hart_id: config.boot_hart_id,
        bootargs: config.bootargs,
        ..Default::default()
    };
    boot(Handoff::new(description, placement))
}

/// This monolithic flow authenticates its own root, not an inherited one.
/// The board has no established protected initial verifier or rollback store.
fn install_boot_context(
    config: &Fu740Config,
    detected_size: u64,
    handoff: usize,
) -> Result<(), ServiceError> {
    use fstart_stage::boot::{MemoryPolicy, MemoryWindow};
    use fstart_stage::fixed_helpers::MemoryMappedFfs;

    // Never grant more RAM than the trusted DDR profile initialized,
    // even if the controller filter reports a larger address window.
    let size = detected_size.min(config.ddr.dram_size);
    if size == 0 || FU740_DRAM_BASE.checked_add(size).is_none() {
        return Err(ServiceError::InvalidParam);
    }
    let writable = [MemoryWindow {
        start: FU740_DRAM_BASE,
        size,
    }];
    let stage =
        fstart_stage::boot::running_stage_windows().map_err(|_| ServiceError::InvalidParam)?;
    let workspace = MemoryWindow {
        start: config.dtb_addr,
        size: 64 * 1024,
    };
    let source = {
        #[cfg(feature = "crabefi")]
        {
            let readable = [writable[0]];
            boot_dtb_window(workspace, &readable)?
        }
        #[cfg(not(feature = "crabefi"))]
        {
            workspace
        }
    };
    let reserved = [
        stage[0],
        stage[1],
        MemoryWindow {
            start: config.firmware_base,
            size: config.firmware_size,
        },
        if source == workspace {
            MemoryWindow { start: 0, size: 0 }
        } else {
            source
        },
        // No inherited directory is imported. Still keep any supplied
        // handoff bytes out of all loader destinations for this boot.
        MemoryWindow {
            start: handoff as u64,
            size: if handoff == 0 {
                0
            } else {
                fstart_core::handoff::HANDOFF_MAX_SIZE as u64
            },
        },
    ];
    if source != workspace {
        let source_reserved = [reserved[0], reserved[1], reserved[2], reserved[4]];
        let source_policy = MemoryPolicy {
            writable: &writable,
            reserved: &source_reserved,
            entry_alignment: 4,
        };
        if !source_policy.permits(source.start, source.size) {
            return Err(ServiceError::InvalidParam);
        }
    }
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 4,
    };
    // SAFETY: only initialized DRAM is writable. Linker ranges cover the full
    // live image, heap and stack in LIM; SPI and handoff are excluded. This
    // fixed flow starts no DMA devices or secondary harts and configures no
    // external temporary allocation arena.
    unsafe {
        fstart_stage::directory::set_load_policy(
            &policy,
            const { fstart_stage::boot::codec(crate::compression::PAYLOAD) },
        )
    }
    .map_err(|_| ServiceError::InvalidParam)?;
    #[cfg(any(feature = "linux", feature = "crabefi"))]
    // SAFETY: source is either a bounded boot DTB in trusted readable memory,
    // or the dedicated destination for an authenticated FFS DTB.
    // Registration checks destination RAM and all live reservations.
    unsafe { fstart_stage::configure_fdt_workspace(source, workspace) }?;
    fstart_log::info!(
        "fu740: development integrity; no hardware secure boot or rollback enforcement"
    );
    let firmware = MemoryMappedFfs::new(
        config.firmware_base,
        usize::try_from(config.firmware_size).map_err(|_| ServiceError::InvalidParam)?,
    );
    firmware.mount()?;
    firmware.verify()
}

#[cfg(feature = "crabefi")]
fn boot_dtb_window(
    destination: fstart_stage::boot::MemoryWindow,
    readable: &[fstart_stage::boot::MemoryWindow],
) -> Result<fstart_stage::boot::MemoryWindow, ServiceError> {
    use fstart_stage::boot::{MemoryPolicy, MemoryWindow};
    let address = fstart_arch::riscv64::boot_dtb_addr();
    if address == 0 {
        return Ok(destination);
    }
    let bounds = MemoryPolicy {
        writable: readable,
        reserved: &[],
        entry_alignment: 4,
    };
    if address & 3 != 0 || !bounds.permits(address, 8) {
        return Err(ServiceError::InvalidParam);
    }
    // SAFETY: header lies in the controller/profile-bounded DRAM window.
    let size = u64::from(u32::from_be(unsafe {
        core::ptr::read_unaligned((address + 4) as *const u32)
    }));
    if !(40..=destination.size).contains(&size) || !bounds.permits(address, size) {
        return Err(ServiceError::InvalidParam);
    }
    Ok(MemoryWindow {
        start: address,
        size,
    })
}
