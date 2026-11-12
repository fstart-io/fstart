//! SP3 boot phases called by the board's stage `main`.
//!
//! Phases from the PSP's release of the BSP to the payload, which the
//! board's stage `main` runs with its own work in between: [`start`]
//! identifies the CPU ([`Identified`]), then the FCH reset tasks
//! ([`Identified::init_fch`]), the console and silicon up to DXIO
//! ([`Early::console`]), the board's GPIO table ([`Stage::program_gpio`]),
//! DXIO with the board's lanes, the chipset, the CPUs, PCI, the tables and
//! the payload. Each phase consumes the one before ([`Stage`]'s phase
//! parameter). The flow is here; the silicon steps it runs are the
//! driver's (`fstart_driver_amd::sp3::soc`: Naples, and Rome and Milan
//! with the Milan deltas inline). One reset image carries every family
//! the board supports:
//! [`start`] returns the running one ([`Start`]); a
//! generation this build does not carry halts at POST 13 before the
//! console (its FCH bring-up is not compiled in), one the board does not
//! list halts after logging at step 13.
//!
//! Every step writes a POST code to port 80h and logs its name; a failing
//! step logs and halts (the last POST code shows where).
//!
//! | POST | Step |
//! |---|---|
//! | 10 | ECAM |
//! | 11 | FCH reset tasks, board LPC/SIO |
//! | 12 | console |
//! | 13 | CPU identity |
//! | 14 | APOB |
//! | 15 | memory map, MTRRs |
//! | 16 | TPM: measured boot starts (`Board::DISCRETE_TPM`) |
//! | 20/21 | DF plan + maps / DF init |
//! | 30/31/32 | NBIO / SMU / FCH secondaries and USB |
//! | 38 | DXIO |
//! | 40 | FCH env, board GPIO |
//! | 48 | boot media (the FFS, with the microcode MP loads) |
//! | 50 | MP + CCX |
//! | 58 | SMM |
//! | 60 | PCI enumeration |
//! | 70 | ACPI |
//! | 80 | ReadyToBoot |
//! | 90 | payload |

extern crate alloc;

use fstart_amdfw_format::anchor::FlashAnchor;
use fstart_amdfw_format::directory::{BiosEntry, Levels, bios};
use fstart_arch::x86::cpu::amd::msr::{SYSCFG, Syscfg, TOP_MEM, TOP_MEM2, TopMem, TopMem2};
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_arch::x86::mp::MpHandle;
use fstart_core::description::PciRootInfo;
use fstart_core::services::memory_detect::{E820Kind, E820State};
use fstart_driver_amd::apob::sysmap::{self, Region};
use fstart_driver_amd::apob::{self, Apob};
use fstart_driver_amd::dxio::wire::MAX_ENGINES;
use fstart_driver_amd::nbio::iohc::DramTops;
use fstart_driver_amd::sp3::dxio::lanes::Engine;
use fstart_driver_amd::sp3::fch::rom3::{self, FlashMap, Rom3};
use fstart_driver_amd::sp3::fch::{self, FchHw};
use fstart_driver_amd::sp3::soc::steps::{self, Steps};
use fstart_driver_amd::sp3::soc::{self, Generation, Identity};
use fstart_driver_amd::sp3::topology::MAX_ROOT_BRIDGES;
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::StaticConsole;
use tock_registers::interfaces::Readable;
use zerocopy::IntoBytes;

use crate::board::{Board, Gpio, Policy};
use crate::layout::{self, Span};
use crate::memory::{self, TopOfMemory};
use fstart_driver_amd::sp3::fch::gpio::Pad;
use fstart_driver_amd::sp3::soc::hw::{self, PrimaryFch, Tsc, msr, post};

mod handoff;
mod options;
mod tpm;

#[cfg(feature = "naples")]
pub use fstart_driver_amd::sp3::soc::naples::Silicon as NaplesSilicon;
#[cfg(any(feature = "rome", feature = "milan"))]
pub use fstart_driver_amd::sp3::soc::rome_milan::Silicon as RomeMilanSilicon;

#[cfg(not(any(feature = "naples", feature = "rome", feature = "milan")))]
compile_error!("a stage build enables at least one of naples, rome, milan");

/// Arm (or park) the AP path of the reset entry, which the SMU releases
/// the APs into.
fn route_aps(route: crate::entry::ApRoute) {
    #[cfg(all(target_os = "none", feature = "bundle-stage"))]
    crate::entry::set_ap_route(route);
    #[cfg(not(all(target_os = "none", feature = "bundle-stage")))]
    let _ = route;
}

/// Halt after logging a failed step; the POST code of the step stays on
/// port 80h.
pub(crate) fn fail(step: &str) -> ! {
    fstart_log::error!("sp3: {} failed, halting", step);
    fstart_log::flush();
    fstart_arch::x86::boot::halt()
}

/// `Result`/`Option` unwrapping that halts with the step's name.
pub(crate) trait OrHalt<T> {
    fn or_halt(self, step: &str) -> T;
}

