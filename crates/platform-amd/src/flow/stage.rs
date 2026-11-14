// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stage phases: the PSP released the BSP at the reset vector of the
//! image it copied to DRAM. The arch crate's shared `amd_psp_reset.S` opens a
//! write-back window over the firmware RAM, builds the identity map, enters
//! long mode and continues at the architectural `_start_ram` (stack, BSS,
//! IDT), which calls the board's `main` (`fstart_platform_amd::entry!`).
//!
//! The board's `main` drives the phases and does its own work between them.
//! Each phase consumes the previous one ([`Stage`]'s second parameter), so
//! they run once and in order:
//!
//! ```ignore
//! let stage = match start::<Board>() {            // ECAM, FCH pre-init
//!     Cpu::Zeppelin(id) => id.enter(CONFIG),
//!     cpu => cpu.unsupported(),
//! };
//! // board: LPC decodes for its console (stage.hw())
//! let stage = stage.console(&CONSOLE, console)    // memory map, FFS
//!     .init_fch();                                // DF, FCH
//! // board: devices on the FCH's buses, early pads
//! let stage = stage.train_dxio(/* lanes */);
//! // board: links are trained
//! let stage = stage.init_chipset(/* what the board found */)
//!     .start_cpus()                               // MP, SMM
//!     .scan_bus();
//! // board: after PCI enumeration
//! let stage = stage.ready_to_boot();
//! // board: right before the tables
//! stage.emit_tables(&SMBIOS, board_acpi).boot()
//! ```
//!
//! What the silicon needs in each phase is the SoC driver's
//! ([`crate::soc::Init`]); the phases are the same for every
//! SoC.
use super::{Ctx, Mapped, Memory, Silicon, Soc, Tables};
use crate::mp;
use crate::soc::Entered;
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_core::services::memory_detect::E820State;
use fstart_driver_amd::{apob::Apob, ccx::tsc::Tsc, fch::post_native as post};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::{Hex, StaticConsole};
use fstart_pci::PciEcam;

fn halt() -> ! {
    fstart_arch::x86::boot::halt()
}

/// Stop the boot: log that `what` failed, flush the log and halt.
pub fn fail<S: Soc>(what: &'static str) -> ! {
    fstart_log::error!("{}: {} failed, halting", S::NAME, what);
    fstart_log::flush();
    halt()
}

/// The first phase: ECAM and the SoC's FCH setup before the console, from
/// the board's configuration `config` of the SoC.
pub fn enter<S: Soc>(board: crate::board::Facts, config: S::Config) -> Early<S> {
    Early::enter(board, config)
}

/// Right after the reset vector: ECAM and the FCH decode the console needs.
pub struct Early<S: Soc> {
    board: crate::board::Facts,
    config: S::Config,
    msr: NativeMsr,
    entered: Entered<S>,
}

impl<S: Soc> Early<S> {
    fn enter(board: crate::board::Facts, config: S::Config) -> Self {
        fstart_stage::anchor::initial_stage();
        // SAFETY: BSP only, single-threaded cold boot.
        let msr = unsafe { NativeMsr::new() };
        // A failure is reported once there is a console.
        let entered = S::enter(&config, &msr);
        Self {
            board,
            config,
            msr,
            entered,
        }
    }

    /// The SoC's register handles, for the decodes and the Super I/O
    /// routing of a console behind the LPC bridge.
    pub fn hw(&self) -> &S::Hw {
        &self.entered.hw
    }

    /// Stop the boot: the board's `what` failed. There is no console yet.
    pub fn fail(&self, what: &'static str) -> ! {
        fail::<S>(what)
    }

