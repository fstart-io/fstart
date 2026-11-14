//! Naples, Rome and Milan on the flow ([`crate::flow`]).
//!
//! One reset image carries every family the board supports: [`start`]
//! returns the running one ([`Start`]). A CPU this build does not carry
//! halts at POST 13 before the console (its FCH bring-up is not compiled
//! in); one the board does not list halts after logging.
//!
//! A family's silicon steps are the driver's (`crate::sp3::silicon`).
//! [`Sp3`] puts them at the flow's phases ([`Init`]) and adds the flow's
//! services for the socket ([`crate::flow::Soc`]): the flash windows and
//! the APOB behind them, the options, the memory map, the TPM, SMM with
//! RAS and EINJ, and the tables.
//!
//! Every step writes a POST code to port 80h and logs its name; a failing
//! step logs and halts (the last POST code shows where).
//!
//! | POST | Step |
//! |---|---|
//! | 10 | ECAM |
//! | 11 | FCH reset tasks, board LPC/SIO |
//! | 31 | console (the flow's) |
//! | 13 | CPU identity |
//! | 14 | APOB |
//! | 15 | memory map, MTRRs |
//! | 16 | TPM: measured boot starts (`Board::DISCRETE_TPM`) |
//! | 48 | boot media (the FFS, with the microcode MP loads) |
//! | 32 | the memory map is done (the flow's) |
//! | 20/21 | DF plan + maps / DF init |
//! | 30/31/32 | NBIO / SMU / FCH secondaries and USB; board pads and GPIO |
//! | 38 | DXIO |
//! | 40 | FCH env |
//! | 50 | MP + CCX |
//! | 58 | SMM |
//! | 60 | PCI enumeration |
//! | 70 | ACPI |
//! | 80 | ReadyToBoot |
//! | 43/44 | locked / the APs are parked (the flow's) |
//! | 90 | payload |
extern crate alloc;

use core::marker::PhantomData;

use crate::soc::{Entered, Init, Roots};
use crate::sp3::silicon::cpu::Sp3Cpu;
use crate::sp3::silicon::hw::{self, PrimaryFch};
use crate::sp3::silicon::steps::{self, Steps};
use crate::sp3::silicon::{self, Generation, Identity, Policy};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_arch::x86::mp::MpHandle;
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::dxio::lanes::Engine;
use fstart_driver_amd::dxio::wire::MAX_ENGINES;
use fstart_driver_amd::fch::gpio::Pad;
use fstart_driver_amd::fch::hw::FchHw;
use fstart_driver_amd::nbio::iohc::DramTops;
use fstart_driver_amd::psp;

use super::board::{Board, Gpio};
use super::flow::{self as parts, step};
use super::layout;
use crate::flow::{Ctx, Early, Mapped, Reserved, Silicon as Up, Stage, Tables};
use crate::layout::Region;

/// An SP3 family `F` (its silicon steps) on the flow.
pub struct Sp3<F>(PhantomData<F>);
/// Naples: DF2, an NBIO and SMU V9 per die.
pub type Naples = Sp3<silicon::naples::Silicon>;
/// Rome and Milan: DF3, SMU V11 per IOD.
pub type RomeMilan = Sp3<silicon::rome_milan::Silicon>;

/// The silicon steps of one family, which a board's flow is generic over:
/// `fn flow<F: Silicon>(id: Identified<Board, F>) -> !`.
pub trait Silicon: Steps + 'static {}
impl<F: Steps + 'static> Silicon for F {}

/// A board's configuration of the socket: its silicon policy, and what
/// the flow's services read of the board.
pub struct Config {
    pub policy: Policy,
    /// Shown in the log.
    pub name: &'static str,
    pub generations: &'static [Generation],
    pub max_cpus: u16,
    pub discrete_tpm: bool,
    pub bmc_sx_notify: Option<super::smm::SleepNotify>,
    /// The silicon policy knobs the user may override.
    pub options: &'static [fstart_options::Knob<Policy>],
}

/// The handles of the socket's flow: the identified CPU, its ECAM segment,
/// the clock and socket 0's FCH.
pub struct Hw {
    pub identity: Identity,
    pub pci_root: PciRootInfo,
    pub timer: hw::Tsc,
    pub fch: PrimaryFch,
    /// The FCH woke from S3, which is not supported.
    resume: bool,
}

