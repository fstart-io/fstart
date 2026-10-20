//! The fixed SP3 boot flow (NATIVE_PORT_PLAN §4, INTEGRATION_NOTES).
//!
//! One function, [`run`], from the PSP's release of the BSP to the
//! payload. The shared steps live here; the silicon steps that differ are
//! in [`sp3`] (Rome and Milan, with the Milan deltas inline) and
//! [`naples`]. A build serves exactly one generation, so the silicon module
//! is chosen at compile time and the runtime identity check fails closed on
//! any other CPU.
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
//! | 20/21 | DF plan + maps / DF init |
//! | 30/31/32 | NBIO / SMU / FCH secondaries and USB |
//! | 38 | DXIO |
//! | 40 | FCH env, board GPIO |
//! | 50 | MP + CCX |
//! | 58 | SMM |
//! | 60 | PCI enumeration |
//! | 70 | ACPI |
//! | 78 | boot media |
//! | 80 | ReadyToBoot |
//! | 90 | payload |

extern crate alloc;

use alloc::vec::Vec;

use fstart_arch::x86::cpu::amd::msr::{SYSCFG, Syscfg, TOP_MEM, TOP_MEM2, TopMem, TopMem2};
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_core::services::memory_detect::{E820Kind, E820State};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_driver_zen::apob::sysmap::{self, Region};
use fstart_driver_zen::apob::{self, Apob};
use fstart_driver_zen::df::plan::{self, Plan};
use fstart_driver_zen::df::topology::{self, Topology};
use fstart_driver_zen::df::{Df, program};
use fstart_driver_zen::fch::{self, FchHw};
use fstart_driver_zen::psp::directory::Efs;
use fstart_driver_zen::soc::{self, Generation, Identity};
use fstart_driver_zen::topology::MAX_ROOT_BRIDGES;
use tock_registers::interfaces::Readable;

use crate::board::{Board, Policy};
use crate::hw::{self, Pci, PrimaryFch, Tsc, msr, post};
use crate::layout::{self, GENERATION, Span};
use crate::memory::{self, TopOfMemory};
use crate::pci::NonPci;

mod handoff;
#[cfg(feature = "naples")]
mod naples;
#[cfg(any(feature = "rome", feature = "milan"))]
mod sp3;

#[cfg(feature = "naples")]
use naples as silicon;
#[cfg(any(feature = "rome", feature = "milan"))]
use sp3 as silicon;

#[cfg(not(any(
    all(feature = "naples", not(any(feature = "rome", feature = "milan"))),
    all(feature = "rome", not(any(feature = "naples", feature = "milan"))),
    all(feature = "milan", not(any(feature = "naples", feature = "rome")))
)))]
compile_error!("a reset-image build enables exactly one of naples, rome, milan");

/// Halt after logging a failed step; the POST code of the step stays on
/// port 80h.
pub(crate) fn fail(step: &str) -> ! {
    fstart_log::error!("sp3: {} failed, halting", step);
    fstart_log::flush();
    fstart_arch::x86::boot::halt()
}

/// `Result`/`Option` unwrapping that halts with the step's name (fail
/// closed, INTEGRATION_NOTES "Errors: log and halt").
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

/// Physically contiguous, identity-mapped DRAM from the stage heap (inside
/// the reserved early region, so it stays the firmware's after handoff):
/// DMA buffers and tables the SMU/PSP keep using.
pub(crate) fn dma_alloc(size: usize, align: usize) -> &'static mut [u8] {
    let layout = core::alloc::Layout::from_size_align(size, align).or_halt("DMA layout");
    // SAFETY: nonzero size; never freed; zeroed before use.
    let ptr = unsafe { alloc::alloc::alloc_zeroed(layout) };
    if ptr.is_null() {
        fail("DMA allocation");
    }
    // SAFETY: fresh allocation of `size` bytes.
    unsafe { core::slice::from_raw_parts_mut(ptr, size) }
}

