// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin DRAM stage: the PSP released the BSP at the reset vector of
//! the image it copied to DRAM. `psp_reset.S` enters long mode and continues
//! at the architectural `_start_ram` (stack, BSS, data, IDT), which calls the
//! board's `fstart_main`.
use super::{Board, BoardCtx, acpi, boot, layout, memory, pci, smm};
use crate::cpu::{Fam17hCpu, PatchPage};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_core::services::memory_detect::E820State;
use fstart_driver_amd::{
    fam17h::{
        apob::{Apob, MemoryMap},
        cache,
        ccx::Thread,
        df, iohc, msr,
        topology,
        tsc::{self, Tsc},
    },
    fch::{self as common_fch, Acpimmio, Fch, NativeBus, early},
    lpc::Lpc,
    zeppelin::{
        self as soc, Die, MAX_DIES, Package, Revision, Zeppelin, ccx, dxio, fabric, fch, iommu,
        nbio, psp, smu,
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
    clock: Tsc,
    e820: E820State,
    tseg: u64,
}

fn run<B: Board>() -> ! {
    post(0x30);
    // SAFETY: BSP only, single-threaded cold boot.
    let msr = unsafe { NativeMsr::new() };
    // MMCONF first, everything else uses it.
    msr::enable_ecam(&msr, soc::ECAM, soc::ECAM_BUSES);
    fstart_pci::ecam::init(soc::ECAM as usize);
    let pci = GlobalEcam;
    // Before the console: ACPI MMIO, LPC/SPI, SMBus, CF9, legacy I/O.
    // SAFETY: exclusive FCH programming before the console.
    unsafe { common_fch::enable_acpimmio() };
    // SAFETY: exclusive FCH programming; ECAM is up.
    let die0_fch = Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0);
    let early_ok = fch::pre_init(&die0_fch, true).is_ok();
    {
        // SAFETY: exclusive LPC decode programming before the console.
        let lpc = unsafe { Lpc::new(die0_fch.bus()) };
        B::early_init(&lpc);
    }
    post(0x31);
    console::<B>();
    if !early_ok {
        fail("FCH pre-init");
    }
    early::set_power_failure_state(&die0_fch, B::POWER_FAILURE);

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
    let tseg = memory::tseg_base(&map, top_mem);
    let e820 = match memory::e820(&map, top_mem) {
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
        tseg,
    };
    early_silicon::<B>(&mut silicon, top_mem, top_mem2, &map);
    post(0x40);
    late_silicon::<B>(&mut silicon)
}