/// What the silicon steps hand on, and the SMM handler's configuration
/// once it is installed.
pub struct State<F> {
    pub soc: silicon::Ctx,
    pub si: F,
    /// EINJ's targets, once their scratch pages are planned.
    einj: Option<super::smm::EinjConfig>,
    smm: Option<super::smm::SmmConfig>,
}

/// What the socket's services keep between the phases.
pub struct Flow {
    /// The FFS in the CPU view (ROM3), from the flash anchor.
    #[cfg(feature = "payload-uefi")]
    ffs: Region,
    /// The FFS's flash offset, for the payload's settings storage.
    #[cfg(feature = "payload-uefi")]
    ffs_offset: u32,
    /// The settings store ([`layout::NVRAM`]) in the CPU view, if a ROM
    /// window shows it.
    nvram: Option<&'static [u8]>,
    /// The outcome of every option knob resolved so far.
    options: fstart_options::Report,
    tseg: Region,
    /// The size of the flash part.
    rom_size: u32,
}

/// The running CPU's family: what [`start`] returns. A board calls its
/// flow with each family it carries and [`Identified::unsupported`] for the
/// others; only the flows it calls are linked.
pub enum Start<B> {
    Naples(Identified<B, silicon::naples::Silicon>),
    /// Rome and Milan share their steps (Milan deltas inline).
    RomeMilan(Identified<B, silicon::rome_milan::Silicon>),
}

/// CPUID → revision before anything else: the generation picks the silicon
/// steps. An unknown CPU or a non-SP3 package halts at POST 13 (SOC_DF
/// §2.3: no "unknown stepping = first row"); there is no console yet, as
/// bringing it up already needs the generation's FCH code.
fn identify() -> Identity {
    let signature = fstart_arch::x86::cpuid(1).0;
    let ext_ebx = fstart_arch::x86::cpuid(0x8000_0001).1;
    silicon::identify(signature, ext_ebx).unwrap_or_else(|_| unsupported_cpu())
}

fn unsupported_cpu() -> ! {
    hw::post(0x13);
    fstart_arch::x86::boot::halt()
}

/// Identify the CPU. A generation the board does not list halts at POST 13.
pub fn start<B: Board>() -> Start<B> {
    let generation = Generation::of(identify().silicon());
    if !B::GENERATIONS.contains(&generation) {
        unsupported_cpu()
    }
    match generation {
        Generation::Naples => Start::Naples(Identified::new(generation)),
        Generation::Rome | Generation::Milan => Start::RomeMilan(Identified::new(generation)),
    }
}

/// The CPU is identified; the board picks its silicon policy for the
/// generation next.
pub struct Identified<B, F> {
    generation: Generation,
    _flow: PhantomData<(B, F)>,
}

impl<B: Board, F: Silicon> Identified<B, F> {
    fn new(generation: Generation) -> Self {
        Self {
            generation,
            _flow: PhantomData,
        }
    }

    /// The running CPU's generation.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// A family the board does not carry: halt at POST 13, before the
    /// console (its FCH code is not linked).
    pub fn unsupported(self) -> ! {
        unsupported_cpu()
    }

    /// ECAM, then socket 0's FCH reset tasks with the board's silicon
    /// `policy`. The board opens its LPC decodes and sets up its Super I/O
    /// or BMC next, then the console.
    pub fn init_fch(self, policy: Policy) -> Early<Sp3<F>> {
        crate::flow::enter(Config {
            policy,
            name: B::NAME,
            generations: B::GENERATIONS,
            max_cpus: B::MAX_CPUS,
            discrete_tpm: B::DISCRETE_TPM,
            bmc_sx_notify: B::BMC_SX_NOTIFY,
            options: B::OPTIONS,
        })
    }
}

impl<F: Silicon> Early<Sp3<F>> {
    /// Socket 0's FCH, for the board's LPC decodes.
    pub fn fch(&self) -> &PrimaryFch {
        &self.hw().fch
    }
}

impl<F: Silicon, P> Stage<Sp3<F>, P> {
    /// The running CPU's generation.
    pub fn generation(&self) -> Generation {
        Generation::of(self.hw().identity.silicon())
    }

    /// Socket 0's FCH.
    pub fn fch(&self) -> &PrimaryFch {
        &self.hw().fch
    }