    /// Bring up the console, identify the CPU, then the SoC's memory map
    /// from the APOB, cached, with the verified FFS mounted
    /// ([`Soc::map`]).
    pub fn console(
        self,
        slot: &'static StaticConsole<Ns16550>,
        config: fstart_driver_uart::ns16550::Ns16550Config,
    ) -> Stage<S, Console> {
        let Self {
            board,
            config: mut soc_config,
            msr,
            entered:
                Entered {
                    mut hw,
                    clock,
                    pre_init,
                },
        } = self;
        post(0x31);
        // Nothing can be logged yet; the POST code shows where.
        let Ok(mut console) = Ns16550::new(config) else {
            fail::<S>("console")
        };
        if console.init().is_err() {
            fail::<S>("console")
        }
        let console = slot.install(console);
        if let Err(what) = pre_init {
            fail::<S>(what);
        }

        let bist = fstart_arch::x86::psp_reset::bsp_bist();
        if bist != 0 {
            fstart_log::error!("{}: BSP BIST {}", S::NAME, Hex(bist.into()));
            fail::<S>("BIST");
        }
        let identity = match S::identify(&hw, &soc_config, clock) {
            Ok(identity) => identity,
            Err(what) => fail::<S>(what),
        };
        let Some(clock) = clock else {
            fail::<S>("CPU identification")
        };
        let Mapped {
            top_mem,
            top_mem2,
            apob,
            tseg,
            e820,
            ffs,
            nvram,
            options,
            flow,
        } = S::map(&mut hw, &mut soc_config, &board, &msr, clock)
            .unwrap_or_else(|what| fail::<S>(what));
        post(0x32);

        Stage {
            machine: Machine {
                board,
                config: soc_config,
                console,
                hw,
                msr,
                clock,
                identity,
                top_mem,
                top_mem2,
                apob,
                tseg,
                e820,
                ffs,
                nvram,
                options,
                flow,
            },
            phase: Console,
        }
    }
}

/// What every phase after the console works on.
struct Machine<S: Soc> {
    board: crate::board::Facts,
    config: S::Config,
    console: &'static Ns16550,
    hw: S::Hw,
    msr: NativeMsr,
    clock: Tsc,
    identity: S::Identity,
    top_mem: u64,
    top_mem2: Option<u64>,
    apob: Option<Apob<'static>>,
    tseg: u64,
    e820: E820State,
    /// The FFS with the payload.
    ffs: core::ops::Range<u64>,
    /// The settings store, if the SoC has one.
    nvram: Option<&'static [u8]>,
    /// The outcome of every option knob resolved so far.
    options: fstart_options::Report,
    /// What the SoC's flow services keep between the phases.
    flow: S::Flow,
}

/// The stage after the console, in phase `P`.
pub struct Stage<S: Soc, P> {
    machine: Machine<S>,
    phase: P,
}

/// The console is up, DRAM is mapped and cached, the FFS is mounted.
pub struct Console;

/// The FCH's buses are up ([`crate::soc::Init::init_fch`]).
pub struct FchUp<S: Soc>(S::FchUp);

/// The links are trained.
pub struct Trained<S: Soc>(S::Trained);

/// The silicon is ready for MP.
pub struct Chipset<S: Soc>(S::Chipset);

/// The silicon init is done: MP, SMM and what the SoC reports for the
/// tables.
pub struct Cpus<S: Soc>(Silicon<S>);

/// PCI is enumerated and its resources are allocated. The allocator keeps
/// the root bridges it was built from; the tables describe those.
pub struct Enumerated<S: Soc> {
    silicon: Silicon<S>,
    pci: PciEcam,
}

/// Everything the tables describe is ready.
pub struct Ready<S: Soc> {
    silicon: Silicon<S>,
    pci: PciEcam,
}

/// The ACPI and SMBIOS tables are installed.
pub struct Published<S: Soc> {
    silicon: Silicon<S>,
    pci: PciEcam,
    tables: Tables,
    #[cfg(feature = "payload-uefi-basic")]
    peripherals: super::Peripherals,
}

impl<S: Soc> Machine<S> {
    /// What the SoC's steps work on.
    fn ctx(&self) -> Ctx<'_, S> {
        Ctx {
            config: &self.config,
            board: &self.board,
            identity: self.identity,
            hw: &self.hw,
            msr: &self.msr,
            clock: self.clock,
            memory: Memory {
                top_mem: self.top_mem,
                top_mem2: self.top_mem2,
                apob: self.apob.as_ref(),
                tseg: self.tseg,
                tseg_size: S::TSEG_SIZE,
                e820: &self.e820,
            },
        }
    }
}

impl<S: Soc, P> Stage<S, P> {
    /// The board's configuration of the SoC.
    pub fn config(&self) -> &S::Config {
        &self.machine.config
    }