/// The silicon init before MP: AmdSocSp4ZpPei, AmdFabricZpPei, FchTaishanPei,
/// AmdNbioBaseZPPei, AmdCcxZenZpPei.
fn early_silicon<B: Board>(s: &mut Silicon, top_mem: u64, top_mem2: Option<u64>, map: &MemoryMap) {
    let clock = s.clock;
    let mut delay = clock;
    let dies = s.dies.as_slice();
    let count = dies.len() as u8;
    // DF MMIO/IO distribution, then the per-die DF init.
    let tom2 = top_mem2.unwrap_or(0);
    let Ok(plan) = df::plan(top_mem, tom2, u64::from(soc::ECAM_BUSES) << 20, count) else {
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
    df::program(&plan, dies);
    for die in dies {
        match fabric::nonpci_mmio_base(&dies[0].df(), die.index()) {
            Some(base) => s.nonpci[usize::from(die.index())] = base,
            None => fail("non-PCI MMIO pool"),
        }
    }
    let apob = Apob::parse_with_capacity(apob_bytes(), soc::APOB_SIZE).ok();
    for die in dies {
        fabric::early_init(&die.df(), count, s.revision);
    }
    if let Some(apob) = &apob {
        for die in dies {
            if let Some((rate, size)) = fabric::scrub_init(&die.df(), die.index(), apob) {
                fstart_log::debug!("DF {}: scrub rate {} for {}", die.index(), rate, Hex(size));
            }
        }
    }
    let _ = map;

    // FchTaishanPei, then the board PEIMs that wait for its SMBus PPI.
    // SAFETY: BSP-only FCH programming.
    let die0_fch = Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0);
    fch::reset_init(&die0_fch, s.revision);
    fch::sata::reset(&dies[0].smn(), &mut delay);
    let nonpci = s.nonpci;
    fch::multi::reset(
        die0_fch.bus(),
        dies,
        s.package,
        |die| nonpci[usize::from(die)],
        &mut delay,
    );
    B::fch_reset_done(&mut BoardCtx {
        dies,
        clock,
    });

    // AmdNbioBaseZPPei, AmdCcxZenZpPei.
    for die in dies {
        nbio::early_init(die, s.revision);
        // A locked BAR gets reported as a fixed resource by the root complex.
        let psp = s.nonpci[usize::from(die.index())] + soc::NONPCI_PSP;
        if let Err(bar) = iohc::map_psp(&die.smn(), psp) {
            fstart_log::error!("PSP BAR of die {} is locked at {}", die.index(), Hex(bar.into()));
        }
    }
    // SAFETY: BSP-only MSR programming.
    ccx::bsp_init(&unsafe { NativeMsr::new() }, s.revision, Thread::current());
    smu::print_name(&dies[0], &mut delay);
    for die in dies {
        iohc::dram_map(die, top_mem as u32, top_mem2);
    }
}