    /// The calibrated clock, to pace board device transactions.
    pub fn timer(&self) -> &dyn fstart_core::services::Timer {
        &self.hw().timer
    }

    /// Configure and read the board's detect pads (M.2 SATA/PCIe detect,
    /// straps), in order, before its GPIO table.
    pub fn read_pads(&self, pads: &[Pad]) -> heapless::Vec<bool, 8> {
        let acpi = self.fch().acpi();
        pads.iter()
            .map(|pad| {
                fstart_driver_amd::fch::gpio::configure(acpi, pad);
                fstart_driver_amd::fch::gpio::read(acpi, pad.pin)
            })
            .collect()
    }

    /// The board GPIO program: the OEM pads, then the CPM table. The vendor
    /// applies both in PEI before the PCIe topology reaches DXIO
    /// (BOARD_S8030 §2 steps 3–5), so board enables and resets are in place
    /// when the links train.
    pub fn program_gpio(&self, gpio: &Gpio) {
        let acpi = self.fch().acpi();
        fstart_driver_amd::fch::gpio::configure_all(acpi, gpio.early);
        fstart_driver_amd::fch::gpio::configure_all(acpi, gpio.table);
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
        let flow = self.flow_mut();
        parts::options::resolve(flow.nvram, knobs, config, &mut flow.options, check);
    }
}

impl<F: Silicon> Init for Sp3<F> {
    const NAME: &'static str = "sp3";

    type Config = Config;
    type Identity = Identity;
    type Hw = Hw;
    /// The DXIO engines of the board's lanes.
    type Lanes<'a> = &'a heapless::Vec<Engine<'static>, MAX_ENGINES>;
    type Runtime = ();
    type FchUp = State<F>;
    type Trained = State<F>;
    type Chipset = State<F>;
    type Cpu = Sp3Cpu;
    type State = State<F>;

    /// ECAM, then the reset tasks of socket 0's FCH.
    fn enter(config: &Config, _: &NativeMsr) -> Entered<Self> {
        let identity = identify();
        let generation = Generation::of(identity.silicon());
        step(0x10, "ECAM");
        let pci_root = hw::enable_ecam(generation);
        let timer = hw::tsc();
        let fch = hw::primary_fch(timer);
        step(0x11, "FCH reset tasks");
        // There is no console yet; the POST code shows where.
        let resume = steps::fch_reset(&fch, generation.silicon(), &config.policy)
            .unwrap_or_else(|what| parts::fail(what));
        Entered {
            hw: Hw {
                identity,
                pci_root,
                timer,
                fch,
                resume,
            },
            clock: Some(timer),
            pre_init: Ok(()),
        }
    }

