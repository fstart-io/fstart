//! The FU740 stage, from the reset vector to the payload, as phases:
//!
//! ```ignore
//! Fu740::enter(&CONFIG)            // PRCI: core, DDR and peripheral clocks
//!     .console(&CONSOLE, &UART)
//!     .train_memory()
//!     .boot_media()
//!     .boot()
//! ```

use core::marker::PhantomData;

use fstart_core::description::Range;
use fstart_core::services::{MemoryInit, ServiceError};
use fstart_driver_sifive::fu740_ddr::{FU740_DRAM_BASE, Fu740Ddr};
use fstart_driver_sifive::fu740_prci::Fu740Prci;
use fstart_driver_uart::sifive::{SifiveUart, SifiveUartConfig};
use fstart_log::StaticConsole;
use fstart_payload::{Description, Handoff, MainstagePayload, PciRootInfo, Placement};
use fstart_stage::{fail, step};

use crate::config::{FU740_ECAM_BASE, FU740_MMIO, Fu740Config};

/// The clocks run.
pub struct Clocks;
/// The console is up.
pub struct Console;
/// DRAM is trained.
pub struct Dram;
/// The verified boot media is mounted.
pub struct Ready;

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
        self.dram = step("DDR training", || ddr.bring_up());
        fstart_log::info!("fu740: {} MiB DRAM", self.dram.size >> 20);
        self.into()
    }
}

impl Fu740<Dram> {
    /// Grant the loader the trained DRAM and mount the verified FFS on the
    /// SPI flash.
    pub fn boot_media(self) -> Fu740<Ready> {
        // The only stage: no predecessor handoff to keep out of loads.
        step("boot media", || {
            install_boot_context(self.config, self.dram.size, 0)
        });
        self.into()
    }
}

impl Fu740<Ready> {
    /// Describe the board to the build's payload launcher and boot it.
    pub fn boot(self) -> ! {
        fstart_log::info!("fu740 ramstage: ready for payload");
        let console = self.console.unwrap_or_else(|| fail("console"));
        hand_off(self.config, console, |handoff| {
            <fstart_payload::BuildSelectedPayload as MainstagePayload>::boot(handoff, ())
        })
    }
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
        #[cfg(all(fstart_payload = "crabefi", not(fstart_payload = "linux")))]
        {
            let readable = [writable[0]];
            boot_dtb_window(workspace, &readable)?
        }
        #[cfg(not(all(fstart_payload = "crabefi", not(fstart_payload = "linux"))))]
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
    unsafe { fstart_stage::directory::set_load_policy(&policy, &fstart_stage::boot::Lz4) }
        .map_err(|_| ServiceError::InvalidParam)?;
    #[cfg(any(fstart_payload = "linux", fstart_payload = "crabefi"))]
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

#[cfg(all(fstart_payload = "crabefi", not(fstart_payload = "linux")))]
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