    /// What the SoC's CPU identification found.
    pub fn identity(&self) -> S::Identity {
        self.machine.identity
    }

    /// The memory map from the APOB and the CPU's TOP_MEM/TOM2.
    pub fn memory(&self) -> Memory<'_> {
        self.machine.ctx().memory
    }

    /// The SoC's register handles, for the board's own work on them.
    pub fn hw(&self) -> &S::Hw {
        &self.machine.hw
    }

    /// The calibrated clock, to pace board device transactions.
    pub fn clock(&self) -> Tsc {
        self.machine.clock
    }

    /// Stop the boot after logging that the board's `what` failed.
    pub fn fail(&self, what: &'static str) -> ! {
        fail::<S>(what)
    }

    /// Apply the stored overrides of `knobs` to the board's `config`
    /// (`fstart_options`), before the board uses it. `check` is the
    /// whole-set validation of the consumer: if the overridden
    /// configuration fails it, `config` keeps the board's values and the
    /// overrides are reported as rejected. The knobs are shown in the
    /// payload's settings menu.
    pub fn resolve_options<C: Clone>(
        &mut self,
        knobs: &'static [fstart_options::Knob<C>],
        config: &mut C,
        check: impl FnOnce(&C) -> Result<(), &'static str>,
    ) {
        let machine = &mut self.machine;
        crate::options::resolve(machine.nvram, knobs, config, &mut machine.options, check);
    }
}

impl<S: Soc> Stage<S, Console> {
    /// The silicon up to the FCH's buses; the board's devices on them and
    /// its early pads come next.
    pub fn init_fch(self) -> Stage<S, FchUp<S>> {
        let up = S::init_fch(&self.machine.ctx()).unwrap_or_else(|what| fail::<S>(what));
        Stage {
            machine: self.machine,
            phase: FchUp(up),
        }
    }
}

impl<S: Soc> Stage<S, FchUp<S>> {
    /// Link training of the board's `lanes`; board work that needs trained
    /// links comes next.
    pub fn train_dxio(self, lanes: S::Lanes<'_>) -> Stage<S, Trained<S>> {
        let Stage {
            machine,
            phase: FchUp(up),
        } = self;
        let trained =
            S::train_dxio(&machine.ctx(), up, lanes).unwrap_or_else(|what| fail::<S>(what));
        Stage {
            machine,
            phase: Trained(trained),
        }
    }
}

impl<S: Soc> Stage<S, Trained<S>> {
    /// The rest of the silicon init before MP, with what the board learned
    /// at run time since the FCH came up (`runtime`).
    pub fn init_chipset(self, runtime: S::Runtime) -> Stage<S, Chipset<S>> {
        let Stage {
            machine,
            phase: Trained(trained),
        } = self;
        let chipset = S::init_chipset(&machine.ctx(), trained, runtime)
            .unwrap_or_else(|what| fail::<S>(what));
        Stage {
            machine,
            phase: Chipset(chipset),
        }
    }
}

impl<S: Soc> Stage<S, Chipset<S>> {
    /// MP init with the SoC's CPU driver on every CPU, SMM with the PSP's
    /// SMM info, then what the SoC does before PCI enumeration.
    pub fn start_cpus(self) -> Stage<S, Cpus<S>> {
        let Stage {
            mut machine,
            phase: Chipset(chipset),
        } = self;
        let (cpus, mut state) =
            bring_up_cpus(&machine, chipset).unwrap_or_else(|what| fail::<S>(what));
        // What the SMM handler needs of the RAM leaves the memory map
        // before the handler is installed and locked.
        let reserved =
            S::smm_memory(&machine.ctx(), &mut state).unwrap_or_else(|what| fail::<S>(what));
        for range in reserved {
            if machine
                .e820
                .reserve_range(range.start, range.end - range.start)
                .is_err()
            {
                fail::<S>("memory map");
            }
        }
        let silicon = install_smm(&machine, cpus, state).unwrap_or_else(|what| fail::<S>(what));
        Stage {
            machine,
            phase: Cpus(silicon),
        }
    }
}

