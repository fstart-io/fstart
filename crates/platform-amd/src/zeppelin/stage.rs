// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin DRAM stage: the PSP released the BSP at the reset vector of
//! the image it copied to DRAM. `psp_reset.S` enters long mode and continues
//! at the architectural `_start_ram` (stack, BSS, data, IDT), which calls the
//! board's `fstart_main`.
use super::{
    Board, BoardCtx,
    layout::LAYOUT,
    silicon::{self, Dma, Identity, SmuPages, SmuPolicy},
};
use crate::noncar::{boot, layout::Region, memory, pci, psp, smm};
use crate::cpu::{Fam17hCpu, PatchPage};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_core::services::memory_detect::E820State;
use fstart_driver_amd::{
    apob::{Apob, MemoryMap},
    ccx::{
        self, cache, msr,
        tsc::{self, Tsc},
    },
    df, dxio,
    fch::{self, Acpimmio, Fch, NativeBus, lpc::Lpc, smi::FchSmi, taishan},
    smu::threads::{self, Policy},
    soc::zeppelin::{self as soc, Die, MAX_DIES, Package, Revision, Zeppelin},
};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::Hex;
use fstart_pci::ecam::GlobalEcam;
use heapless::Vec;

core::arch::global_asm!(
    include_str!("../../asm/psp_reset.S"),
    ap_reset_segment = const {
        let address = fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS;
        assert!(address & 15 == 0 && address <= 0xffff0);
        address >> 4
    },
);
core::arch::global_asm!(
    ".code64",
    ".global _fstart_amd_long_mode_entry",
    "_fstart_amd_long_mode_entry:",
    "jmp _start_ram",
);

/// Platform-owned stage program for a Zeppelin board.
pub struct Program<B>(core::marker::PhantomData<B>);
impl<B: Board> fstart_stage::StageProgram for Program<B> {
    fn run_stage(_handoff: usize) -> ! {
        run::<B>()
    }
}

fn halt() -> ! {
    fstart_arch::x86::boot::halt()
}

fn fail(what: &str) -> ! {
    fstart_log::error!("zeppelin: {} failed, halting", what);
    halt()
}

/// 8 MiB of TSEG.
const TSEG_SIZE: u32 = 8 << 20;
const FFS: Region = match LAYOUT.ffs {
    Some(ffs) => ffs,
    None => panic!("Zeppelin images have an FFS"),
};

static mut CONSOLE: core::mem::MaybeUninit<Ns16550> = core::mem::MaybeUninit::uninit();

/// DRAM the SMU and the DXIO firmware use with DMA: firmware BSS in the
/// reserved early region.
static mut SMU_PAGES: SmuPages = SmuPages([[0; 4096]; 2 * MAX_DIES]);
static mut SMU_POLICY: SmuPolicy = SmuPolicy([0; 0x80]);
static mut DXIO_BUFFERS: dxio::zeppelin::Buffers = dxio::zeppelin::Buffers::new();
/// The microcode patch the CPUs load: firmware BSS in the reserved region.
static mut MICROCODE: core::mem::MaybeUninit<PatchPage> = core::mem::MaybeUninit::uninit();

fn console<B: Board>() {
    let Ok(console) = Ns16550::new(B::console()) else { halt() };
    // SAFETY: written once here, before anything reads it; lives forever.
    let console = unsafe {
        let slot = &mut *core::ptr::addr_of_mut!(CONSOLE);
        slot.write(console)
    };
    if console.init().is_err() {
        halt()
    }
    // SAFETY: the console is static and never moves.
    unsafe { fstart_log::init(console) };
}

fn apob() -> Result<MemoryMap, fstart_driver_amd::apob::Error> {
    Apob::parse_with_capacity(apob_bytes(), soc::APOB_SIZE)?.memory_map()
}

fn apob_bytes() -> &'static [u8] {
    // SAFETY: the PSP wrote the APOB here before releasing the BSP and the
    // memory map keeps it reserved.
    unsafe { core::slice::from_raw_parts(soc::APOB_BASE as *const u8, soc::APOB_SIZE) }
}

/// The silicon state the later phases need.
struct Silicon {
    dies: Vec<Die<GlobalEcam>, MAX_DIES>,
    state: silicon::State,
    clock: Tsc,
    e820: E820State,
    tseg: u64,
}