/// The native silicon init, MP init and the devices.
fn late_silicon<B: Board>(s: &mut Silicon) -> ! {
    let clock = s.clock;
    let mut delay = clock;
    let dies = s.dies.as_slice();
    let mut ioapics = [0u32; MAX_DIES];
    for die in dies {
        // The NBIO IOAPIC 1 MiB below the top of the pool (0xfeb00000 on
        // die 0). The vendor firmware sets its ID to 0x81 + die; the ACPI
        // code assigns the IDs.
        let nonpci = s.nonpci[usize::from(die.index())];
        let ioapic = nonpci + soc::NONPCI_IOAPIC;
        iohc::IOAPIC_BAR.map(&die.smn(), ioapic);
        ioapics[usize::from(die.index())] = ioapic;
        iommu::init_bar(die, nonpci + soc::NONPCI_IOMMU);
    }
    let psp_bars = psp::bars(dies);

    // AmdNbioPcieZPPei
    let mut descriptors: Vec<&'static [dxio::Descriptor], MAX_DIES> = Vec::new();
    for die in dies {
        let set = B::dxio_descriptors(
            &mut BoardCtx {
                dies,
                clock,
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
        clock,
    });

    // FchTaishanPei USB, then the DXE drivers.
    fch::usb::init(&dies[0].smn(), 0, s.package, &mut delay);
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
    let sockets = fabric::sockets(&dies[0].df());
    for die in dies {
        if let Some(apob) = &apob {
            fabric::late_init(&die.df(), dies.len() as u8, sockets, s.revision, apob);
        }
        df::route_vga(&die.df(), dies[0].ioms_fabric_id());
    }
    for die in dies {
        nbio::pre_enumeration_init(die);
    }
    // SAFETY: BSP-only FCH programming.
    let die0_fch = Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0);
    die0_fch.acpi_decode(fch::ACPI_PORTS);
    fch::env_init(&die0_fch, &dies[0].smn(), B::SUBSYSTEM_ID);
    if B::FCH_DEVICES[0].sata {
        fch::sata::init(&die0_fch, 0, &dies[0].smn(), B::SUBSYSTEM_ID, &mut delay);
    }
    fch::multi::xgbe_init(&dies[0].smn());
    let nonpci = s.nonpci;
    fch::multi::env(
        die0_fch.bus(),
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
    // SAFETY: BSP only, before the APs start; the page is reserved BSS.
    let cpu = unsafe {
        Fam17hCpu::<Zeppelin, _>::new(
            "AMD Zeppelin",
            flash,
            &mut *core::ptr::addr_of_mut!(MICROCODE),
            move |msr: &NativeMsr, thread| ccx::cpu_init(msr, &policy, thread),
        )
    };
    let drivers: [&dyn fstart_arch::x86::mp::CpuDriver; 1] = [&cpu];
    let mut launch = |aps: u16| {
        // SAFETY: the reset vector sends APs to the SIPI trampoline the MP
        // engine installed before calling this.
        unsafe { smu::launch_aps(dies, usize::from(aps), &mut { clock }) }
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
    if smu::aps_ready(dies, &mut delay).is_err() {
        fstart_log::error!("zeppelin: SMU messages after AP launch failed");
    }
    if smu::ccx_done(dies, &mut delay).is_err() {
        fstart_log::error!("zeppelin: SMU CCX-done messages failed");
    }
    let tseg = match smm::install::<B>(&mp, s.tseg) {
        Ok(tseg) => tseg,
        Err(error) => {
            fstart_log::error!("zeppelin: SMM: {}", error);
            fail("SMM installation")
        }
    };
    // The PSP gets the SMM info now, LockDFReg at ready-to-boot and BootDone
    // right before the payload.
    let mut psp = smm::psp_mailbox(psp_bars[0], &tseg, &mut delay);
    post(0x42);

    // PCI: the interrupt router first, then the bus.
    if fch::irq::write_routing(die0_fch.bus(), B::IRQ_ROUTES).is_err() {
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
    die0_fch.acpi_ports(fch::ACPI_PORTS);
    B::mainboard_final(&mut BoardCtx {
        dies,
        clock,
    });

    // IOAPICs: the FCH's (GSI 0-23, ID 0) in virtual-wire mode, then one
    // per die's NBIO (ID 1 + die), all masked.
    die0_fch.enable_hpet_ioapic();
    // SAFETY: decoded IOAPIC MMIO, BSP only.
    let fch_ioapic =
        unsafe { fstart_arch::x86::ioapic::IoApic::new(common_fch::IOAPIC as usize) };
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

    // ReadyToBoot of the vendor firmware.
    if let Some(sata) = ecam.devices().find(|a| {
        ecam.device(*a)
            .is_some_and(|d| d.read32(0) == 0x7901_1022)
    }) {
        fch::sata::late(&dies[0].smn(), 0, pci_access(), sata);
    }
    fch::multi::late(dies, |_| None);
    fch::late_init(&die0_fch);
    B::ready_to_boot(&mut BoardCtx {
        dies,
        clock,
    });
    for die in dies {
        fabric::disable_ext_cfg(&die.df());
    }
    // After the last DF write; it needs the SMM info the PSP got after MP.
    if let Some(psp) = psp.as_mut() {
        match psp.lock_df_registers(&mut delay) {
            Ok(()) => fstart_log::info!("PSP: DF registers locked"),
            Err(error) => fstart_log::error!("PSP: LockDFReg failed: {}", error.name()),
        }
    }
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
    // Afterwards the PSP only takes commands with SMM buffers.
    if let Some(psp) = psp.as_mut() {
        // SAFETY: the firmware is done with the CPUs and SMM is locked.
        match unsafe { psp.boot_done(&mut delay) } {
            Ok(()) => fstart_log::info!("PSP: boot done"),
            Err(error) => fstart_log::error!("PSP: BootDone failed: {}", error.name()),
        }
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
                        devfn: fstart_pci::devfn(a.device(), a.function()),
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
            // SAFETY: the IOMMU MMIO is in the identity-mapped pool.
            iommu: unsafe { iommu::read(die) },
        })
        .collect();
    let board = B::acpi();
    // SAFETY: HPET general capabilities, decoded by the FCH.
    let hpet_id = unsafe { fstart_core::mmio::read32(common_fch::HPET as usize as *const u32) };
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