fn bring_up_cpus<S: Soc>(
    machine: &Machine<S>,
    mut chipset: S::Chipset,
) -> Result<(mp::Cpus, S::State), &'static str> {
    let ctx = machine.ctx();
    let cpu = S::cpu_driver(&ctx, &mut chipset, fstart_stage::microcode_blob())?;
    let threads = S::threads(&ctx, &chipset)?;
    let cpus = mp::start(
        S::NAME,
        &cpu,
        || S::cpu_errors(&cpu),
        threads,
        ctx.board.max_cpus,
        |aps| S::release_aps(&ctx, &chipset, aps),
    )?;
    let state = S::cpus_up(&ctx, chipset, &cpus.mp, &cpus.apic_ids)?;
    Ok((cpus, state))
}

fn install_smm<S: Soc>(
    machine: &Machine<S>,
    cpus: mp::Cpus,
    mut state: S::State,
) -> Result<Silicon<S>, &'static str> {
    let ctx = machine.ctx();
    S::install_smm(&ctx, &mut state, &cpus.mp, &machine.flow)?;
    S::before_enumeration(&ctx, &mut state)?;
    let (devices, aml) = S::acpi_devices(ctx.config, &state)?;
    Ok(Silicon {
        cpus,
        devices,
        aml,
        tables: alloc::vec::Vec::new(),
        framebuffer: None,
        state,
    })
}

impl<S: Soc> Stage<S, Cpus<S>> {
    /// PCI enumeration and resource allocation below every root bridge the
    /// SoC reports, write-combining for the framebuffer apertures, then
    /// what the SoC does after the allocation.
    pub fn scan_bus(self) -> Stage<S, Enumerated<S>> {
        let Stage {
            machine,
            phase: Cpus(mut silicon),
        } = self;
        let ctx = machine.ctx();
        let pci = enumerate::<S>(&ctx, &silicon);
        // PCI resources become known after AMD's required MP and SMU steps.
        // Extend cacheability coherently on all CPUs without replacing AMD's
        // fixed DRAM routing, high-memory TOM2WB or the existing RAM/ROM MTRRs.
        let wc = pci
            .prefetchable_vga_windows()
            .map(|window| (window.base, window.size));
        // SAFETY: BSP owns MP dispatch; interrupts are disabled, APs are online,
        // and only allocated prefetchable VGA BARs are eligible.
        if let Err(error) =
            unsafe { fstart_arch::x86::mtrr::install_write_combining(wc, &silicon.cpus.mp) }
        {
            if error == fstart_core::services::ServiceError::NotSupported {
                fstart_log::warn!(
                    "mtrr: framebuffer WC unavailable; preserving existing cache policy"
                );
            } else {
                fail::<S>("framebuffer cache policy");
            }
        }
        if let Err(what) = S::after_resources(&ctx, &mut silicon.state) {
            fail::<S>(what);
        }
        Stage {
            machine,
            phase: Enumerated { silicon, pci },
        }
    }
}

impl<S: Soc> Stage<S, Enumerated<S>> {
    /// The allocated PCI devices, for board-owned device initialization.
    pub fn pci(&self) -> &fstart_pci::PciEcam {
        &self.phase.pci
    }

    /// Publish the framebuffer a board's display driver initialized.
    pub fn set_framebuffer(&mut self, framebuffer: fstart_core::services::FramebufferInfo) {
        self.phase.silicon.framebuffer = Some(framebuffer);
    }

    /// The SoC's ready-to-boot, then the display ([`Soc::init_display`]).
    /// Graphics POST may patch the ROM and contribute its ACPI; everything
    /// the tables describe is ready afterwards.
    pub fn ready_to_boot(self) -> Stage<S, Ready<S>> {
        let Stage {
            machine,
            phase: Enumerated { mut silicon, pci },
        } = self;
        let ctx = machine.ctx();
        if let Err(what) = S::ready_to_boot(&ctx, &mut silicon.state, &pci) {
            fail::<S>(what);
        }
        if let Err(what) = S::init_display(&mut silicon, &ctx, &pci) {
            fail::<S>(what);
        }
        Stage {
            machine,
            phase: Ready { silicon, pci },
        }
    }
}