fn run<B: Board>() -> ! {
    // SAFETY: BSP only, single-threaded cold boot; the ECAM comes up before
    // the first config space access.
    let die0_fch = Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0);
    silicon::post(&die0_fch, 0x30);
    // SAFETY: BSP only, single-threaded cold boot.
    let msr = unsafe { NativeMsr::new() };
    // MMCONF first, everything else uses it.
    msr::enable_ecam(&msr, soc::ECAM, soc::ECAM_BUSES);
    fstart_pci::ecam::init(soc::ECAM as usize);
    let pci = GlobalEcam;
    // SAFETY: exclusive FCH programming before the console.
    unsafe { fch::enable_acpimmio() };
    let early_ok = silicon::fch_pre_init(&die0_fch).is_ok();
    {
        // SAFETY: exclusive LPC decode programming before the console.
        let lpc = unsafe { Lpc::new(die0_fch.bus()) };
        B::early_init(&lpc);
    }
    silicon::post(&die0_fch, 0x31);
    console::<B>();
    if !early_ok {
        fail("FCH pre-init");
    }
    silicon::fch_early_init(&die0_fch, B::CONFIG);

    let mhz = unsafe { tsc::frequency_mhz() }.unwrap_or(0);
    let (ext_eax, ext_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
    let package = Package::from_cpuid(ext_ebx);
    let revision = Revision::from_cpuid(ext_eax);
    fstart_log::info!(
        "fstart: AMD Zeppelin {}, package {}, TSC {} MHz",
        revision.map_or("unknown", Revision::name),
        package.name(),
        mhz
    );
    let dies = df::die_count(&pci);
    let Some(revision) = revision.filter(|_| package.dies() == Some(dies) && mhz != 0) else {
        fail("CPU identification")
    };
    let clock = Tsc::new(mhz);
    let Ok((top_mem, top_mem2)) = msr::memory_limits(&msr) else {
        fail("TOP_MEM from the PSP")
    };
    let Ok(map) = apob() else { fail("APOB") };
    fstart_log::info!(
        "zeppelin: {} die(s), TOM {}, TOM2 {}, APOB top {}",
        dies,
        Hex(top_mem),
        Hex(top_mem2.unwrap_or(0)),
        Hex(map.top)
    );
    let tseg = memory::tseg_base(&map, top_mem, TSEG_SIZE.into());
    let apob_region = Region::new(soc::APOB_BASE, soc::APOB_SIZE as u64);
    let e820 = match memory::e820(&map, top_mem, TSEG_SIZE.into(), &[LAYOUT.early, apob_region]) {
        Ok(e820) => e820,
        Err(_) => fail("memory map"),
    };

    // The early cache plan: WB DRAM, WP flash.
    let address_bits = fstart_arch::x86::physical_address_bits() as u8;
    // SAFETY: CPUID reported MTRRs; the cache plan rejects CPUs without.
    let mtrrs = unsafe { fstart_arch::x86::mtrr::variable_count() } as usize;
    let installed = cache::Plan::new(memory::low_top(&map, top_mem), address_bits, mtrrs)
        .and_then(|plan| cache::install(&msr, &plan));
    if installed.is_err() {
        fail("MTRR setup");
    }
    silicon::post(&die0_fch, 0x32);

    // SAFETY: BSP-only silicon init serializes all die register accesses.
    let dies: Vec<Die<GlobalEcam>, MAX_DIES> = unsafe { df::dies(pci) }.collect();
    let apob = Apob::parse_with_capacity(apob_bytes(), soc::APOB_SIZE).ok();
    let mut delay = clock;
    let ctx = || BoardCtx {
        dies: &dies,
        clock,
    };
    // SAFETY: the DMA memory is reserved firmware BSS below 4 GiB, handed
    // out once here.
    let dma = unsafe {
        Dma {
            smu_pages: &mut *core::ptr::addr_of_mut!(SMU_PAGES),
            smu_policy: &mut *core::ptr::addr_of_mut!(SMU_POLICY),
            dxio: &mut *core::ptr::addr_of_mut!(DXIO_BUFFERS),
        }
    };
    let state = silicon::pre_mp(
        &dies,
        &die0_fch,
        &msr,
        B::CONFIG,
        Identity { revision, package },
        (top_mem, top_mem2),
        apob.as_ref(),
        dma,
        &mut delay,
        || B::fch_reset_done(&mut ctx()),
        || B::dxio_training_done(&mut ctx()),
    );
    let state = match state {
        Ok(state) => state,
        Err(what) => fail(what),
    };
    let mut silicon = Silicon {
        dies,
        state,
        clock,
        e820,
        tseg,
    };
    late_silicon::<B>(&mut silicon, &die0_fch)
}