impl<T, E> OrHalt<T> for Result<T, E> {
    fn or_halt(self, step: &str) -> T {
        self.unwrap_or_else(|_| fail(step))
    }
}

impl<T> OrHalt<T> for Option<T> {
    fn or_halt(self, step: &str) -> T {
        self.unwrap_or_else(|| fail(step))
    }
}

/// Announce a step on port 80h and in the log.
pub(crate) fn step(code: u8, name: &str) {
    post(code);
    fstart_log::info!("sp3: [{:#x}] {}", code, name);
}

/// What the flow hands from phase to phase: the silicon steps' state
/// ([`fstart_driver_amd::sp3::soc::Ctx`]) and the flow's own.
pub(crate) struct Ctx {
    pub soc: soc::Ctx,
    /// The FFS in the CPU view (ROM3), from the flash anchor.
    pub ffs: Span,
    /// The FFS's flash offset, for the payload's settings storage.
    #[cfg(feature = "payload-uefi")]
    pub ffs_offset: u32,
    /// The settings store ([`layout::NVRAM`]) in the CPU view, if a ROM
    /// window shows it.
    pub nvram: Option<&'static [u8]>,
    /// The outcome of every option knob resolved so far.
    pub options: fstart_options::Report,
    pub tseg: Span,
    pub e820: E820State,
    pub smm: Option<crate::smm::SmmConfig>,
    pub rsdp: Option<u64>,
    pub smbios: Option<u64>,
}

/// CPUID → revision before anything else: the generation picks the flow's
/// silicon steps. An unknown CPU or a non-SP3 package halts at POST 13
/// (SOC_DF §2.3: no "unknown stepping = first row"); there is no console
/// yet, as bringing it up already needs the generation's FCH code.
pub(crate) fn identify() -> Identity {
    let signature = fstart_arch::x86::cpuid(1).0;
    let ext_ebx = fstart_arch::x86::cpuid(0x8000_0001).1;
    soc::identify(signature, ext_ebx).unwrap_or_else(|_| unsupported_cpu())
}

/// The CPU is not one this build carries: halt at POST 13 without a
/// console.
pub(crate) fn unsupported_cpu() -> ! {
    post(0x13);
    fstart_arch::x86::boot::halt()
}

/// The silicon steps of one family, generic over in a board's flow:
/// `fn flow<S: Silicon>(id: Identified<Board, S>) -> !`. Sealed; only this
/// crate's families implement it.
#[allow(private_bounds)]
pub trait Silicon: Steps {}
impl<S: Steps> Silicon for S {}

/// The running CPU's family, with its silicon steps: what [`start`]
/// returns. A board matches the families it carries and calls its flow
/// with each; a build carries only the families its features enable.
pub enum Start<B> {
    #[cfg(feature = "naples")]
    Naples(Identified<B, NaplesSilicon>),
    /// Rome and Milan share the SP3 steps (Milan deltas inline).
    #[cfg(any(feature = "rome", feature = "milan"))]
    RomeMilan(Identified<B, RomeMilanSilicon>),
}

/// Identify the CPU and bring up ECAM. A CPU this build does not carry
/// halts at POST 13 before the console (its FCH bring-up is not compiled
/// in).
pub fn start<B: Board>() -> Start<B> {
    crate::stage_runtime::anchor::initial_stage();
    let identity = identify();
    match identity.generation() {
        #[cfg(feature = "naples")]
        Generation::Naples => Start::Naples(Identified::enter(identity)),
        #[cfg(feature = "rome")]
        Generation::Rome => Start::RomeMilan(Identified::enter(identity)),
        #[cfg(feature = "milan")]
        Generation::Milan => Start::RomeMilan(Identified::enter(identity)),
        #[allow(unreachable_patterns)]
        _ => unsupported_cpu(),
    }
}

/// The CPU is identified and ECAM is up; the board picks its silicon
/// policy for the generation next.
pub struct Identified<B, S> {
    identity: Identity,
    pci_root: PciRootInfo,
    timer: Tsc,
    fch: PrimaryFch,
    _flow: core::marker::PhantomData<(B, S)>,
}

impl<B: Board, S: Silicon> Identified<B, S> {
    fn enter(identity: Identity) -> Self {
        step(0x10, "ECAM");
        let pci_root = hw::enable_ecam(identity.generation());
        let timer = hw::tsc();
        let fch = hw::primary_fch(timer);
        Self {
            identity,
            pci_root,
            timer,
            fch,
            _flow: core::marker::PhantomData,
        }
    }

    /// The running CPU's generation.
    pub fn generation(&self) -> Generation {
        self.identity.generation()
    }

    /// Socket 0's FCH reset tasks with the board's silicon `policy`.
    pub fn init_fch(self, policy: Policy) -> Early<B, S> {
        step(0x11, "FCH reset tasks");
        let resume = steps::fch_reset(&self.fch, self.generation(), &policy)
            .unwrap_or_else(|what| fail(what));
        Early {
            identity: self.identity,
            pci_root: self.pci_root,
            timer: self.timer,
            fch: self.fch,
            policy,
            resume,
            _flow: core::marker::PhantomData,
        }
    }
}