impl<S: Soc> Stage<S, Ready<S>> {
    /// Finish the SoC's ready-to-boot, then install ACPI with the board's
    /// contribution `acpi` and SMBIOS with its identity `smbios`.
    pub fn emit_tables(
        self,
        smbios: &crate::acpi::SmbiosIdentity<'_>,
        acpi: crate::acpi::Board,
    ) -> Stage<S, Published<S>> {
        let Stage {
            machine,
            phase: Ready { mut silicon, pci },
        } = self;
        let ctx = machine.ctx();
        S::end_of_post(&ctx, &mut silicon.state);
        let tables = S::tables(&ctx, &mut silicon, &pci, &machine.flow, smbios, acpi);
        #[cfg(feature = "payload-uefi-basic")]
        let peripherals = S::peripherals(&ctx, &machine.config);
        Stage {
            machine,
            phase: Published {
                silicon,
                pci,
                tables,
                #[cfg(feature = "payload-uefi-basic")]
                peripherals,
            },
        }
    }
}

impl<S: Soc> Stage<S, Published<S>> {
    /// The SoC's locks after its last fabric write, RAM mapped for the
    /// payload, the APs parked, the PSP's BootDone, then the payload. (A
    /// SoC whose PSP wants BootDone before the APs are parked sends it with
    /// its locks.)
    pub fn boot(self) -> ! {
        let Stage {
            machine,
            phase:
                Published {
                    mut silicon,
                    pci,
                    tables,
                    #[cfg(feature = "payload-uefi-basic")]
                    peripherals,
                },
        } = self;
        let ctx = machine.ctx();
        if let Err(what) = S::finalize(&ctx, &mut silicon.state) {
            fail::<S>(what);
        }
        post(0x43);

        if S::map_payload_ram(&machine.e820, &pci).is_err() {
            fail::<S>("page tables");
        }
        if !fstart_arch::x86::mp::park_aps_for_payload() {
            fail::<S>("parking the APs");
        }
        if let Err(what) = S::boot_done(&ctx, &mut silicon.state) {
            fail::<S>(what);
        }
        fstart_log::info!("{}: starting the payload", S::NAME);
        post(0x44);
        use fstart_payload::{AcpiTables, Description, Handoff, Placement, Range};
        let ffs = &machine.ffs;
        let mut description = Description::new(
            machine.e820.entries(),
            Range::new(ffs.start, ffs.end - ffs.start),
        );
        description.console = Some(machine.console);
        description.acpi = tables.rsdp.map(AcpiTables::at);
        let acpi_s5 = [tables.s5];
        description.acpi_s5 = &acpi_s5;
        description.smbios = tables.smbios;
        description.acpi_devices = &silicon.devices;
        description.framebuffer = silicon.framebuffer;
        description.pci_root = Some(S::pci_segment(&machine.hw));
        #[allow(unused_mut)]
        let mut handoff = Handoff::new(description, Placement::default());
        #[cfg(feature = "payload-uefi-basic")]
        {
            handoff.peripherals = peripherals;
        }
        S::launch(
            handoff,
            &ctx,
            &silicon.state,
            &machine.flow,
            &machine.options,
        )
    }
}

/// PCI enumeration and resource allocation below every root bridge of the
/// SoC, each from what the fabric routes to it.
fn enumerate<S: Soc>(ctx: &Ctx<'_, S>, silicon: &Silicon<S>) -> PciEcam {
    let Some(roots) = S::roots(ctx, &silicon.state) else {
        fail::<S>("PCI root")
    };
    let mut ecam = match PciEcam::new(roots) {
        Ok(ecam) => ecam,
        Err(_) => fail::<S>("PCI root"),
    };
    if ecam.enumerate_and_allocate().is_err() {
        fail::<S>("PCI enumeration");
    }
    for address in ecam.devices() {
        if let Some(device) = ecam.device(address) {
            let id = device.read32(0);
            let class = device.read32(8) >> 8;
            fstart_log::info!(
                "PCI {}:{}.{} {} class {}",
                Hex(address.bus().into()),
                Hex(address.device().into()),
                address.function(),
                Hex(id.into()),
                Hex(class.into())
            );
        }
    }
    ecam
}