/// State the steps hand on.
pub(crate) struct Ctx {
    pub generation: Generation,
    pub identity: Identity,
    pub timer: Tsc,
    pub pci: Pci,
    pub ecam: (u64, u64),
    pub fch: PrimaryFch,
    pub resume: bool,
    pub policy: Policy,
    /// The booted 16 MiB flash half (EFS, directories, microcode).
    pub flash: &'static [u8],
    pub apob: Apob<'static>,
    pub tops: TopOfMemory,
    pub tseg: Span,
    pub e820: E820State,
    pub df: Df<Pci>,
    pub topology: Topology,
    pub plan: Plan,
    /// Non-PCI MMIO of each root bridge, physical order.
    pub non_pci: heapless::Vec<NonPci, MAX_ROOT_BRIDGES>,
    pub phys_addr_bits: u8,
    /// Socket 0's (Naples: die 0's) PSP BAR.
    pub psp_bar: u64,
    /// APIC IDs of every enabled thread, BSP first (MADT order).
    pub apic_ids: Vec<u32>,
    pub smm: Option<crate::smm::SmmConfig>,
    pub rsdp: Option<u64>,
}

/// The fixed flow.
pub(crate) fn run<B: Board>() -> ! {
    const {
        assert!(matches!(
            (B::GENERATION, GENERATION),
            (Generation::Naples, Generation::Naples)
                | (Generation::Rome, Generation::Rome)
                | (Generation::Milan, Generation::Milan)
        ))
    };
    let generation = GENERATION;

    step(0x10, "ECAM");
    let ecam = hw::enable_ecam(generation);
    let timer = Tsc::new();
    let fch = hw::primary_fch(timer);

    step(0x11, "FCH reset tasks");
    let resume = fch_reset(&fch, generation);
    B::early_init(&fch).or_halt("board early init");

    step(0x12, "console");
    let mut console = Ns16550::new(B::CONSOLE).or_halt("console");
    console.init().or_halt("console");
    // SAFETY: `console` lives on this never-returning frame.
    unsafe { fstart_log::init(&console) };
    fstart_log::info!("sp3: {} ({}) console ready", B::NAME, generation.name());
    if resume {
        // S3 needs the resume path (boot scripts, wake vector); not built.
        fstart_log::error!("sp3: S3 resume is not supported, cold booting");
    }

    step(0x13, "CPU identity");
    let identity = identify(generation);

    step(0x14, "APOB");
    let flash = flash_half();
    let (apob, apob_span) = read_apob(flash, generation);

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
        generation,
        identity,
        timer,
        pci: Pci::default(),
        ecam,
        fch,
        resume: false,
        policy: B::policy(),
        flash,
        apob,
        tops,
        tseg,
        e820,
        df: Df::new(Pci::default()),
        topology: Topology {
            generation,
            sockets: 0,
            nodes: heapless::Vec::new(),
            root_bridges: heapless::Vec::new(),
        },
        plan: Plan {
            mmio: heapless::Vec::new(),
            io: heapless::Vec::new(),
            root_bridges: heapless::Vec::new(),
            above_ecam_too_small: false,
            below_ecam_too_small: false,
            above_ecam: 0,
        },
        non_pci: heapless::Vec::new(),
        phys_addr_bits: 0,
        psp_bar: 0,
        apic_ids: Vec::new(),
        smm: None,
        rsdp: None,
    };

    step(0x20, "DF topology and maps");
    data_fabric(&mut ctx);
    step(0x21, "DF init");
    silicon::df_init(&mut ctx);

    step(0x30, "NBIO early");
    let mut si = silicon::nbio_early(&mut ctx);
    step(0x31, "SMU");
    silicon::smu_init(&mut ctx, &mut si);
    step(0x32, "FCH secondaries, USB");
    silicon::fch_secondaries(&mut ctx, &si);

    step(0x38, "DXIO");
    let levels = detect_levels::<B>(&ctx.fch);
    let engines = B::dxio_engines(&levels, &ctx.timer);
    silicon::dxio(&mut ctx, &si, &engines);

    step(0x40, "FCH env, GPIO");
    fch_env::<B>(&ctx);
    silicon::fch_env(&ctx, &si);

    step(0x50, "MP and CCX");
    let mp = silicon::mp::<B>(&mut ctx, &si);

    step(0x58, "SMM");
    install_smm(&mut ctx, &si, &mp);

    step(0x60, "PCI enumeration");
    silicon::pci_pre(&mut ctx, &mut si);
    let roots = crate::flow::enumerate(&ctx);
    silicon::pci_post(&ctx, &si);

    step(0x70, "ACPI");
    #[cfg(feature = "acpi")]
    {
        ctx.rsdp = Some(handoff::acpi::<B>(&ctx, &si));
    }

    step(0x78, "boot media");
    handoff::mount_boot_media(&ctx);

    step(0x80, "ReadyToBoot");
    ready_to_boot(&ctx, &si);

    step(0x90, "payload");
    if !fstart_arch::x86::mp::park_aps_for_payload() {
        fail("AP park");
    }
    handoff::boot(ctx, roots, &console)
}