/// After socket 0's FCH reset tasks. The board opens its LPC decodes and
/// sets up its Super I/O or BMC next.
pub struct Early<B, S> {
    identity: Identity,
    pci_root: PciRootInfo,
    timer: Tsc,
    fch: PrimaryFch,
    policy: Policy,
    resume: bool,
    _flow: core::marker::PhantomData<(B, S)>,
}

impl<B: Board, S: Silicon> Early<B, S> {
    /// The running CPU's generation.
    pub fn generation(&self) -> Generation {
        self.identity.generation()
    }

    /// Socket 0's FCH, for the board's LPC decodes.
    pub fn fch(&self) -> &PrimaryFch {
        &self.fch
    }

    /// Stop the boot after logging `what`.
    pub fn fail(&self, what: &str) -> ! {
        fail(what)
    }

    /// The console on `config`, then everything up to DXIO: CPU identity,
    /// APOB, the memory map, the TPM, the DF, NBIO, the SMU and the
    /// secondary FCHs.
    pub fn console(
        self,
        slot: &'static StaticConsole<Ns16550>,
        config: fstart_driver_uart::ns16550::Ns16550Config,
    ) -> Stage<B, S, Mapped> {
        let Self {
            identity,
            pci_root,
            timer,
            fch,
            mut policy,
            resume,
            ..
        } = self;
        let generation = identity.generation();

        step(0x12, "console");
        let mut console = Ns16550::new(config).or_halt("console");
        console.init().or_halt("console");
        let console = slot.install(console);
        fstart_log::info!("sp3: {} ({}) console ready", B::NAME, generation.name());
        if resume {
            // S3 needs the resume path (boot scripts, wake vector); not built.
            fstart_log::error!("sp3: S3 resume is not supported, cold booting");
        }

        step(0x13, "CPU identity");
        check_identity::<B>(&identity, timer.mhz());

        step(0x14, "APOB");
        let Flash {
            efs: flash,
            ffs,
            #[cfg(feature = "payload-uefi")]
            ffs_offset,
            size: rom_size,
            nvram,
        } = flash(&fch, generation);
        let (apob, apob_span) = read_apob(flash, generation);
        // The console level first, then the policy overrides, before the
        // first step that reads the policy after the FCH reset tasks (the
        // DF plan).
        let mut options = fstart_options::Report::new();
        let mut level = fstart_log::max_level();
        options::resolve(
            nvram,
            &fstart_log::option::KNOBS,
            &mut level,
            &mut options,
            |_| Ok(()),
        );
        // SAFETY: BSP only, before MP.
        unsafe { fstart_log::set_max_level(level) };
        options::resolve(nvram, B::OPTIONS, &mut policy, &mut options, |_| Ok(()));

        step(0x15, "memory map");
        let tops = top_of_memory();
        let regions = memory_regions(&apob, generation);
        let tseg = crate::smm::place_tseg(
            &regions,
            tops.tom,
            crate::smm::TSEG_SIZE,
            &[layout::CACHE_WINDOW, apob_span],
        )
        .or_halt("TSEG placement");
        let e820 = build_e820(generation, &regions, tops, apob_span, tseg);
        cache_ram(&regions);

        let mut ctx = Ctx {
            soc: soc::Ctx::new(
                identity,
                timer,
                pci_root,
                fch,
                policy,
                apob,
                DramTops {
                    tom: tops.tom,
                    tom2: tops.tom2,
                },
            ),
            ffs,
            #[cfg(feature = "payload-uefi")]
            ffs_offset,
            nvram,
            options,
            tseg,
            e820,
            smm: None,
            rsdp: None,
            smbios: None,
        };

        if B::DISCRETE_TPM {
            step(0x16, "TPM");
            tpm::start(flash, generation);
        }

        step(0x20, "DF topology and maps");
        steps::data_fabric::<S>(&mut ctx.soc).unwrap_or_else(|what| fail(what));
        step(0x21, "DF init");
        S::df_init(&mut ctx.soc).unwrap_or_else(|what| fail(what));

        step(0x30, "NBIO early");
        let mut si = S::nbio_early(&mut ctx.soc).unwrap_or_else(|what| fail(what));
        step(0x31, "SMU");
        S::smu_init(&mut ctx.soc, &mut si).unwrap_or_else(|what| fail(what));
        step(0x32, "FCH secondaries, USB");
        S::fch_secondaries(&mut ctx.soc, &si).unwrap_or_else(|what| fail(what));

        Stage {
            machine: Machine {
                ctx,
                si,
                console,
                rom_size,
            },
            phase: Mapped,
            _board: core::marker::PhantomData,
        }
    }
}

/// What every phase after the console works on.
struct Machine<S> {
    ctx: Ctx,
    si: S,
    console: &'static Ns16550,
    #[cfg_attr(not(feature = "acpi"), allow(dead_code))]
    rom_size: u32,
}

/// The stage after the console, in phase `P`.
pub struct Stage<B, S, P> {
    machine: Machine<S>,
    phase: P,
    _board: core::marker::PhantomData<B>,
}

