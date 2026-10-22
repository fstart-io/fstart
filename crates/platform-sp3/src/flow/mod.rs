//! The fixed SP3 boot flow (NATIVE_PORT_PLAN §4, INTEGRATION_NOTES).
//!
//! One function, [`run`], from the PSP's release of the BSP to the
//! payload. The shared steps live here; the silicon steps that differ are
//! in [`sp3`] (Rome and Milan, with the Milan deltas inline) and
//! [`naples`], bound to the flow by [`Steps`]. One reset image carries
//! every generation the board supports: `Program::run_stage` identifies
//! the CPU ([`identify`]) and runs the flow with the generation's steps; a
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
use fstart_arch::x86::mp::MpHandle;
use fstart_core::services::memory_detect::{E820Kind, E820State};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_driver_zen::apob::sysmap::{self, Region};
use fstart_driver_zen::apob::{self, Apob};
use fstart_driver_zen::df::plan::{self, Plan};
use fstart_driver_zen::df::topology::{self, Topology};
use fstart_driver_zen::df::{BusRange, Df, program};
use fstart_driver_zen::dxio::engine::MAX_ENGINES;
use fstart_driver_zen::dxio::lanes::Engine;
use fstart_driver_zen::fch::rom3::{self, FlashAnchor, FlashMap, Rom3};
use fstart_driver_zen::fch::{self, FchHw};
use fstart_driver_zen::nbio::Nbio;
use fstart_driver_zen::psp::directory::Efs;
use fstart_driver_zen::soc::{self, Generation, Identity};
use fstart_driver_zen::topology::MAX_ROOT_BRIDGES;
use tock_registers::interfaces::Readable;
use zerocopy::IntoBytes;

use crate::board::{Board, Policy};
use crate::hw::{self, Pci, PrimaryFch, Tsc, msr, post};
use crate::layout::{self, Span};
use crate::memory::{self, TopOfMemory};
use crate::pci::NonPci;

mod handoff;
#[cfg(feature = "naples")]
pub(crate) mod naples;
#[cfg(any(feature = "rome", feature = "milan"))]
pub(crate) mod sp3;

#[cfg(not(any(feature = "naples", feature = "rome", feature = "milan")))]
compile_error!("a reset-image build enables at least one of naples, rome, milan");

