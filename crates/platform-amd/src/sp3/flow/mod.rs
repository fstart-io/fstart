//! What SP3's part of the flow ([`crate::sp3::soc`]) is made of: the
//! flash windows, the APOB, the memory map, EINJ's scratch pages, the
//! tables and the options.

extern crate alloc;

use fstart_amdfw_format::anchor::FlashAnchor;
use fstart_amdfw_format::directory::{BiosEntry, Levels, bios};
use fstart_arch::x86::cpu::amd::msr::{SYSCFG, Syscfg, TOP_MEM, TOP_MEM2, TopMem, TopMem2};
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_core::services::memory_detect::{E820Kind, E820State};
use fstart_driver_amd::apob::sysmap::{self, Region};
use fstart_driver_amd::apob::{self, Apob};
use fstart_driver_amd::sp3::fch::rom3::{self, FlashMap, Rom3};
use fstart_driver_amd::sp3::soc::hw::{PrimaryFch, msr, post};
use fstart_driver_amd::sp3::soc::{self, Generation};
use tock_registers::interfaces::Readable;
use zerocopy::IntoBytes;

use crate::sp3::layout::{self, Span};
use crate::sp3::memory::{self, TopOfMemory};

pub(crate) mod handoff;
pub(crate) mod options;
pub(crate) mod tpm;

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

/// What the tables and the handoff read: the silicon steps' state and
/// the flow's own.
pub(crate) struct Ctx<'a> {
    pub soc: &'a soc::Ctx,
    /// The FFS in the CPU view (ROM3), from the flash anchor.
    #[cfg(feature = "payload-uefi")]
    pub ffs: Span,
    /// The FFS's flash offset, for the payload's settings storage.
    #[cfg(feature = "payload-uefi")]
    pub ffs_offset: u32,
    /// The outcome of every option knob resolved so far.
    #[cfg(feature = "payload-uefi")]
    pub options: &'a fstart_options::Report,
    pub smm: Option<crate::sp3::smm::SmmConfig>,
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
pub(crate) struct Flash {
    /// The booted half (from its EFS).
    pub efs: &'static [u8],
    /// The FFS in the CPU view, and its flash offset.
    pub ffs: Span,
    #[cfg(feature = "payload-uefi")]
    pub ffs_offset: u32,
    /// The size of the part.
    pub size: u32,
    /// The settings store, if a window shows it.
    pub nvram: Option<&'static [u8]>,
}

/// The SPI flash as this boot sees it (`fch::rom3`): the EFS view (the
/// half whose EFS the generation's boot ROM takes), the FFS window and the
/// settings store. They are read through ROM2 when they lie in the page it
/// shows; ROM3 is probed only for an FFS outside it, and an EFS or FFS that
/// neither shows halts.
pub(crate) fn flash(fch: &PrimaryFch, generation: Generation) -> Flash {
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
pub(crate) fn read_apob(flash: &'static [u8], generation: Generation) -> (Apob<'static>, Span) {
    let address = bios_directories(flash, generation)
        .and_then(|dirs| dirs.find(bios::APOB, None, None))
        .map(|(_, entry)| entry.destination())
        .filter(|&d| d != u64::MAX)
        .or_halt("APOB directory entry");
    // SAFETY: the PSP wrote the APOB there before releasing x86; it is
    // reserved in e820 and only read.
    let apob =
        unsafe { Apob::from_address(address, generation.silicon().into()) }.or_halt("APOB header");
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
pub(crate) fn top_of_memory() -> TopOfMemory {
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

pub(crate) fn memory_regions(
    apob: &Apob<'_>,
    generation: Generation,
) -> heapless::Vec<Region, { sysmap::MAX_REGIONS }> {
    let map = sysmap::system_memory_map(apob).or_halt("APOB memory map");
    sysmap::memory_map(generation.silicon().into(), map.top, map.holes()).or_halt("memory map")
}

pub(crate) fn build_e820(
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
pub(crate) fn cache_ram(regions: &[Region]) {
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

/// Select scratch pages using the live routing map, and name those the
/// flow has to take out of the memory map (`reserved`) before
/// [`clear_einj_scratch`] and the SMM installation.
pub(crate) fn plan_einj(
    ctx: &soc::Ctx,
    e820: &E820State,
    reserved: &mut crate::flow::Reserved,
) -> Option<crate::sp3::smm::EinjConfig> {
    if ctx.generation == fstart_driver_amd::cpu::Silicon::Ssp
        && (1..=2).contains(&ctx.topology.sockets)
    {
        use fstart_driver_amd::{
            df::Target,
            psp::ras,
            smn::Smn,
            sp3::df::{self, v3},
        };
        let mut config = crate::sp3::smm::EinjConfig {
            sockets: ctx.topology.sockets,
            ..Default::default()
        };
        for socket in 0..config.sockets {
            config.df[usize::from(socket)] = fstart_driver_amd::df::node_address(socket, 4);
            config.buses[usize::from(socket)] = ctx
                .topology
                .root_bridges
                .iter()
                .find(|r| r.socket == socket && r.index == 0)?
                .bus_base;
        }
        let map = df::dram::read::<v3::Df3, _>(&ctx.df, 0, Target::Instance(v3::CCM0));
        config.targets =
            crate::sp3::einj::plan_targets(config.sockets, e820.entries(), &map.rules, |address| {
                ras::memory_target(
                    address,
                    config.sockets,
                    |socket, instance, reg| unsafe {
                        ctx.df
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
                        Smn::on_bus(ctx.pci, config.buses[usize::from(socket)]).read32(address)
                    },
                )
            });
        for page in scratch_pages(&config).filter(|&page| page != crate::sp3::einj::SCRATCH) {
            reserved
                .push(page..page + crate::sp3::einj::SCRATCH_SIZE)
                .ok()
                .or_halt("EINJ scratch reservation");
        }
        return Some(config);
    }
    let _ = ctx;
    None
}

/// The distinct scratch pages of `config`'s targets.
fn scratch_pages(config: &crate::sp3::smm::EinjConfig) -> impl Iterator<Item = u64> + '_ {
    let page = |target: u64| target & !(crate::sp3::einj::SCRATCH_SIZE - 1);
    (config.targets.iter().enumerate())
        .filter(|(_, target)| **target != 0)
        .filter(move |&(i, &target)| {
            !config.targets[..i]
                .iter()
                .any(|&earlier| earlier != 0 && page(earlier) == page(target))
        })
        .map(move |(_, &target)| page(target))
}

/// Clear the scratch pages of `config`, once they are out of the memory
/// map ([`plan_einj`]) and before the SMM handler gets them.
pub(crate) fn clear_einj_scratch(config: &crate::sp3::smm::EinjConfig) {
    for page in scratch_pages(config) {
        // SAFETY: the legacy reserved scratch or a page the flow removed
        // from usable RAM; all DRAM is identity mapped by the reset image.
        unsafe {
            core::ptr::write_bytes(page as *mut u8, 0, crate::sp3::einj::SCRATCH_SIZE as usize)
        };
    }
}