/// The silicon up to DXIO. The board reads its detect pads, then programs
/// its GPIO table.
pub struct Mapped;

/// The board's GPIOs are programmed. It picks its DXIO lane map (from the
/// pad levels, its BMC or I2C devices) next.
pub struct Configured;

/// The links are trained.
pub struct Trained;

/// The FCH environment is set and the FFS is mounted.
pub struct Chipset;

/// The CPUs are up and SMM is installed.
pub struct Cpus {
    mp: MpHandle,
}

/// PCI is enumerated and its resources are allocated.
pub struct Enumerated {
    #[cfg_attr(not(feature = "acpi"), allow(dead_code))]
    mp: MpHandle,
    pci: fstart_pci::PciEcam,
}

/// The ACPI and SMBIOS tables are installed.
pub struct Published {
    pci: fstart_pci::PciEcam,
}

/// The board's contributions to the tables the payload gets.
#[cfg(feature = "acpi")]
pub struct Tables<'a> {
    /// Board identity only. CPU packages, DIMMs and memory ranges are
    /// discovered by shared drivers.
    pub smbios: fstart_acpi::smbios::SmbiosIdentity<'a>,
    pub acpi: crate::acpi::Board<'a>,
}

impl<B: Board, S: Silicon, P> Stage<B, S, P> {
    /// The running CPU's generation.
    pub fn generation(&self) -> Generation {
        self.machine.ctx.soc.generation
    }

    /// Socket 0's FCH.
    pub fn fch(&self) -> &PrimaryFch {
        &self.machine.ctx.soc.fch
    }

    /// The calibrated clock, to pace board device transactions.
    pub fn timer(&self) -> &dyn fstart_core::services::Timer {
        &self.machine.ctx.soc.timer
    }

    /// Stop the boot after logging `what`.
    pub fn fail(&self, what: &str) -> ! {
        fail(what)
    }

    /// Apply the stored overrides of `knobs` to the board's `config`
    /// (`fstart_options`), before the board uses it. `check` is the whole-set
    /// validation of the consumer: if the overridden configuration fails
    /// it, `config` keeps the board's values and the overrides are reported
    /// as rejected. The knobs are shown in the payload's settings menu.
    pub fn resolve_options<C: Clone>(
        &mut self,
        knobs: &'static [fstart_options::Knob<C>],
        config: &mut C,
        check: impl FnOnce(&C) -> Result<(), &'static str>,
    ) {
        let ctx = &mut self.machine.ctx;
        options::resolve(ctx.nvram, knobs, config, &mut ctx.options, check);
    }

    fn into_phase<Q>(self, next: Q) -> Stage<B, S, Q> {
        Stage {
            machine: self.machine,
            phase: next,
            _board: core::marker::PhantomData,
        }
    }
}

impl<B: Board, S: Silicon> Stage<B, S, Mapped> {
    /// Configure and read the board's detect pads (M.2 SATA/PCIe detect,
    /// straps), in order, before its GPIO table.
    pub fn read_pads(&self, pads: &[Pad]) -> heapless::Vec<bool, 8> {
        step(0x38, "DXIO");
        let acpi = self.machine.ctx.soc.fch.acpi();
        pads.iter()
            .map(|pad| {
                fch::gpio::configure(acpi, pad);
                fch::gpio::read(acpi, pad.pin)
            })
            .collect()
    }

    /// The board GPIO program: the OEM pads, then the CPM table. The vendor
    /// applies both in PEI before the PCIe topology reaches DXIO
    /// (BOARD_S8030 §2 steps 3–5), so board enables and resets are in place
    /// when the links train.
    pub fn program_gpio(self, gpio: &Gpio) -> Stage<B, S, Configured> {
        let acpi = self.machine.ctx.soc.fch.acpi();
        fch::gpio::configure_all(acpi, gpio.early);
        fch::gpio::configure_all(acpi, gpio.table);
        self.into_phase(Configured)
    }
}

impl<B: Board, S: Silicon> Stage<B, S, Configured> {
    /// DXIO with the board's `engines`.
    pub fn train_dxio(
        mut self,
        engines: &heapless::Vec<Engine<'static>, MAX_ENGINES>,
    ) -> Stage<B, S, Trained> {
        let Machine { ctx, si, .. } = &mut self.machine;
        S::dxio(&mut ctx.soc, si, engines).unwrap_or_else(|what| fail(what));
        self.into_phase(Trained)
    }
}

impl<B: Board, S: Silicon> Stage<B, S, Trained> {
    /// The FCH environment, then the boot media: the FFS with the microcode
    /// MP loads.
    pub fn init_chipset(self) -> Stage<B, S, Chipset> {
        let Machine { ctx, si, .. } = &self.machine;
        step(0x40, "FCH env");
        steps::fch_env(&ctx.soc);
        S::fch_env(&ctx.soc, si).unwrap_or_else(|what| fail(what));

        step(0x48, "boot media");
        handoff::mount_boot_media(ctx);
        self.into_phase(Chipset)
    }
}