/// The eight PEI reset tasks on socket 0's FCH (FCH_SP3 §2.1). Returns
/// whether this is an S3 resume.
fn fch_reset(fch: &PrimaryFch, generation: Generation) -> bool {
    use fch::common::reset;
    reset::prelim(fch, generation);
    reset::ab_bar(fch);
    fch::spi::reset(fch, generation, &fch::spi::SpiPolicy::default());
    reset::hw_acpi(fch, generation);
    match generation {
        #[cfg(feature = "naples")]
        Generation::Naples => fch::taishan::sata::reset(fch, true),
        #[cfg(any(feature = "rome", feature = "milan"))]
        Generation::Rome | Generation::Milan => {
            fch::huashan::sata::reset(fch, &fch::huashan::sata::SataPolicy::socket0())
        }
        #[allow(unreachable_patterns)]
        _ => fail("FCH SATA reset"),
    }
    fch::lpc::reset(fch, generation);
    reset::espi(fch, false);
    reset::resume_sleep_type(fch) == 3
}

/// CPUID → revision; anything else than this build's generation and the
/// SP3 package halts (SOC_DF §2.3: no "unknown stepping = first row").
fn identify(generation: Generation) -> Identity {
    #[cfg(all(target_os = "none", fstart_entry = "amd-psp-reset"))]
    if crate::entry::bsp_bist() != 0 {
        fstart_log::error!("sp3: BSP BIST {:#x}", crate::entry::bsp_bist());
        fail("BIST");
    }
    let signature = fstart_arch::x86::cpuid(1).0;
    let ext_ebx = fstart_arch::x86::cpuid(0x8000_0001).1;
    let identity = soc::identify(signature, ext_ebx).unwrap_or_else(|_| {
        fstart_log::error!("sp3: unsupported CPU {:#x}/{:#x}", signature, ext_ebx);
        fail("CPU identity")
    });
    if identity.generation() != generation {
        fstart_log::error!(
            "sp3: {} CPU in a {} build",
            identity.generation().name(),
            generation.name()
        );
        fail("CPU identity");
    }
    fstart_log::info!("sp3: CPU {} ({:#x})", identity.revision.name(), signature);
    identity
}

/// The booted flash half as the SPI ROM window maps it.
fn flash_half() -> &'static [u8] {
    // SAFETY: the FCH maps the booted 16 MiB half read-only at
    // `0xff00_0000`; nothing writes flash during boot.
    unsafe {
        core::slice::from_raw_parts(
            layout::FLASH_MMIO.base as *const u8,
            layout::FLASH_MMIO.size as usize,
        )
    }
}

/// The APOB from BIOS directory entry 0x61 (PSP_APOB §3.5); logs the ABL
/// event log and the APCB recovery flag.
fn read_apob(flash: &'static [u8], generation: Generation) -> (Apob<'static>, Span) {
    let address = Efs::new(flash)
        .and_then(|efs| efs.bios_directories(generation))
        .ok()
        .and_then(|dirs| dirs.apob_address())
        .or_halt("APOB directory entry");
    // SAFETY: the PSP wrote the APOB there before releasing x86; it is
    // reserved in e820 and only read.
    let apob = unsafe { Apob::from_address(address, generation) }.or_halt("APOB header");
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
    sysmap::memory_map(generation, map.top, map.holes()).or_halt("memory map")
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
    msr().reg(SYSCFG).modify(Syscfg::TOM2_WB::SET);
}