/// The silicon steps of one family, in flow order: [`naples`] (DF2, an
/// NBIO and SMU V9 per die) and [`sp3`] (DF3, SMU V11 per IOD; Rome and
/// Milan). Each module implements them as free functions of the same
/// names, bound by [`steps!`].
pub(crate) trait Steps: Sized {
    /// DF MMIO base/limit pairs per node.
    const MMIO_PAIRS: u8;
    fn bus_maps(df: &Df<Pci>, topology: &mut Topology, ranges: &[BusRange]);
    fn program_maps(df: &Df<Pci>, topology: &Topology, plan: &Plan);
    fn df_init(ctx: &mut Ctx);
    fn nbio_early(ctx: &mut Ctx) -> Self;
    fn smu_init(ctx: &mut Ctx, si: &mut Self);
    fn fch_secondaries(ctx: &mut Ctx, si: &Self);
    fn dxio(ctx: &mut Ctx, si: &Self, engines: &heapless::Vec<Engine<'static>, MAX_ENGINES>);
    fn fch_env(ctx: &Ctx, si: &Self);
    fn mp<B: Board>(ctx: &mut Ctx, si: &Self) -> MpHandle;
    fn pci_pre(ctx: &mut Ctx, si: &mut Self);
    fn pci_post(ctx: &Ctx, si: &Self);
    fn fch_late(ctx: &Ctx, si: &Self);
    fn usb_buses(ctx: &Ctx, si: &Self) -> [u8; 8];
    fn usb_bus_count(ctx: &Ctx) -> u8;
    /// NUMA domains for SRAT/SLIT/MSCT and `_PXM`; `None` with one domain.
    fn numa(ctx: &Ctx, si: &Self) -> Option<NumaPlan>;
    /// Every present NBIO, physical order.
    fn nbios(&self) -> &[Nbio<Pci>];
}

/// The SMU-served CPPC PCC region (`smu::v11::cppc`).
pub(crate) struct Cppc {
    pub base: u64,
    pub smu_bar: u64,
    pub info: fstart_driver_zen::smu::v11::CppcInfo,
    pub lowest_mhz: u32,
    pub nominal_mhz: u32,
    /// APIC ID of each PCC slot (the PPTable CPPC map, sockets in order).
    pub slots: Vec<u32>,
}

/// NUMA domains as ACPI needs them.
pub(crate) struct NumaPlan {
    /// Socket of each proximity domain.
    pub domains: Vec<u8>,
    /// DRAM `(base, end, domain)`.
    pub memory: Vec<(u64, u64, u32)>,
    /// Domain of each thread, in [`Ctx::apic_ids`] order.
    pub cpus: Vec<u32>,
    /// Domain of each root bridge, in topology order.
    pub root_bridges: Vec<u32>,
}

/// Implement [`Steps`] for a silicon module's `Silicon` with the module's
/// free functions.
macro_rules! steps {
    () => {
        impl super::Steps for Silicon {
            const MMIO_PAIRS: u8 = MMIO_PAIRS;
            fn bus_maps(df: &Df<Pci>, topology: &mut Topology, ranges: &[BusRange]) {
                bus_maps(df, topology, ranges)
            }
            fn program_maps(df: &Df<Pci>, topology: &Topology, plan: &Plan) {
                program_maps(df, topology, plan)
            }
            fn df_init(ctx: &mut Ctx) {
                df_init(ctx)
            }
            fn nbio_early(ctx: &mut Ctx) -> Self {
                nbio_early(ctx)
            }
            fn smu_init(ctx: &mut Ctx, si: &mut Self) {
                smu_init(ctx, si)
            }
            fn fch_secondaries(ctx: &mut Ctx, si: &Self) {
                fch_secondaries(ctx, si)
            }
            fn dxio(
                ctx: &mut Ctx,
                si: &Self,
                engines: &heapless::Vec<Engine<'static>, MAX_ENGINES>,
            ) {
                dxio(ctx, si, engines)
            }
            fn fch_env(ctx: &Ctx, si: &Self) {
                fch_env(ctx, si)
            }
            fn mp<B: Board>(ctx: &mut Ctx, si: &Self) -> MpHandle {
                mp::<B>(ctx, si)
            }
            fn pci_pre(ctx: &mut Ctx, si: &mut Self) {
                pci_pre(ctx, si)
            }
            fn pci_post(ctx: &Ctx, si: &Self) {
                pci_post(ctx, si)
            }
            fn fch_late(ctx: &Ctx, si: &Self) {
                fch_late(ctx, si)
            }
            fn usb_buses(ctx: &Ctx, si: &Self) -> [u8; 8] {
                usb_buses(ctx, si)
            }
            fn usb_bus_count(ctx: &Ctx) -> u8 {
                usb_bus_count(ctx)
            }
            fn numa(ctx: &Ctx, si: &Self) -> Option<super::NumaPlan> {
                numa(ctx, si)
            }
            fn nbios(&self) -> &[fstart_driver_zen::nbio::Nbio<Pci>] {
                &self.nbios
            }
        }
    };
}
use steps;

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

/// DXIO training, halting with the error's details logged.
pub(crate) fn dxio_or_halt(result: Result<(), fstart_driver_zen::dxio::DxioError>) {
    if let Err(e) = result {
        let (what, a, b) = e.parts();
        fstart_log::error!("sp3: DXIO error {} ({:#x}, {:#x})", what, a, b);
        fail("DXIO training");
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
    /// The flash from the start of the half whose EFS the boot ROM took
    /// (EFS, directories, microcode; see [`flash`]).
    pub flash: &'static [u8],
    /// The FFS in the CPU view (ROM3), from the flash anchor.
    pub ffs: Span,
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
    /// CPPC through the SMU, when the SMU reports it enabled.
    pub cppc: Option<Cppc>,
    pub smm: Option<crate::smm::SmmConfig>,
    pub rsdp: Option<u64>,
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

/// The fixed flow for `identity`'s generation with its silicon steps `S`.
pub(crate) fn run<B: Board, S: Steps>(identity: Identity) -> ! {
    let generation = identity.generation();

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
    check_identity::<B>(&identity);

    step(0x14, "APOB");
    let (flash, ffs) = flash(&fch, generation);
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
        policy: B::policy(generation),
        flash,
        ffs,
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
        cppc: None,
        smm: None,
        rsdp: None,
    };

    step(0x20, "DF topology and maps");
    data_fabric::<S>(&mut ctx);
    step(0x21, "DF init");
    S::df_init(&mut ctx);

    step(0x30, "NBIO early");
    let mut si = S::nbio_early(&mut ctx);
    step(0x31, "SMU");
    S::smu_init(&mut ctx, &mut si);
    step(0x32, "FCH secondaries, USB");
    S::fch_secondaries(&mut ctx, &si);

    step(0x38, "DXIO");
    let levels = detect_levels::<B>(&ctx.fch, generation);
    board_gpio::<B>(&ctx.fch, generation);
    let engines = B::dxio_engines(generation, &levels, &ctx.timer);
    S::dxio(&mut ctx, &si, &engines);

    step(0x40, "FCH env");
    fch_env::<B>(&ctx);
    S::fch_env(&ctx, &si);

    step(0x50, "MP and CCX");
    let mp = S::mp::<B>(&mut ctx, &si);

    step(0x58, "SMM");
    install_smm(&mut ctx, &si, &mp, B::BMC_SX_NOTIFY);

    step(0x60, "PCI enumeration");
    S::pci_pre(&mut ctx, &mut si);
    let roots = crate::flow::enumerate(&ctx);
    S::pci_post(&ctx, &si);

    step(0x70, "ACPI");
    #[cfg(feature = "acpi")]
    {
        ctx.rsdp = Some(handoff::acpi::<B, S>(&ctx, &si));
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

/// With the console up: the BSP BIST and whether the board supports the
/// CPU's generation (the build may carry more generations than a board
/// lists).
fn check_identity<B: Board>(identity: &Identity) {
    #[cfg(all(target_os = "none", fstart_entry = "amd-psp-reset"))]
    if crate::entry::bsp_bist() != 0 {
        fstart_log::error!("sp3: BSP BIST {:#x}", crate::entry::bsp_bist());
        fail("BIST");
    }
    let generation = identity.generation();
    fstart_log::info!(
        "sp3: CPU {} ({:#x})",
        identity.revision.name(),
        identity.signature
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

/// The SPI flash as this boot sees it (`fch::rom3`): the EFS view (the
/// half whose EFS the generation's boot ROM takes) and the FFS window.
/// Both are read through ROM2 when they lie in the page it shows; ROM3 is
/// probed only for an FFS outside it, and whatever neither shows halts.
fn flash(fch: &PrimaryFch, generation: Generation) -> (&'static [u8], Span) {
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
            Efs::new(view)
                .is_ok_and(|efs| efs.matches(generation))
                .then_some(view)
        })
        .or_halt("EFS");
    let ffs = map.address(offset, size).or_halt("FFS window");
    fstart_log::info!("sp3: FFS at flash {:#x}+{:#x}", offset, size);
    (efs, Span::new(ffs, u64::from(size)))
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
    fixed_mtrrs_to_dram();
    msr().reg(SYSCFG).modify(Syscfg::TOM2_WB::SET);
}

/// Fixed MTRRs: with SYSCFG.MtrrFixDramEn set (the reset image keeps the
/// vendor's setting), an access in a fixed range goes to DRAM only if the
/// range's RdDram/WrDram bits (4/3) are set, otherwise to MMIO (AMD APM
/// vol. 2 §7.9). The generic MTRR code writes types only and those bits
/// are write-protected while MtrrFixDramModEn is clear, so the values ABL
/// left stay. Route every write-back fixed range to DRAM, as AGESA does.
fn fixed_mtrrs_to_dram() {
    use fstart_arch::x86::cpu::amd::msr::Syscfg;
    use tock_registers::interfaces::ReadWriteable;
    const FIXED: [u32; 11] = [
        0x250, 0x258, 0x259, 0x268, 0x269, 0x26a, 0x26b, 0x26c, 0x26d, 0x26e, 0x26f,
    ];
    const RD_WR_DRAM: u8 = 0x18;
    const WB: u8 = 6;
    let cpu = msr();
    let syscfg = cpu.reg(SYSCFG);
    crate::hw::wbinvd();
    syscfg.modify(Syscfg::MTRR_FIX_DRAM_MOD_EN::SET);
    for index in FIXED {
        let old = cpu.read(index);
        let new = u64::from_le_bytes(old.to_le_bytes().map(|b| {
            if b & 7 == WB {
                b | RD_WR_DRAM
            } else {
                b & !RD_WR_DRAM
            }
        }));
        if new != old {
            cpu.write(index, new);
        }
        fstart_log::info!("mtrr: fixed {:#x} {:#x} -> {:#x}", index, old, new);
    }
    syscfg.modify(Syscfg::MTRR_FIX_DRAM_MOD_EN::CLEAR);
    crate::hw::wbinvd();
}

/// DF topology, board bus counts, the MMIO/IO planner and its maps
/// (SOC_DF §3.3–3.4). Runs before NBIO: every NBIO access goes through a
/// root bus the DF routes.
fn data_fabric<S: Steps>(ctx: &mut Ctx) {
    let mut topology = topology::discover(&ctx.df, ctx.generation).or_halt("DF topology");
    if let Some(ranges) =
        plan::plan_buses(&topology.root_bridges, &ctx.policy.df).or_halt("DF bus plan")
    {
        S::bus_maps(&ctx.df, &mut topology, &ranges);
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
            mmio_pairs: S::MMIO_PAIRS,
        },
        &ctx.policy.df,
    )
    .or_halt("DF plan");
    S::program_maps(&ctx.df, &topology, &plan);
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
fn detect_levels<B: Board>(fch: &PrimaryFch, generation: Generation) -> heapless::Vec<bool, 8> {
    let acpi = fch.acpi();
    B::detect_pads(generation)
        .iter()
        .map(|pad| {
            fch::gpio::configure(acpi, pad);
            fch::gpio::read(acpi, pad.pin)
        })
        .collect()
}

/// FCH env (DXE) tasks on socket 0 (FCH_SP3 §2.2).
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
}

/// The board GPIO program: the OEM pads, then the CPM table. The vendor
/// applies both in PEI before the PCIe topology reaches DXIO
/// (BOARD_S8030 §2 steps 3–5), so board enables and resets are in place
/// when the links train.
fn board_gpio<B: Board>(fch: &PrimaryFch, generation: Generation) {
    let gpio = B::gpio(generation);
    fch::gpio::configure_all(fch.acpi(), gpio.early);
    fch::gpio::configure_all(fch.acpi(), gpio.table);
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
    bmc: Option<crate::smm::BmcSxNotify>,
) {
    let config = crate::smm::SmmConfig {
        generation: ctx.generation,
        ecam_base: ctx.ecam.0,
        psp_bar: ctx.psp_bar,
        usb_buses: S::usb_buses(ctx, si),
        usb_bus_count: S::usb_bus_count(ctx),
        bmc,
    };
    #[cfg(fstart_sp3_has_smm_image)]
    {
        const IMAGE: &[u8] = include_bytes!(env!("FSTART_SMM_IMAGE"));
        // The handler's page tables map the low 4 GiB only; a PSP BAR
        // above would fault in SMM at SmmInfo, after the lock.
        if ctx.psp_bar == 0 || ctx.psp_bar >= crate::memory::FOUR_GIB {
            fail("PSP BAR for SMM");
        }
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
fn ready_to_boot<S: Steps>(ctx: &Ctx, si: &S) {
    use fstart_driver_zen::psp::command::{BootDone, C2pBuffer, LockDfReg};
    fch::common::late::hw_acpi(&ctx.fch, ctx.generation, &[]);
    S::fch_late(ctx, si);
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
pub(crate) fn collect_apic_ids(mp: &MpHandle) -> Vec<u32> {
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