impl<B: Board, S: Silicon> Stage<B, S, Chipset> {
    /// MP and the CCX, then SMM.
    pub fn start_cpus(mut self) -> Stage<B, S, Cpus> {
        let Machine { ctx, si, .. } = &mut self.machine;
        step(0x50, "MP and CCX");
        let mp = S::mp(
            &mut ctx.soc,
            si,
            B::MAX_CPUS,
            fstart_stage::microcode_blob(),
            &route_aps,
        )
        .unwrap_or_else(|what| fail(what));

        step(0x58, "SMM");
        install_smm(ctx, si, &mp, B::BMC_SX_NOTIFY);
        self.into_phase(Cpus { mp })
    }
}

impl<B: Board, S: Silicon> Stage<B, S, Cpus> {
    /// PCI enumeration and resource allocation.
    pub fn scan_bus(mut self) -> Stage<B, S, Enumerated> {
        let Machine { ctx, si, .. } = &mut self.machine;
        step(0x60, "PCI enumeration");
        S::pci_pre(&mut ctx.soc, si).unwrap_or_else(|what| fail(what));
        let pci = crate::flow::enumerate(ctx);
        S::pci_post(&ctx.soc, si).unwrap_or_else(|what| fail(what));
        let Stage {
            machine,
            phase: Cpus { mp },
            _board,
        } = self;
        Stage {
            machine,
            phase: Enumerated { mp, pci },
            _board,
        }
    }
}

impl<B: Board, S: Silicon> Stage<B, S, Enumerated> {
    /// ACPI and SMBIOS with the board's `tables`.
    pub fn emit_tables(
        mut self,
        #[cfg(feature = "acpi")] tables: Tables<'_>,
    ) -> Stage<B, S, Published> {
        step(0x70, "ACPI");
        #[cfg(feature = "acpi")]
        {
            let Machine {
                ctx, si, rom_size, ..
            } = &mut self.machine;
            ctx.rsdp = Some(handoff::acpi::<B, S>(
                ctx,
                si,
                &self.phase.pci,
                &tables.acpi,
            ));
            ctx.smbios = Some(crate::smbios::runtime::prepare(
                ctx,
                &self.phase.mp,
                *rom_size,
                tables.smbios,
            ));
        }
        let Stage {
            machine,
            phase: Enumerated { pci, .. },
            _board,
        } = self;
        Stage {
            machine,
            phase: Published { pci },
            _board,
        }
    }
}

impl<B: Board, S: Silicon> Stage<B, S, Published> {
    /// ReadyToBoot, park the APs, then the payload.
    pub fn boot(self) -> ! {
        let Stage {
            machine: Machine {
                ctx, si, console, ..
            },
            phase: Published { pci },
            ..
        } = self;
        step(0x80, "ReadyToBoot");
        steps::ready_to_boot(&ctx.soc, &si).unwrap_or_else(|what| fail(what));

        step(0x90, "payload");
        if !fstart_arch::x86::mp::park_aps_for_payload() {
            fail("AP park");
        }
        handoff::boot(ctx, pci, console)
    }
}

/// With the console up: the BSP BIST and whether the board supports the
/// CPU's generation (the build may carry more generations than a board
/// lists).
fn check_identity<B: Board>(identity: &Identity, tsc_mhz: u32) {
    #[cfg(all(target_os = "none", feature = "bundle-stage"))]
    if crate::entry::bsp_bist() != 0 {
        fstart_log::error!("sp3: BSP BIST {:#x}", crate::entry::bsp_bist());
        fail("BIST");
    }
    let generation = identity.generation();
    fstart_log::info!(
        "sp3: CPU {} ({:#x}), TSC {} MHz",
        identity.revision.name(),
        identity.signature,
        tsc_mhz
    );
    if !B::GENERATIONS.contains(&generation) {
        fstart_log::error!(
            "sp3: {} does not support {} CPUs",
            B::NAME,
            generation.name()
        );
        fail("CPU identity");
    }
}

/// The SP3 flash anchor patch site: FFS flash offset and size and the
/// part size, filled by the image builder (`sp3_flash::patch_reset_image`).
#[repr(transparent)]
struct AnchorSite(core::cell::UnsafeCell<FlashAnchor>);

// SAFETY: only the image builder writes it; the reset image only reads.
unsafe impl Sync for AnchorSite {}

#[used]
static FLASH_ANCHOR: AnchorSite =
    AnchorSite(core::cell::UnsafeCell::new(FlashAnchor::placeholder()));

/// Physical memory the identity map covers, read byte-wise.
fn read_phys(address: u64, buf: &mut [u8]) {
    buf.iter_mut().zip(address..).for_each(|(b, a)| {
        // SAFETY: flash windows (ROM2, ROM3) only: read-only device memory,
        // identity-mapped by the reset image's page tables.
        *b = unsafe { core::ptr::read_volatile(a as *const u8) }
    });
}