/// DF topology, board bus counts, the MMIO/IO planner and its maps
/// (SOC_DF §3.3–3.4). Runs before NBIO: every NBIO access goes through a
/// root bus the DF routes.
fn data_fabric(ctx: &mut Ctx) {
    let mut topology = topology::discover(&ctx.df, ctx.generation).or_halt("DF topology");
    if let Some(ranges) =
        plan::plan_buses(&topology.root_bridges, &ctx.policy.df).or_halt("DF bus plan")
    {
        silicon::bus_maps(&ctx.df, &mut topology, &ranges);
    }
    ctx.phys_addr_bits = phys_addr_bits(ctx.generation, ctx.policy.ccx.smee);
    let plan = plan::plan(
        &plan::Input {
            tom: ctx.tops.tom,
            tom2: ctx.tops.tom2.unwrap_or(0),
            ecam_base: ctx.ecam.0,
            ecam_size: ctx.ecam.1,
            phys_addr_bits: ctx.phys_addr_bits,
            root_bridges: &topology.root_bridges,
            mmio_pairs: silicon::MMIO_PAIRS,
        },
        &ctx.policy.df,
    )
    .or_halt("DF plan");
    silicon::program_maps(&ctx.df, &topology, &plan);
    for rb in &plan.root_bridges {
        fstart_log::info!(
            "df: rb {}.{} buses {:#x}-{:#x} mmio {:#x}+{:#x}",
            rb.socket,
            rb.index,
            rb.bus.base,
            rb.bus.limit,
            rb.below.base,
            rb.below.size
        );
    }
    ctx.non_pci = plan.root_bridges.iter().map(NonPci::of).collect();
    ctx.topology = topology;
    ctx.plan = plan;
}

/// CPUID 8000_0008 EAX\[7:0\], minus the memory-encryption reduction
/// (8000_001F EBX\[11:6\]) where it applies: Rome always, Milan with SMEE
/// (SOC_DF §3.4).
fn phys_addr_bits(generation: Generation, smee: bool) -> u8 {
    let bits = fstart_arch::x86::cpuid(0x8000_0008).0 & 0xff;
    let reduced = (fstart_arch::x86::cpuid(0x8000_001f).1 >> 6) & 0x3f;
    let reduce = match generation {
        Generation::Naples => false,
        Generation::Rome => true,
        Generation::Milan => smee,
    };
    (bits - if reduce { reduced } else { 0 }) as u8
}

/// Configure and read the board's detect pads (before its GPIO table).
fn detect_levels<B: Board>(fch: &PrimaryFch) -> heapless::Vec<bool, 8> {
    let acpi = fch.acpi();
    B::detect_pads()
        .iter()
        .map(|pad| {
            fch::gpio::configure(acpi, pad);
            fch::gpio::read(acpi, pad.pin)
        })
        .collect()
}

/// FCH env (DXE) tasks on socket 0 and the board GPIO program (FCH_SP3
/// §2.2, BOARD §2).
fn fch_env<B: Board>(ctx: &Ctx) {
    use fch::common::env;
    let (fch, generation) = (&ctx.fch, ctx.generation);
    env::hw_acpi_pre(fch, generation);
    env::hw_acpi(fch, generation, &ctx.policy.fch_env);
    fch::lpc::env(fch, generation);
    env::sd(fch, generation);
    fch::alink::clock_gating(
        fch,
        generation,
        &fch::alink::AbPolicy::default_for(generation),
    );
    let gpio = B::gpio();
    fch::gpio::configure_all(fch.acpi(), gpio.early);
    fch::gpio::configure_all(fch.acpi(), gpio.table);
}