/// After the silicon: MP init and the devices.
fn late_silicon<B: Board>(s: &mut Silicon, die0_fch: &Fch<NativeBus>) -> ! {
    let clock = s.clock;
    let mut delay = clock;
    let dies = s.dies.as_slice();
    let ioapics = s.state.ioapics;

    // MP: microcode and the CCX init on every CPU, the SMU releases the APs.
    let Ok(threads) = threads::thread_count(dies, Policy::DEFAULT) else {
        fail("CPU topology")
    };
    let flash = unsafe {
        core::slice::from_raw_parts(LAYOUT.flash.base as *const u8, LAYOUT.flash.size as usize)
    };
    let policy = s.state.cpu_policy();
    // SAFETY: BSP only, before the APs start; the page is reserved BSS.
    let cpu = unsafe {
        Fam17hCpu::<Zeppelin, _>::new(
            "AMD Zeppelin",
            flash,
            &mut *core::ptr::addr_of_mut!(MICROCODE),
            move |msr: &NativeMsr, thread| ccx::zeppelin::cpu_init(msr, &policy, thread),
        )
    };
    let drivers: [&dyn fstart_arch::x86::mp::CpuDriver; 1] = [&cpu];
    let mut launch = |aps: u16| {
        // SAFETY: the reset vector sends APs to the SIPI trampoline the MP
        // engine installed before calling this.
        unsafe { threads::launch(dies, Policy::DEFAULT, usize::from(aps), &mut { clock }) }
            .map_err(|error| {
                if let threads::Error::ApCount { started, requested } = error {
                    fstart_log::error!("zeppelin: started {} of {} APs", started, requested);
                }
                1
            })
    };
    let mp = fstart_arch::x86::mp::mp_init_with_launcher(
        &fstart_arch::x86::mp::MpConfig {
            cpu_drivers: &drivers,
            num_cpus: threads as u16,
            max_cpus: 32,
        },
        &mut launch,
    );
    let mp = match mp {
        Ok(mp) => {
            fstart_log::info!(
                "zeppelin: {} CPUs up, {} CPU init errors",
                mp.num_cpus(),
                cpu.errors()
            );
            mp
        }
        Err(error) => {
            use fstart_arch::x86::mp::MpError;
            match error {
                MpError::NoApsResponded => fstart_log::error!("mp: no AP responded"),
                MpError::PartialBringup { expected, actual } => {
                    fstart_log::error!("mp: {} of {} APs responded", actual, expected)
                }
                MpError::PlatformLaunchFailed(_) => fstart_log::error!("mp: SMU release failed"),
                MpError::HardwareCpuCountExceedsCapacity { reported, .. } => {
                    fstart_log::error!("mp: {} CPUs responded", reported)
                }
                _ => fstart_log::error!("mp: setup failed"),
            }
            fail("MP init")
        }
    };
    let apic_ids: [core::sync::atomic::AtomicU32; 32] =
        [const { core::sync::atomic::AtomicU32::new(u32::MAX) }; 32];
    mp.scope(|scope| {
        scope.scatter(&|cpu: u32| {
            if let Some(slot) = apic_ids.get(cpu as usize) {
                slot.store(
                    fstart_arch::x86::cpuid(1).1 >> 24,
                    core::sync::atomic::Ordering::Release,
                );
            }
        })
    });
    let apic_ids: Vec<u32, 32> = apic_ids
        .iter()
        .map(|id| id.load(core::sync::atomic::Ordering::Acquire))
        .take(usize::from(mp.num_cpus()))
        .collect();
    silicon::cpus_up(dies, &mut delay);
    // SAFETY: BSP only after MP init; ACPI MMIO and the ACPI blocks are
    // decoded since the FCH init.
    let smi = unsafe { FchSmi::new(taishan::ACPI_PORTS, B::CONFIG.fch.smi_gevents) };
    let tseg = match smm::install("zeppelin", &mp, &smi, s.tseg, TSEG_SIZE) {
        Ok(tseg) => tseg,
        Err(error) => {
            fstart_log::error!("zeppelin: SMM: {}", error);
            fail("SMM installation")
        }
    };
    // The PSP gets the SMM info now, LockDFReg at ready-to-boot and BootDone
    // right before the payload.
    let mut psp = psp::notify_smm(s.state.psp_bars[0], &tseg, &mut delay);
    silicon::post(die0_fch, 0x42);

    // PCI: the interrupt router first, then the bus.
    if let Err(what) = silicon::route_interrupts(die0_fch, B::CONFIG) {
        fail(what);
    }
    let root = pci::Root::new(
        &dies[0],
        dies[0].bus()..=soc::ECAM_BUSES as u8 - 1,
        Region::new(soc::ECAM, u64::from(soc::ECAM_BUSES) << 20),
        Region::new(s.state.nonpci[0].into(), soc::NONPCI_MMIO_SIZE.into()),
    );
    let mut ecam = match fstart_pci::PciEcam::from_provider(&root) {
        Ok(ecam) => ecam,
        Err(_) => fail("PCI root"),
    };
    if ecam.enumerate_and_allocate().is_err() {
        fail("PCI enumeration");
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
    silicon::after_resources(die0_fch);
    B::mainboard_final(&mut BoardCtx {
        dies,
        clock,
    });

    // IOAPICs: the FCH's (GSI 0-23, ID 0) in virtual-wire mode, then one
    // per die's NBIO (ID 1 + die), all masked.
    die0_fch.enable_hpet_ioapic();
    // SAFETY: decoded IOAPIC MMIO, BSP only.
    let fch_ioapic =
        unsafe { fstart_arch::x86::ioapic::IoApic::new(fch::IOAPIC as usize) };
    fch_ioapic.set_id(0);
    fch_ioapic.mask_all();
    fch_ioapic.route_i8259(apic_ids.first().copied().unwrap_or(0) as u8);
    for die in dies {
        let ioapic =
            unsafe { fstart_arch::x86::ioapic::IoApic::new(ioapics[usize::from(die.index())] as usize) };
        ioapic.set_id(1 + die.index());
        ioapic.mask_all();
    }

    let rsdp = crate::noncar::acpi::install(
        &super::acpi::ACPI,
        dies,
        &ioapics,
        &ecam,
        crate::noncar::acpi::Ecam {
            base: soc::ECAM,
            buses: soc::ECAM_BUSES,
        },
        &apic_ids,
        &B::acpi(),
    );

    // ReadyToBoot of the vendor firmware.
    let sata = ecam.devices().find(|a| {
        ecam.device(*a)
            .is_some_and(|d| d.read32(0) == 0x7901_1022)
    });
    silicon::ready_to_boot(dies, die0_fch, sata, || {
        B::ready_to_boot(&mut BoardCtx { dies, clock })
    });
    // After the last DF write; it needs the SMM info the PSP got after MP.
    psp::lock_df_registers(psp.as_mut(), &mut delay);
    silicon::post(die0_fch, 0x43);

    if boot::mount(&s.e820, FFS).is_err() {
        fail("boot media");
    }
    if boot::map_ram(&s.e820).is_err() {
        fail("page tables");
    }
    if !fstart_arch::x86::mp::park_aps_for_payload() {
        fail("parking the APs");
    }
    // Afterwards the PSP only takes commands with SMM buffers.
    // SAFETY: the firmware is done with the CPUs and SMM is locked.
    unsafe { psp::boot_done(psp.as_mut(), &mut delay) };
    // SAFETY: CONSOLE was initialized at entry and lives forever.
    let console = unsafe { (*core::ptr::addr_of!(CONSOLE)).assume_init_ref() };
    fstart_log::info!("zeppelin: starting the payload");
    silicon::post(die0_fch, 0x44);
    let handoff = boot::Handoff {
        console,
        e820: core::mem::replace(&mut s.e820, E820State::new()),
        rsdp,
        ffs: FFS,
        pci_root: fstart_pci::PciRootInfo {
            segment: 0,
            ecam_base: soc::ECAM,
            bus_start: 0,
            bus_end: (soc::ECAM_BUSES - 1) as u8,
        },
    };
    use fstart_stage::payload::MainstagePayload;
    fstart_stage::payload::BuildSelectedPayload::boot(handoff)
}