/// `len` bytes of flash windows at `address` as a slice.
fn flash_slice(address: u64, len: u32) -> &'static [u8] {
    // SAFETY: a ROM2/ROM3 range `FlashMap::address` vouched for; nothing
    // writes flash during boot.
    unsafe { core::slice::from_raw_parts(address as *const u8, len as usize) }
}

/// What [`flash`] found.
struct Flash {
    /// The booted half (from its EFS).
    efs: &'static [u8],
    /// The FFS in the CPU view, and its flash offset.
    ffs: Span,
    #[cfg(feature = "payload-uefi")]
    ffs_offset: u32,
    /// The size of the part.
    size: u32,
    /// The settings store, if a window shows it.
    nvram: Option<&'static [u8]>,
}

/// The SPI flash as this boot sees it (`fch::rom3`): the EFS view (the
/// half whose EFS the generation's boot ROM takes), the FFS window and the
/// settings store. They are read through ROM2 when they lie in the page it
/// shows; ROM3 is probed only for an FFS outside it, and an EFS or FFS that
/// neither shows halts.
fn flash(fch: &PrimaryFch, generation: Generation) -> Flash {
    // SAFETY: the static is always readable; volatile so the linked
    // placeholder is not folded into the code.
    let anchor = unsafe { core::ptr::read_volatile(FLASH_ANCHOR.0.get()) };
    let anchor = FlashAnchor::parse(anchor.as_bytes()).or_halt("flash anchor");
    let page = rom3::rom_page(fch);
    let (offset, size) = (anchor.ffs_offset.get(), anchor.ffs_size.get());
    // ROM2 first: ROM3 is read only if the FFS is outside ROM2's page (the
    // EFS of the booted half always is in it, AMD #55758 §4.1.7).
    let rom2 = FlashMap::rom2(page, anchor.flash_size.get());
    let map = if rom2.address(offset, size).is_some() {
        rom2
    } else {
        FlashMap::probe(page, anchor.flash_size.get(), read_phys)
    };
    fstart_log::info!(
        "sp3: flash {:#x} bytes, ROM2 page {}, ROM3 {}",
        map.size,
        page,
        match map.rom3 {
            Rom3::Linear => "linear",
            Rom3::Paged => "paged",
            Rom3::Absent => "not used",
        }
    );
    let halves: &[u32] = match generation {
        Generation::Naples => &[0],
        Generation::Rome | Generation::Milan => &[0, rom3::PAGE_SIZE],
    };
    let efs = halves
        .iter()
        .filter(|&&half| half < map.size)
        .find_map(|&half| {
            let len = map.size - half;
            let view = map
                .address(half, len)
                .map(|a| flash_slice(a, len))
                .or_else(|| {
                    Some(flash_slice(
                        map.address(half, rom3::PAGE_SIZE)?,
                        rom3::PAGE_SIZE,
                    ))
                })?;
            generation.flash(view).efs_checked().is_ok().then_some(view)
        })
        .or_halt("EFS");
    let ffs = map.address(offset, size).or_halt("FFS window");
    fstart_log::info!("sp3: FFS at flash {:#x}+{:#x}", offset, size);
    let nvram = map
        .address(layout::NVRAM.offset, layout::NVRAM.size)
        .map(|address| flash_slice(address, layout::NVRAM.size));
    Flash {
        efs,
        ffs: Span::new(ffs, u64::from(size)),
        #[cfg(feature = "payload-uefi")]
        ffs_offset: offset,
        size: anchor.flash_size.get(),
        nvram,
    }
}

/// `generation`'s BIOS directories in the booted half `flash`.
fn bios_directories(
    flash: &'static [u8],
    generation: Generation,
) -> Option<Levels<'static, BiosEntry>> {
    generation
        .flash(flash)
        .bios_directories_checked(generation.profile().bios)
        .ok()
}

/// The APOB from BIOS directory entry 0x61 (PSP_APOB §3.5); logs the ABL
/// event log and the APCB recovery flag.
fn read_apob(flash: &'static [u8], generation: Generation) -> (Apob<'static>, Span) {
    let address = bios_directories(flash, generation)
        .and_then(|dirs| dirs.find(bios::APOB, None, None))
        .map(|(_, entry)| entry.destination())
        .filter(|&d| d != u64::MAX)
        .or_halt("APOB directory entry");
    // SAFETY: the PSP wrote the APOB there before releasing x86; it is
    // reserved in e820 and only read.
    let apob = unsafe { Apob::from_address(address, generation.into()) }.or_halt("APOB header");
    for event in apob::event::all_events(&apob).flatten() {
        let (instance, e) = event;
        fstart_log::info!(
            "apob: event {:#x} class {:#x} info {:#x} data {:#x} {:#x}",
            instance,
            e.class,
            e.info,
            e.data_a,
            e.data_b
        );
    }
    if apob::mem::apcb_boot_info(&apob)
        .and_then(|i| i.recovery())
        .unwrap_or(false)
    {
        fstart_log::warn!("apob: APCB recovery boot");
    }
    let span = Span::new(address, apob.reserved_size());
    (apob, span)
}

