// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin DRAM stage: the PSP released the BSP at the reset vector of
//! the image it copied to DRAM. `psp_reset.S` enters long mode and continues
//! at the architectural `_start_ram` (stack, BSS, data, IDT), which calls the
//! board's `fstart_main`.
use super::{Board, BoardCtx, acpi, boot, cpu::ZeppelinCpu, layout, memory, pci};
use fstart_core::services::memory_detect::E820State;
use fstart_driver_amd::{
    fam17h::{
        apob::{Apob, MemoryMap},
        cache,
        msr::{self, Msr, NativeMsr},
        tsc,
    },
    fch::{self as common_fch, early},
    lpc::Lpc,
    zeppelin::{
        self as soc, MAX_DIES, Package, Revision, ccx, dxio, fabric, fch, nbio, psp, smu,
        topology::{self, Die},
    },
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

fn post(code: u8) {
    // SAFETY: port 0x80 is decoded to the BMC's POST snoop.
    unsafe { fstart_core::pio::outb(0x80, code) };
}

fn halt() -> ! {
    fstart_arch::x86::boot::halt()
}

fn fail(what: &str) -> ! {
    fstart_log::error!("zeppelin: {} failed, halting", what);
    halt()
}

static mut CONSOLE: core::mem::MaybeUninit<Ns16550> = core::mem::MaybeUninit::uninit();

/// DRAM the SMU and the DXIO firmware use with DMA: firmware BSS in the
/// reserved early region.
#[repr(C, align(4096))]
struct SmuPages([[u8; 4096]; 2 * MAX_DIES]);
static mut SMU_PAGES: SmuPages = SmuPages([[0; 4096]; 2 * MAX_DIES]);
#[repr(C, align(64))]
struct SmuPolicy([u8; 0x80]);
static mut SMU_POLICY: SmuPolicy = SmuPolicy([0; 0x80]);
static mut DXIO_BUFFERS: dxio::Buffers = dxio::Buffers::new();

struct Clock {
    mhz: u32,
}
impl Clock {
    fn delay_us(&self, us: u32) {
        tsc::delay_us(self.mhz, us)
    }
    fn now_us(&self) -> u64 {
        // SAFETY: RDTSC has no side effects.
        unsafe { core::arch::x86_64::_rdtsc() / u64::from(self.mhz) }
    }
}

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

fn apob() -> Result<MemoryMap, fstart_driver_amd::fam17h::apob::Error> {
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
    nonpci: [u32; MAX_DIES],
    revision: Revision,
    package: Package,
    clock: Clock,
    e820: E820State,
}

fn run<B: Board>() -> ! {
    post(0x30);
    // SAFETY: BSP only, single-threaded cold boot.
    let mut msr = unsafe { NativeMsr::new() };
    // MMCONF first, everything else uses it.
    msr::enable_ecam(&mut msr, soc::ECAM, soc::ECAM_BUSES);
    fstart_pci::ecam::init(soc::ECAM as usize);
    let pci = GlobalEcam;
    // Before the console: ACPI MMIO, LPC/SPI, SMBus, CF9, legacy I/O.
    // SAFETY: exclusive FCH programming before the console.
    unsafe { common_fch::enable_acpimmio() };
    let mut early_fch = unsafe { early::Native::new(pci) };
    let early_ok = fch::pre_init(&mut early_fch, true).is_ok();
    {
        // SAFETY: exclusive LPC decode programming before the console.
        let mut lpc = unsafe { Lpc::new(pci) };
        lpc.enable_port80();
        B::early_init(&mut lpc);
    }
    post(0x31);
    console::<B>();
    if !early_ok {
        fail("FCH pre-init");
    }
    early::set_power_failure_state(&mut early_fch, B::POWER_FAILURE);

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
    let dies = topology::die_count(&pci);
    let Some(revision) = revision.filter(|_| package.dies() == Some(dies) && mhz != 0) else {
        fail("CPU identification")
    };
    let clock = Clock { mhz };
    let Ok((top_mem, top_mem2)) = msr::memory_limits(&mut msr) else {
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
    let e820 = match memory::e820(&map, top_mem) {
        Ok(e820) => e820,
        Err(_) => fail("memory map"),
    };

    // The early cache plan: WB DRAM, WP flash.
    let address_bits = (fstart_arch::x86::cpuid(0x8000_0008).0 & 0xff) as u8;
    let mtrrs = (msr.read(0xfe) & 0xff) as usize;
    let installed = cache::Plan::new(memory::low_top(&map, top_mem), address_bits, mtrrs)
        .and_then(|plan| cache::install(&mut msr, &plan));
    if installed.is_err() {
        fail("MTRR setup");
    }
    post(0x32);

    // SAFETY: BSP-only silicon init serializes all die register accesses.
    let dies: Vec<Die<GlobalEcam>, MAX_DIES> = unsafe { topology::dies(pci) }.collect();
    let mut silicon = Silicon {
        nonpci: [0; MAX_DIES],
        dies,
        revision,
        package,
        clock,
        e820,
    };
    early_silicon::<B>(&mut silicon, top_mem, top_mem2, &map);
    post(0x40);
    late_silicon::<B>(&mut silicon)
}

/// The silicon init before MP: AmdSocSp4ZpPei, AmdFabricZpPei, FchTaishanPei,
/// AmdNbioBaseZPPei, AmdCcxZenZpPei.
fn early_silicon<B: Board>(s: &mut Silicon, top_mem: u64, top_mem2: Option<u64>, map: &MemoryMap) {
    let clock = Clock { mhz: s.clock.mhz };
    let mut delay = |us| clock.delay_us(us);
    let dies = s.dies.as_slice();
    let count = dies.len() as u8;
    // DF MMIO/IO distribution, then the per-die DF init.
    let tom2 = top_mem2.unwrap_or(0);
    let Ok(plan) = fabric::plan(top_mem, tom2, u64::from(soc::ECAM_BUSES) << 20, count) else {
        fail("DF MMIO plan")
    };
    for w in &plan.mmio {
        fstart_log::debug!(
            "DF MMIO pair {}: {}..{} -> die {}",
            w.pair,
            Hex(w.base),
            Hex(w.end()),
            w.die
        );
    }
    {
        let mut ports: Vec<_, MAX_DIES> = dies.iter().map(|d| d.df()).collect();
        let mut dfs: Vec<&mut dyn fstart_driver_amd::fam17h::registers::Fabric, MAX_DIES> =
            ports.iter_mut().map(|p| p as _).collect();
        fabric::program(&plan, &mut dfs);
    }
    for die in dies {
        match fabric::nonpci_mmio_base(&mut dies[0].df(), die.index()) {
            Ok(base) => s.nonpci[usize::from(die.index())] = base,
            Err(_) => fail("non-PCI MMIO pool"),
        }
    }
    let apob = Apob::parse_with_capacity(apob_bytes(), soc::APOB_SIZE).ok();
    for die in dies {
        fabric::early_init(&mut die.df(), count, s.revision);
    }
    if let Some(apob) = &apob {
        for die in dies {
            if let Some((rate, size)) = fabric::scrub_init(&mut die.df(), die.index(), apob) {
                fstart_log::debug!("DF {}: scrub rate {} for {}", die.index(), rate, Hex(size));
            }
        }
    }
    let _ = map;

    // FchTaishanPei, then the board PEIMs that wait for its SMBus PPI.
    // SAFETY: BSP-only FCH programming.
    let mut bus = unsafe { fch::NativeBus::new() };
    fch::reset_init(&mut bus, s.revision);
    fch::sata::reset(&mut dies[0].smn(), &mut delay);
    let nonpci = s.nonpci;
    fch::multi::reset(&mut bus, dies, s.package, |die| nonpci[usize::from(die)], &mut delay);
    B::fch_reset_done(&mut BoardCtx {
        dies,
        delay_us: &mut delay,
    });

    // AmdNbioBaseZPPei, AmdCcxZenZpPei.
    for die in dies {
        nbio::early_init(die, s.revision);
        psp::init_bar(die, s.nonpci[usize::from(die.index())]);
    }
    let thread = ccx::Thread::from_cpuid(
        fstart_arch::x86::cpuid(1).1,
        fstart_arch::x86::cpuid(0x8000_001e).1,
    );
    // SAFETY: BSP-only MSR programming.
    ccx::bsp_init(&mut unsafe { NativeMsr::new() }, s.revision, thread);
    smu::print_name(&dies[0], &mut delay);
    for die in dies {
        nbio::tom_init(die, top_mem as u32, top_mem2);
    }
}

/// The native silicon init, MP init and the devices.
fn late_silicon<B: Board>(s: &mut Silicon) -> ! {
    let clock = Clock { mhz: s.clock.mhz };
    let mut delay = |us| clock.delay_us(us);
    let mut now = || clock.now_us();
    let dies = s.dies.as_slice();
    let mut ioapics = [0u32; MAX_DIES];
    for die in dies {
        ioapics[usize::from(die.index())] = nbio::ioapic_init(die, s.nonpci[usize::from(die.index())]);
    }
    let psp_bars = psp::bars(dies);

    // AmdNbioPcieZPPei
    let mut descriptors: Vec<&'static [dxio::Descriptor], MAX_DIES> = Vec::new();
    for die in dies {
        let set = B::dxio_descriptors(
            &mut BoardCtx {
                dies,
                delay_us: &mut delay,
            },
            die.index(),
        );
        let _ = descriptors.push(set);
    }
    // SAFETY: BSP-only; the DMA buffer is reserved firmware BSS below 4 GiB.
    let trained = unsafe {
        dxio::init(
            dies,
            &descriptors,
            B::SUBSYSTEM_ID,
            &mut *core::ptr::addr_of_mut!(DXIO_BUFFERS),
            &mut now,
            &mut delay,
        )
    };
    let topology = match trained {
        Ok(topology) => topology,
        Err(_) => fail("DXIO training"),
    };
    for port in topology[0].ports.iter().flatten() {
        fstart_log::info!(
            "PCIe {}.{}: lanes {}-{} {}",
            port.device,
            port.function,
            port.start_lane,
            port.end_lane,
            if port.trained { "trained" } else { "no link" }
        );
    }
    B::dxio_training_done(&mut BoardCtx {
        dies,
        delay_us: &mut delay,
    });

    // FchTaishanPei USB, then the DXE drivers.
    fch::usb::init(&mut dies[0].smn(), 0, s.package, &mut delay);
    fch::multi::usb(dies, &B::FCH_DEVICES, s.package, &mut delay);
    // SAFETY: the pages and the policy are reserved firmware BSS.
    let smu_ok = unsafe {
        use zerocopy::IntoBytes;
        let pages = &mut *core::ptr::addr_of_mut!(SMU_PAGES);
        let policy = &mut *core::ptr::addr_of_mut!(SMU_POLICY);
        let table = smu::PolicyTable::SP4;
        policy.0[..table.as_bytes().len()].copy_from_slice(table.as_bytes());
        let page = |i: usize| pages.0[i].as_ptr() as u64;
        let buffers = smu::DmaBuffers {
            policy: policy.0.as_ptr() as u64,
            agm_log: core::array::from_fn(page),
            perfmon: core::array::from_fn(|i| page(MAX_DIES + i)),
        };
        smu::init(dies, &buffers, &mut delay)
    };
    if smu_ok.is_err() {
        fstart_log::error!("zeppelin: SMU initialization failed");
    }
    let cac_weights = smu::cac_weights(&dies[0], &mut delay).ok();
    let apob = Apob::parse_with_capacity(apob_bytes(), soc::APOB_SIZE).ok();
    let sockets = fabric::sockets(&mut dies[0].df());
    for die in dies {
        if let Some(apob) = &apob {
            fabric::late_init(&mut die.df(), dies.len() as u8, sockets, s.revision, apob);
        }
        fabric::route_vga(&mut die.df(), 0);
    }
    for die in dies {
        nbio::pre_enumeration_init(die);
    }
    // SAFETY: BSP-only FCH programming.
    let mut bus = unsafe { fch::NativeBus::new() };
    let mut pm = unsafe { common_fch::Fch::new() };
    pm.acpi_decode(fch::ACPI_PORTS);
    fch::env_init(&mut bus, &mut dies[0].smn(), B::SUBSYSTEM_ID);
    if B::FCH_DEVICES[0].sata {
        fch::sata::init(
            &mut bus,
            0,
            &mut dies[0].smn(),
            fch::Acpimmio::DIE0,
            B::SUBSYSTEM_ID,
            &mut delay,
        );
    }
    fch::multi::xgbe_init(&mut dies[0].smn());
    let nonpci = s.nonpci;
    fch::multi::env(
        &mut bus,
        dies,
        &B::FCH_DEVICES,
        |die| nonpci[usize::from(die)],
        B::SUBSYSTEM_ID,
        &mut delay,
    );
    post(0x41);

    // MP: microcode and the CCX init on every CPU, the SMU releases the APs.
    let threads = smu::thread_count(dies);
    let flash = unsafe {
        core::slice::from_raw_parts(layout::FLASH_BASE as *const u8, layout::FLASH_SIZE as usize)
    };
    let policy = ccx::CpuPolicy {
        revision: s.revision,
        psp_bars,
        cac_weights,
    };
    // SAFETY: BSP only, before the APs start.
    let cpu = unsafe { ZeppelinCpu::new(policy, flash, fstart_arch::x86::cpuid(1).0) };
    let drivers: [&dyn fstart_arch::x86::mp::CpuDriver; 1] = [&cpu];
    let mut launch = |aps: u16| {
        // SAFETY: the reset vector sends APs to the SIPI trampoline the MP
        // engine installed before calling this.
        unsafe { smu::launch_aps(dies, usize::from(aps), &mut |us| clock.delay_us(us)) }
        .map_err(
            |error| {
                if let smu::Failure::ApCount { started, requested } = error {
                    fstart_log::error!("zeppelin: started {} of {} APs", started, requested);
                }
                1
            },
        )
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
                ZeppelinCpu::errors()
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
    if smu::aps_ready(dies, &mut delay).is_err() {
        fstart_log::error!("zeppelin: SMU messages after AP launch failed");
    }
    if smu::ccx_done(dies, &mut delay).is_err() {
        fstart_log::error!("zeppelin: SMU CCX-done messages failed");
    }
    post(0x42);

    // PCI: the interrupt router first, then the bus.
    if fch::irq::write_routing(&mut bus, B::IRQ_ROUTES).is_err() {
        fail("PCI interrupt routing");
    }
    let root = pci::Root::new(&dies[0], soc::ECAM_BUSES as u8 - 1, s.nonpci[0]);
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
    pm.acpi_ports_without_smm(fch::ACPI_PORTS);
    B::mainboard_final(&mut BoardCtx {
        dies,
        delay_us: &mut delay,
    });

    // IOAPICs: the FCH's (GSI 0-23, ID 0) in virtual-wire mode, then one
    // per die's NBIO (ID 1 + die), all masked.
    pm.enable_hpet_ioapic();
    // SAFETY: decoded IOAPIC MMIO, BSP only.
    let fch_ioapic = unsafe { fstart_arch::x86::ioapic::IoApic::new(FCH_IOAPIC as usize) };
    fch_ioapic.set_id(0);
    fch_ioapic.mask_all();
    fch_ioapic.route_i8259(apic_ids.first().copied().unwrap_or(0) as u8);
    for die in dies {
        let ioapic =
            unsafe { fstart_arch::x86::ioapic::IoApic::new(ioapics[usize::from(die.index())] as usize) };
        ioapic.set_id(1 + die.index());
        ioapic.mask_all();
    }

    let rsdp = acpi_tables::<B>(s, &ecam, &apic_ids, &ioapics);
    // ACPI mode without SMM: the firmware enables SCI itself.
    // SAFETY: the PM1 control block the FCH decodes at this port.
    unsafe {
        let control = fstart_core::pio::inw(soc::acpi_io::PM1_CNT);
        fstart_core::pio::outw(soc::acpi_io::PM1_CNT, control | 1);
    }

    // ReadyToBoot of the vendor firmware.
    if let Some(sata) = ecam.devices().find(|a| {
        ecam.device(*a)
            .is_some_and(|d| d.read32(0) == 0x7901_1022)
    }) {
        fch::sata::late(&mut dies[0].smn(), 0, &pci_access(), sata);
    }
    fch::multi::late(dies, |_| None);
    fch::late_init(&mut bus);
    B::ready_to_boot(&mut BoardCtx {
        dies,
        delay_us: &mut delay,
    });
    for die in dies {
        fabric::disable_ext_cfg(&mut die.df());
    }
    // No PSP LockDFReg: sent here it breaks device DMA (the xHCI halts with
    // a host system error on its first command). The vendor firmware sends
    // it only after the PSP got its SMM info, and this port has no SMM yet.
    post(0x43);

    if boot::mount(&s.e820).is_err() {
        fail("boot media");
    }
    if boot::map_ram(&s.e820).is_err() {
        fail("page tables");
    }
    if !fstart_arch::x86::mp::park_aps_for_payload() {
        fail("parking the APs");
    }
    // SAFETY: CONSOLE was initialized at entry and lives forever.
    let console = unsafe { (*core::ptr::addr_of!(CONSOLE)).assume_init_ref() };
    fstart_log::info!("zeppelin: starting the payload");
    post(0x44);
    let handoff = boot::Handoff {
        console,
        e820: core::mem::replace(&mut s.e820, E820State::new()),
        rsdp,
    };
    use fstart_stage::payload::MainstagePayload;
    fstart_stage::payload::BuildSelectedPayload::boot(handoff)
}

const FCH_IOAPIC: u32 = 0xfec0_0000;
const HPET: usize = 0xfed0_0000;

fn pci_access() -> GlobalEcam {
    GlobalEcam
}

/// Build the ACPI tables in reserved firmware memory; returns the RSDP.
fn acpi_tables<B: Board>(
    s: &Silicon,
    ecam: &fstart_pci::PciEcam,
    apic_ids: &[u32],
    ioapics: &[u32; MAX_DIES],
) -> Option<u64> {
    #[repr(C, align(4096))]
    struct Tables([u8; 0x2_0000]);
    static mut TABLES: Tables = Tables([0; 0x2_0000]);

    let io: Vec<core::ops::RangeInclusive<u16>, 8> = ecam
        .windows()
        .iter()
        .filter(|w| w.kind == fstart_pci::PciWindowKind::Io && w.size != 0)
        .map(|w| w.base as u16..=(w.end() - 1) as u16)
        .collect();
    let mmio: Vec<core::ops::RangeInclusive<u64>, 8> = ecam
        .windows()
        .iter()
        .filter(|w| w.kind == fstart_pci::PciWindowKind::Mmio && w.size != 0)
        .map(|w| w.base..=w.end() - 1)
        .collect();
    let bridges: Vec<Vec<acpi::Bridge, 32>, MAX_DIES> = s
        .dies
        .iter()
        .map(|die| {
            ecam.devices()
                .filter(|a| a.bus() == die.bus())
                .filter_map(|a| {
                    let device = ecam.device(a)?;
                    (device.read32(8) >> 16 == 0x0604).then_some(acpi::Bridge {
                        devfn: (a.device() << 3) | a.function(),
                        pin: (device.read32(0x3c) >> 8) as u8,
                    })
                })
                .collect()
        })
        .collect();
    let dies: Vec<acpi::DieAcpi<'_>, MAX_DIES> = s
        .dies
        .iter()
        .zip(&bridges)
        .map(|(die, bridges)| acpi::DieAcpi {
            buses: die.bus()..=soc::ECAM_BUSES as u8 - 1,
            ioapic_id: 1 + die.index(),
            ioapic_base: ioapics[usize::from(die.index())],
            io: &io,
            mmio: &mmio,
            vga: die.index() == 0,
            bridges,
        })
        .collect();
    let board = B::acpi();
    // SAFETY: HPET general capabilities, decoded by the FCH.
    let hpet_id = unsafe { fstart_core::mmio::read32(HPET as *const u32) };
    let tables = acpi::build(&acpi::Inputs {
        apic_ids,
        dies: &dies,
        ecam_base: soc::ECAM,
        ecam_buses: soc::ECAM_BUSES,
        hpet_id,
        fch_intx_gsi: [16, 17, 18, 19],
        board: &board,
    })
    .ok()?;
    // SAFETY: firmware BSS in the reserved early region, written once.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(TABLES)).0 };
    let address = storage.as_ptr() as u64;
    match fstart_acpi::platform::assemble_into(
        storage,
        address,
        &tables.platform,
        &tables.dsdt_aml,
        &tables.extra,
    ) {
        Ok(length) => {
            fstart_log::info!("ACPI: {} bytes at {}", length as u32, Hex(address));
            Some(address)
        }
        Err(_) => {
            fstart_log::error!("ACPI: tables don't fit");
            None
        }
    }
}