    /// Whether the board supports the CPU's generation (the build may
    /// carry more generations than a board lists).
    fn identify(hw: &Hw, config: &Config, _: Option<Tsc>) -> Result<Identity, &'static str> {
        let generation = Generation::of(hw.identity.silicon());
        fstart_log::info!("sp3: {} ({}) console ready", config.name, generation.name());
        if hw.resume {
            // S3 needs the resume path (boot scripts, wake vector); not built.
            fstart_log::error!("sp3: S3 resume is not supported, cold booting");
        }
        step(0x13, "CPU identity");
        fstart_log::info!(
            "sp3: CPU {} ({:#x}), TSC {} MHz",
            hw.identity.revision.name(),
            hw.identity.signature,
            hw.timer.mhz()
        );
        if !config.generations.contains(&generation) {
            fstart_log::error!(
                "sp3: {} does not support {} CPUs",
                config.name,
                generation.name()
            );
            return Err("CPU identity");
        }
        Ok(hw.identity)
    }

    fn pci_segment(hw: &Hw) -> PciRootInfo {
        hw.pci_root
    }

    fn psp(hw: &Hw) -> psp::Variant {
        hw.identity.silicon().into()
    }

    /// The fabric, NBIO, the SMU and the secondary FCHs: everything up to
    /// DXIO. The board reads its detect pads and programs its GPIO table
    /// next.
    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<State<F>, &'static str> {
        let apob = *ctx.memory.apob.ok_or("APOB")?;
        let mut soc = silicon::Ctx::new(
            ctx.hw.identity,
            ctx.hw.timer,
            ctx.hw.pci_root,
            hw::primary_fch(ctx.hw.timer),
            ctx.config.policy,
            apob,
            DramTops {
                tom: ctx.memory.top_mem,
                tom2: ctx.memory.top_mem2,
            },
        );
        step(0x20, "DF topology and maps");
        steps::data_fabric::<F>(&mut soc)?;
        step(0x21, "DF init");
        F::df_init(&mut soc)?;
        step(0x30, "NBIO early");
        let mut si = F::nbio_early(&mut soc)?;
        step(0x31, "SMU");
        F::smu_init(&mut soc, &mut si)?;
        step(0x32, "FCH secondaries, USB");
        F::fch_secondaries(&mut soc, &si)?;
        Ok(State {
            soc,
            si,
            einj: None,
            smm: None,
        })
    }

    fn train_dxio(
        _: &Ctx<'_, Self>,
        mut state: State<F>,
        engines: &heapless::Vec<Engine<'static>, MAX_ENGINES>,
    ) -> Result<State<F>, &'static str> {
        step(0x38, "DXIO");
        F::dxio(&mut state.soc, &state.si, engines)?;
        Ok(state)
    }

    fn init_chipset(_: &Ctx<'_, Self>, state: State<F>, (): ()) -> Result<State<F>, &'static str> {
        step(0x40, "FCH env");
        steps::fch_env(&state.soc);
        F::fch_env(&state.soc, &state.si)?;
        Ok(state)
    }

    fn cpu_driver(
        _: &Ctx<'_, Self>,
        state: &mut State<F>,
        microcode: Option<&'static [u8]>,
    ) -> Result<Sp3Cpu, &'static str> {
        step(0x50, "MP and CCX");
        F::cpu_driver(&mut state.soc, &mut state.si, microcode)
    }

    fn threads(_: &Ctx<'_, Self>, state: &State<F>) -> Result<u16, &'static str> {
        Ok(F::threads(&state.si))
    }

    fn release_aps(_: &Ctx<'_, Self>, state: &State<F>, aps: u16) -> Result<(), u32> {
        F::release_aps(&state.soc, &state.si, aps)
    }

    fn cpu_errors(cpu: &Sp3Cpu) -> u32 {
        cpu.failures()
    }

    fn cpus_up(
        _: &Ctx<'_, Self>,
        mut state: State<F>,
        mp: &MpHandle,
        apic_ids: &[u32],
    ) -> Result<State<F>, &'static str> {
        F::cpus_up(&mut state.soc, &mut state.si, mp, apic_ids)?;
        Ok(state)
    }

    fn before_enumeration(_: &Ctx<'_, Self>, state: &mut State<F>) -> Result<(), &'static str> {
        step(0x60, "PCI enumeration");
        F::pci_pre(&mut state.soc, &mut state.si)
    }

    /// Every root bridge the DF routes, each within its own buses and
    /// windows, from the plan the fabric step programmed.
    fn roots(_: &Ctx<'_, Self>, state: &State<F>) -> Option<Roots> {
        (state.soc.plan.root_bridges.iter())
            .map(|rb| fstart_driver_amd::df::root::planned(rb, &state.soc.pci_root))
            .collect()
    }

    fn after_resources(_: &Ctx<'_, Self>, state: &mut State<F>) -> Result<(), &'static str> {
        F::pci_post(&state.soc, &state.si)
    }

    fn ready_to_boot(
        _: &Ctx<'_, Self>,
        _: &mut State<F>,
        _: &fstart_pci::PciEcam,
    ) -> Result<(), &'static str> {
        Ok(())
    }

    /// ReadyToBoot (SOC_DF §3.7, PSP_APOB §3.4, FCH_SP3 §2.3): the FCH late
    /// tasks, the DF's extended config off, PSP LockDFReg, and BootDone
    /// last, before the APs are parked.
    fn finalize(_: &Ctx<'_, Self>, state: &mut State<F>) -> Result<(), &'static str> {
        step(0x80, "ReadyToBoot");
        steps::ready_to_boot(&state.soc, &state.si)
    }

    /// BootDone is the last command of [`Init::finalize`].
    fn boot_done(_: &Ctx<'_, Self>, _: &mut State<F>) -> Result<(), &'static str> {
        Ok(())
    }
}

impl<F: Silicon> State<F> {
    /// What the tables and the handoff read.
    #[cfg_attr(not(feature = "payload-uefi"), allow(unused_variables))]
    fn view<'a>(&'a self, flow: &'a Flow) -> parts::Ctx<'a> {
        parts::Ctx {
            soc: &self.soc,
            #[cfg(feature = "payload-uefi")]
            ffs: flow.ffs,
            #[cfg(feature = "payload-uefi")]
            ffs_offset: flow.ffs_offset,
            #[cfg(feature = "payload-uefi")]
            options: &flow.options,
            smm: self.smm,
        }
    }
}

impl<F: Silicon> crate::flow::Soc for Sp3<F> {
    const TSEG_SIZE: u32 = super::smm::TSEG_SIZE as u32;
    type Flow = Flow;

    fn max_cpus(config: &Config) -> u16 {
        config.max_cpus
    }

    /// The flash as this boot sees it, the APOB its BIOS directory names,
    /// the stored options over the board's policy, the memory map with
    /// TSEG, the TPM, then the FFS the flash anchor names.
    fn map(
        hw: &mut Hw,
        config: &mut Config,
        msr: &NativeMsr,
        _: Tsc,
    ) -> Result<Mapped<Self>, &'static str> {
        let generation = Generation::of(hw.identity.silicon());
        step(0x14, "APOB");
        let parts::Flash {
            efs: flash,
            ffs,
            #[cfg(feature = "payload-uefi")]
            ffs_offset,
            size: rom_size,
            nvram,
        } = parts::flash(&hw.fch, generation);
        let (apob, apob_span) = parts::read_apob(flash, generation);
        // The console level first, then the policy overrides, before the
        // first step that reads the policy after the FCH reset tasks (the
        // DF plan).
        let mut options = fstart_options::Report::new();
        let mut level = fstart_log::max_level();
        parts::options::resolve(
            nvram,
            &fstart_log::option::KNOBS,
            &mut level,
            &mut options,
            |_| Ok(()),
        );
        // SAFETY: BSP only, before MP.
        unsafe { fstart_log::set_max_level(level) };
        parts::options::resolve(
            nvram,
            config.options,
            &mut config.policy,
            &mut options,
            |_| Ok(()),
        );

        step(0x15, "memory map");
        let (tom, tom2) = fstart_driver_amd::ccx::msr::memory_limits(msr).map_err(|_| "TOP_MEM")?;
        let tops = crate::memory::TopOfMemory { tom, tom2 };
        let regions = parts::memory_regions(&apob, generation);
        let tseg = crate::memory::place_tseg(
            &regions,
            tops.tom,
            super::smm::TSEG_SIZE,
            &[layout::LAYOUT.early, apob_span],
        )
        .ok_or("TSEG placement")?;
        let reservations = super::memory::reservations(generation, apob_span, Some(tseg));
        let e820 =
            crate::memory::build("sp3", &regions, tops, &reservations).map_err(|_| "e820")?;
        parts::cache_ram(&regions);

        if config.discrete_tpm {
            step(0x16, "TPM");
            parts::tpm::start(flash, generation);
        }

        step(0x48, "boot media");
        crate::media::mount(e820.entries(), &layout::LAYOUT, ffs).map_err(|error| error.step())?;

        Ok(Mapped {
            top_mem: tops.tom,
            top_mem2: tops.tom2,
            apob: Some(apob),
            tseg: tseg.base,
            e820,
            ffs: ffs.base..ffs.end(),
            flow: Flow {
                #[cfg(feature = "payload-uefi")]
                ffs,
                #[cfg(feature = "payload-uefi")]
                ffs_offset,
                nvram,
                options,
                tseg,
                rom_size,
            },
        })
    }

    /// EINJ's scratch pages, selected with the live routing map.
    fn smm_memory(ctx: &Ctx<'_, Self>, state: &mut State<F>) -> Result<Reserved, &'static str> {
        step(0x58, "SMM");
        let mut reserved = Reserved::new();
        state.einj = parts::plan_einj(&state.soc, ctx.memory.e820, &mut reserved);
        Ok(reserved)
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
    fn install_smm(
        ctx: &Ctx<'_, Self>,
        state: &mut State<F>,
        mp: &MpHandle,
        flow: &Flow,
    ) -> Result<(), &'static str> {
        let einj = state.einj.unwrap_or_default();
        parts::clear_einj_scratch(&einj);
        let soc = &state.soc;
        let config = super::smm::SmmConfig {
            generation: Generation::of(soc.generation),
            ecam_base: soc.pci_root.ecam_base,
            psp_bar: soc.psp_bar as u32,
            einj,
            usb_buses: F::usb_buses(soc, &state.si)?,
            usb_bus_count: F::usb_bus_count(soc),
            bmc: ctx.config.bmc_sx_notify,
        };
        #[cfg(fstart_sp3_has_smm_image)]
        {
            const IMAGE: &[u8] = include_bytes!(env!("FSTART_SMM_IMAGE"));
            parts::tpm::measure_smm(IMAGE);
            // The handler's page tables map the low 4 GiB only; a PSP BAR
            // above would fault in SMM at SmmInfo, after the lock.
            if soc.psp_bar == 0 || soc.psp_bar >= crate::memory::FOUR_GIB {
                return Err("PSP BAR for SMM");
            }
            let smi = super::smm::Sp3Smi {
                fch: &soc.fch,
                config,
            };
            fstart_arch::x86::cpu::amd::smm::AmdSmm::new("sp3", &smi)
                .install(mp, IMAGE, flow.tseg.base, flow.tseg.size as u32)
                .map_err(|_| "SMM install")?;
            super::smm::prepare_psp_smi(&soc.fch);
            fstart_driver_amd::fch::hw::smi::software_smi(
                soc.fch.io(),
                super::smm::sw::PSP_SMM_INFO,
            );
            state.smm = Some(config);
            state.soc.smm_installed = true;
        }
        #[cfg(not(fstart_sp3_has_smm_image))]
        {
            let _ = (mp, config);
            fstart_log::warn!(
                "sp3: no SMM image in this build; TSEG {:#x} unused, SMM stays unlocked",
                flow.tseg.base
            );
        }
        Ok(())
    }

    fn tables(
        ctx: &Ctx<'_, Self>,
        silicon: &mut Up<Self>,
        pci: &fstart_pci::PciEcam,
        flow: &Flow,
        smbios: &fstart_acpi::smbios::SmbiosIdentity<'_>,
        acpi: crate::acpi::Board,
    ) -> Tables {
        step(0x70, "ACPI");
        let state = &silicon.state;
        let view = state.view(flow);
        let rsdp = parts::handoff::acpi(&view, &state.si, pci, &acpi, ctx.config.bmc_sx_notify);
        let smbios = super::smbios::install(&view, &silicon.cpus, flow.rom_size, smbios, pci);
        Tables {
            rsdp: Some(rsdp),
            smbios: Some(smbios),
            s5: fstart_core::description::AcpiSleepControl::new(
                super::acpi::of(Generation::of(state.soc.generation))
                    .ports
                    .control,
                crate::acpi::S5_SLEEP_TYPE,
            ),
        }
    }

    /// CrabEFI keeps its variables, the option overrides among them, in the
    /// settings store, and shows the options in its menu.
    #[cfg_attr(not(feature = "payload-uefi"), allow(unused_mut, unused_variables))]
    fn launch(
        mut handoff: fstart_payload::Handoff<'_>,
        _: &Ctx<'_, Self>,
        state: &State<F>,
        flow: &Flow,
    ) -> ! {
        step(0x90, "payload");
        fstart_log::flush();
        #[cfg(feature = "payload-uefi")]
        let view = state.view(flow);
        #[cfg(feature = "payload-uefi")]
        {
            type Storage = fstart_payload::crabefi::settings::FlashStorage<
                fstart_driver_amd::fch::spi::flash::Flash<hw::Tsc>,
            >;
            static mut STORAGE: Option<Storage> = None;
            // SAFETY: firmware BSS, written once here on the BSP; the
            // payload keeps the store for as long as it runs.
            let storage = unsafe { &mut *core::ptr::addr_of_mut!(STORAGE) };
            *storage = parts::options::storage(&view);
            handoff.settings = Some(parts::options::settings(&view));
            handoff.variable_storage = storage
                .as_mut()
                .map(|s| s as &mut dyn fstart_payload::crabefi::StorageBackend);
        }
        fstart_payload::boot(handoff, ())
    }
}