/// TOP_MEM / TOM2 as the PSP/ABL left them (PSP_APOB §3.6).
fn top_of_memory() -> TopOfMemory {
    let cpu = msr();
    let tom2 = cpu
        .reg(SYSCFG)
        .is_set(Syscfg::TOM2_EN)
        .then(|| cpu.reg(TOP_MEM2).read(TopMem2::ADDRESS) << 22);
    TopOfMemory {
        tom: cpu.reg(TOP_MEM).read(TopMem::ADDRESS) << 23,
        tom2,
    }
}

fn memory_regions(
    apob: &Apob<'_>,
    generation: Generation,
) -> heapless::Vec<Region, { sysmap::MAX_REGIONS }> {
    let map = sysmap::system_memory_map(apob).or_halt("APOB memory map");
    sysmap::memory_map(generation.into(), map.top, map.holes()).or_halt("memory map")
}

fn build_e820(
    generation: Generation,
    regions: &[Region],
    tops: TopOfMemory,
    apob: Span,
    tseg: Span,
) -> E820State {
    let mut e820 = E820State::new();
    let reservations = memory::standard_reservations(generation, apob, Some(tseg));
    let count = memory::e820(regions, tops, &reservations, e820.entries_mut())
        .unwrap_or_else(|_| fail("e820"));
    let ram: u64 = e820.entries_mut()[..count]
        .iter()
        .filter(|e| e.kind == E820Kind::Ram as u32)
        .map(|e| e.size)
        .sum();
    e820.set_detected(count, ram).or_halt("e820");
    for e in e820.entries() {
        let (addr, size, kind) = (e.addr, e.size, e.kind);
        fstart_log::info!("e820: {:#x}+{:#x} type {}", addr, size, kind);
    }
    fstart_log::info!("sp3: {} MiB RAM", ram >> 20);
    e820
}

/// MTRRs: RAM below 4 GiB write-back by variable MTRRs; above 4 GiB
/// through SYSCFG.Tom2ForceMemTypeWB (the AMD way: the variable MTRRs
/// cannot cover TiBs of DRAM). The reset image's 16 MiB window is RAM too.
fn cache_ram(regions: &[Region]) {
    use fstart_arch::x86::cpu::amd::msr::Syscfg;
    use tock_registers::interfaces::ReadWriteable;
    let low = regions
        .iter()
        .filter(|r| r.kind.is_ram() && r.base < memory::FOUR_GIB)
        .map(|r| (r.base, r.end().min(memory::FOUR_GIB) - r.base));
    fstart_arch::x86::mtrr::set_ram_wb_ranges_from(low).or_halt("MTRR ranges");
    // SAFETY: BSP only, before any AP runs.
    unsafe { fstart_arch::x86::mtrr::setup_ram_wb() }.or_halt("MTRRs");
    // SAFETY: BSP only, caching enabled; SYSCFG.MtrrFixDramModEn is clear
    // (amd_psp_reset.S), so the fixed MTRRs' DRAM routing is hidden again after.
    unsafe { fstart_arch::x86::cpu::amd::mtrr::route_fixed_wb_to_dram(&msr()) };
    msr().reg(SYSCFG).modify(Syscfg::TOM2_WB::SET);
}

/// Select and reserve scratch pages using the live routing map. This runs
/// before payload loading and before the immutable SMM config is installed.
fn plan_einj(ctx: &mut Ctx) -> Option<crate::smm::EinjConfig> {
    #[cfg(feature = "rome")]
    if ctx.soc.generation == Generation::Rome && (1..=2).contains(&ctx.soc.topology.sockets) {
        use fstart_driver_amd::{
            df::Target,
            psp::ras,
            smn::Smn,
            sp3::df::{self, v3},
        };
        let mut config = crate::smm::EinjConfig {
            sockets: ctx.soc.topology.sockets,
            ..Default::default()
        };
        for socket in 0..config.sockets {
            config.df[usize::from(socket)] = fstart_driver_amd::df::node_address(socket, 4);
            config.buses[usize::from(socket)] = ctx
                .soc
                .topology
                .root_bridges
                .iter()
                .find(|r| r.socket == socket && r.index == 0)?
                .bus_base;
        }
        let map = df::dram::read::<v3::Df3, _>(&ctx.soc.df, 0, Target::Instance(v3::CCM0));
        config.targets =
            crate::einj::plan_targets(config.sockets, ctx.e820.entries(), &map.rules, |address| {
                ras::memory_target(
                    address,
                    config.sockets,
                    |socket, instance, reg| unsafe {
                        ctx.soc
                            .df
                            .reg(
                                socket,
                                if instance == ras::BROADCAST {
                                    Target::Node
                                } else {
                                    Target::Instance(instance)
                                },
                                reg,
                            )
                            .get()
                    },
                    |socket, address| unsafe {
                        Smn::on_bus(ctx.soc.pci, config.buses[usize::from(socket)]).read32(address)
                    },
                )
            });
        for (i, &target) in config.targets.iter().enumerate().filter(|(_, t)| **t != 0) {
            let page = target & !(crate::einj::SCRATCH_SIZE - 1);
            if config.targets[..i]
                .iter()
                .any(|&t| t != 0 && t & !(crate::einj::SCRATCH_SIZE - 1) == page)
            {
                continue;
            }
            if page != crate::einj::SCRATCH {
                ctx.e820
                    .reserve_range(page, crate::einj::SCRATCH_SIZE)
                    .or_halt("EINJ scratch reservation");
            }
            // SAFETY: legacy reserved scratch or a fresh page just removed
            // from usable RAM; all DRAM is identity mapped by the reset image.
            unsafe {
                core::ptr::write_bytes(page as *mut u8, 0, crate::einj::SCRATCH_SIZE as usize)
            };
        }
        return Some(config);
    }
    let _ = ctx;
    None
}