/// TSEG, SMBASE and lock on every thread with the FCH handler image, then
/// the PSP SmmInfo registration from SMM (PSP_APOB §3.3). Without an SMM
/// image in this build SMM stays unlocked; that is logged, not hidden.
fn install_smm(ctx: &mut Ctx, si: &silicon::Silicon, mp: &fstart_arch::x86::mp::MpHandle) {
    let config = crate::smm::SmmConfig {
        generation: ctx.generation,
        ecam_base: ctx.ecam.0,
        psp_bar: ctx.psp_bar,
        usb_buses: silicon::usb_buses(ctx, si),
        usb_bus_count: silicon::usb_bus_count(ctx),
    };
    #[cfg(fstart_sp3_has_smm_image)]
    {
        const IMAGE: &[u8] = include_bytes!(env!("FSTART_SMM_IMAGE"));
        let smi = crate::smm::Sp3Smi {
            fch: &ctx.fch,
            config,
        };
        fstart_arch::x86::cpu::amd::smm::AmdSmm::new("sp3", &smi)
            .install(mp, IMAGE, ctx.tseg.base, ctx.tseg.size as u32)
            .or_halt("SMM install");
        crate::smm::prepare_psp_smi(&ctx.fch);
        fch::smi::software_smi(ctx.fch.io(), crate::smm::sw::PSP_SMM_INFO);
        ctx.smm = Some(config);
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

/// Enumerate every root bridge with the allocator windows of the DF plan.
pub(crate) fn enumerate(ctx: &Ctx) -> Vec<fstart_pci::PciEcam> {
    ctx.plan
        .root_bridges
        .iter()
        .map(|rb| {
            let mut pci = fstart_pci::PciEcam::from_config(&crate::pci::root_config(rb, ctx.ecam))
                .or_halt("PCI root");
            pci.enumerate_and_allocate().or_halt("PCI enumeration");
            fstart_log::info!(
                "pci: rb {}.{}: {} functions",
                rb.socket,
                rb.index,
                pci.device_count()
            );
            pci
        })
        .collect()
}

/// ReadyToBoot (SOC_DF §3.7, PSP_APOB §3.4, FCH_SP3 §2.3): FCH late
/// tasks, DF extended config off, PSP LockDFReg, Milan DRTM off and TPM
/// establishment reset, BootDone last.
fn ready_to_boot(ctx: &Ctx, si: &silicon::Silicon) {
    use fstart_driver_zen::psp::command::{BootDone, C2pBuffer, LockDfReg};
    fch::common::late::hw_acpi(&ctx.fch, ctx.generation, &[]);
    silicon::fch_late(ctx, si);
    program::ready_to_boot(&ctx.df, &ctx.topology.nodes);
    let psp = psp(ctx);
    psp.send(&ctx.timer, &mut C2pBuffer::new(LockDfReg))
        .or_halt("PSP LockDFReg");
    #[cfg(feature = "milan")]
    if ctx.generation == Generation::Milan {
        use fstart_driver_zen::psp::command::{DisableDrtm, ResetTpmEstablishment};
        psp.send(&ctx.timer, &mut C2pBuffer::new(DisableDrtm::new()))
            .or_halt("PSP DisableDrtm");
        // SAFETY: the TPM locality-0 page; all-ones means no TPM.
        let tpm = unsafe { fstart_core::mmio::read32(0xfed4_0000 as *const u32) };
        if tpm != u32::MAX {
            psp.send(&ctx.timer, &mut C2pBuffer::new(ResetTpmEstablishment))
                .or_halt("PSP ResetTpmEstablishment");
        }
    }
    psp.send(&ctx.timer, &mut C2pBuffer::new(BootDone))
        .or_halt("PSP BootDone");
}

/// Socket 0's C2P mailbox through its PSP BAR.
pub(crate) fn psp(
    ctx: &Ctx,
) -> fstart_driver_zen::psp::mailbox::Mailbox<fstart_driver_zen::psp::mailbox::MmioTransport> {
    use fstart_driver_zen::psp::mailbox::{Mailbox, MmioTransport};
    if ctx.psp_bar == 0 {
        fail("PSP BAR");
    }
    // SAFETY: the BAR NBIO programmed; the flow owns the mailbox.
    Mailbox::new(unsafe { MmioTransport::new(ctx.psp_bar) }, ctx.generation)
}

/// Every online thread's APIC ID, BSP (CPU 0) first.
pub(crate) fn collect_apic_ids(mp: &fstart_arch::x86::mp::MpHandle) -> Vec<u32> {
    use core::sync::atomic::{AtomicU32, Ordering};
    let ids: Vec<AtomicU32> = (0..mp.num_cpus()).map(|_| AtomicU32::new(0)).collect();
    mp.scope(|s| {
        s.scatter(&|cpu: u32| {
            if let Some(slot) = ids.get(cpu as usize) {
                slot.store(crate::mp::apic_id(), Ordering::Release);
            }
        })
    });
    ids.iter().map(|i| i.load(Ordering::Acquire)).collect()
}

/// The FCH IOAPIC (`FEC0_0000h`) gets the generation's ID (ACPI_SMM §3.2).
pub(crate) fn set_fch_ioapic_id(generation: Generation) {
    // SAFETY: the FCH IOAPIC is decoded at its fixed address.
    let ioapic = unsafe { fstart_arch::x86::ioapic::IoApic::new(0xfec0_0000) };
    ioapic.set_id(fstart_driver_zen::nbio::ioapic::fch_ioapic_id(generation));
}