/// TSEG, SMBASE and lock on every thread with the FCH handler image, then
/// the PSP SmmInfo registration from SMM (PSP_APOB §3.3). Without an SMM
/// image in this build SMM stays unlocked; that is logged, not hidden.
///
/// Order (AMD PPR 55803 §2.1.12.1, MSRC001_0111..0113, C001_0015): FCH
/// SMIs off; TValid/AValid cleared so TSEG is plain DRAM while the BSP
/// copies the handler and flushes it; then every thread (APs first, BSP
/// last) writes SMM_ADDR, SMM_MASK (TValid, WB), its own SMM_BASE and
/// sets HWCR.SmmLock, which makes all of them read-only. SMM_ADDR and
/// SMM_MASK are per core (both SMT threads write the same values), SMM_BASE
/// and HWCR per thread. Then the FCH sources, one self-SMI per thread
/// through the new entry, the PSP SMI route and SW SMI D0 (SmmInfo).
fn install_smm<S: Steps>(
    ctx: &mut Ctx,
    si: &S,
    mp: &MpHandle,
    bmc: Option<crate::smm::SleepNotify>,
) {
    let config = crate::smm::SmmConfig {
        generation: ctx.soc.generation,
        ecam_base: ctx.soc.pci_root.ecam_base,
        psp_bar: ctx.soc.psp_bar as u32,
        einj: plan_einj(ctx).unwrap_or_default(),
        usb_buses: S::usb_buses(&ctx.soc, si).unwrap_or_else(|what| fail(what)),
        usb_bus_count: S::usb_bus_count(&ctx.soc),
        bmc,
    };
    #[cfg(fstart_sp3_has_smm_image)]
    {
        const IMAGE: &[u8] = include_bytes!(env!("FSTART_SMM_IMAGE"));
        tpm::measure_smm(IMAGE);
        // The handler's page tables map the low 4 GiB only; a PSP BAR
        // above would fault in SMM at SmmInfo, after the lock.
        if ctx.soc.psp_bar == 0 || ctx.soc.psp_bar >= crate::memory::FOUR_GIB {
            fail("PSP BAR for SMM");
        }
        let smi = crate::smm::Sp3Smi {
            fch: &ctx.soc.fch,
            config,
        };
        fstart_arch::x86::cpu::amd::smm::AmdSmm::new("sp3", &smi)
            .install(mp, IMAGE, ctx.tseg.base, ctx.tseg.size as u32)
            .or_halt("SMM install");
        crate::smm::prepare_psp_smi(&ctx.soc.fch);
        fch::smi::software_smi(ctx.soc.fch.io(), crate::smm::sw::PSP_SMM_INFO);
        ctx.smm = Some(config);
        ctx.soc.smm_installed = true;
    }
    #[cfg(not(fstart_sp3_has_smm_image))]
    {
        let _ = (mp, config);
        fstart_log::warn!(
            "sp3: no SMM image in this build; TSEG {:#x} unused, SMM stays unlocked",
            ctx.tseg.base
        );
    }
}

/// Enumerate every root bridge the DF routes, each within its own buses and
/// windows, with one allocator for the segment.
///
/// The root bridges are described here, from the plan [`data_fabric`]
/// programmed: before that the plan is empty and there is nothing to
/// enumerate through an unprogrammed fabric. The allocator keeps them, and
/// the tables are built from its copy.
pub(crate) fn enumerate(ctx: &Ctx) -> fstart_pci::PciEcam {
    let roots: heapless::Vec<fstart_pci::PciRoot, MAX_ROOT_BRIDGES> = (ctx.soc.plan.root_bridges)
        .iter()
        .map(|rb| crate::pci::root(rb, &ctx.soc.pci_root))
        .collect::<Option<_>>()
        .or_halt("PCI root");
    let mut pci = fstart_pci::PciEcam::new(roots).or_halt("PCI root");
    pci.enumerate_and_allocate().or_halt("PCI enumeration");
    for (rb, root) in ctx.soc.plan.root_bridges.iter().zip(pci.roots()) {
        let buses = root.buses();
        fstart_log::info!(
            "pci: rb {}.{}: {} functions",
            rb.socket,
            rb.index,
            pci.devices()
                .filter(|address| buses.contains(&address.bus()))
                .count()
        );
    }
    pci
}
