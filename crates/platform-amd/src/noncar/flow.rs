// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stage: the PSP released the BSP at the reset vector of the image it
//! copied to DRAM. `psp_reset.S` enters long mode and continues at the
//! architectural `_start_ram` (stack, BSS, data, IDT), which calls the
//! board's `fstart_main`.
use super::{
    Board, Ctx, Memory, Silicon, Soc, Stop, Tp1, acpi, boot, layout::Region, memory, pci, post, psp,
};
use fstart_arch::x86::{cpu::msr::NativeMsr, ioapic::IoApic};
use fstart_driver_amd::{
    apob::Apob,
    ccx::{cache, msr, tsc::Tsc},
    df::{self, Die, map},
    fch::{self, Acpimmio, Fch, NativeBus, lpc::Lpc},
};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::Hex;
use fstart_pci::{PciEcam, ecam::GlobalEcam};
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

/// The stage program of a noncar board.
pub struct Program<B>(core::marker::PhantomData<B>);
impl<B: Board> fstart_stage::StageProgram for Program<B> {
    fn run_stage(_handoff: usize) -> ! {
        run::<B::Soc, B>()
    }
}

fn halt() -> ! {
    fstart_arch::x86::boot::halt()
}

fn stop<S: Soc>(stop: Stop) -> ! {
    match stop {
        Stop::Failed(what) => fstart_log::error!("{}: {} failed, halting", S::NAME, what),
        Stop::Checkpoint(what) => {
            fstart_log::info!("{}: {} checkpoint reached, halting", S::NAME, what)
        }
    }
    halt()
}

fn fail<S: Soc>(what: &'static str) -> ! {
    stop::<S>(Stop::Failed(what))
}

static mut CONSOLE: core::mem::MaybeUninit<Ns16550> = core::mem::MaybeUninit::uninit();

fn console<B: Board>() {
    let Ok(console) = Ns16550::new(B::console()) else {
        halt()
    };
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

fn apob_bytes<S: Soc>() -> &'static [u8] {
    // SAFETY: the PSP wrote the APOB here before releasing the BSP and the
    // memory map keeps it reserved.
    unsafe { core::slice::from_raw_parts(S::APOB.base as *const u8, S::APOB.size as usize) }
}

fn run<S: Soc, B: Board<Soc = S>>() -> ! {
    // SAFETY: BSP only, single-threaded cold boot; the ECAM comes up before
    // the first config space access.
    let fch = Fch::new(unsafe { NativeBus::new() }, Acpimmio::DIE0);
    post(&fch, 0x30);
    // SAFETY: BSP only, single-threaded cold boot.
    let msr = unsafe { NativeMsr::new() };
    // MMCONF first, everything else uses it.
    msr::enable_ecam(&msr, S::ECAM, S::ECAM_BUSES);
    fstart_pci::ecam::init(S::ECAM as usize);
    let pci = GlobalEcam;
    // SAFETY: exclusive FCH programming before the console.
    unsafe { fch::enable_acpimmio() };
    let mhz = fstart_arch::x86::cpu::amd::msr::tsc_mhz(&msr).unwrap_or(0);
    let clock = core::num::NonZeroU32::new(mhz).map(Tsc::new);
    let pre_init = S::fch_pre_init(&fch, B::CONFIG, clock);
    {
        // SAFETY: exclusive LPC decode programming before the console.
        let lpc = unsafe { Lpc::new(fch.bus()) };
        B::early_init(&lpc);
    }
    post(&fch, 0x31);
    console::<B>();
    if let Err(what) = pre_init {
        fail::<S>(what);
    }

    let dies = df::die_count(&pci);
    let identity = match S::identify(&fch, B::CONFIG, mhz, dies) {
        Ok(identity) => identity,
        Err(what) => fail::<S>(what),
    };
    let Some(clock) = clock else {
        fail::<S>("CPU identification")
    };
    let Ok((top_mem, top_mem2)) = msr::memory_limits(&msr) else {
        fail::<S>("TOP_MEM from the PSP")
    };
    let apob = Apob::parse_with_capacity(apob_bytes::<S>(), S::APOB.size as usize).ok();
    let Some(map) = apob.as_ref().and_then(|apob| apob.memory_map().ok()) else {
        fail::<S>("APOB")
    };
    fstart_log::info!(
        "{}: {} die(s), TOM {}, TOM2 {}, APOB top {}",
        S::NAME,
        dies,
        Hex(top_mem),
        Hex(top_mem2.unwrap_or(0)),
        Hex(map.top)
    );
    let tseg_size = u64::from(S::TSEG_SIZE);
    let tseg = memory::tseg_base(&map, top_mem, tseg_size);
    let mut e820 = match memory::e820(&map, top_mem, tseg_size, &[S::LAYOUT.early, S::APOB]) {
        Ok(e820) => e820,
        Err(_) => fail::<S>("memory map"),
    };

    // The early cache plan: WB DRAM, WP flash.
    let address_bits = fstart_arch::x86::physical_address_bits() as u8;
    // SAFETY: CPUID reported MTRRs; the cache plan rejects CPUs without.
    let mtrrs = unsafe { fstart_arch::x86::mtrr::variable_count() } as usize;
    let installed = cache::Plan::new(memory::low_top(&map, top_mem), address_bits, mtrrs)
        .and_then(|plan| cache::install(&msr, &plan));
    if installed.is_err() {
        fail::<S>("MTRR setup");
    }
    post(&fch, 0x32);

    // The verified FFS, mounted before TP1: MP init loads the CPUs'
    // microcode from it.
    if let Some(ffs) = S::LAYOUT.ffs
        && boot::mount(&e820, ffs).is_err()
    {
        fail::<S>("boot media");
    }
    let microcode = fstart_stage::microcode_blob();

    // SAFETY: BSP-only silicon init serializes all die register accesses.
    let dies: Vec<Die<S, GlobalEcam>, { super::MAX_DIES }> = unsafe { df::dies(pci) }.collect();
    let ctx = Ctx {
        dies: &dies,
        fch: &fch,
        msr: &msr,
        clock,
    };
    let memory = Memory {
        top_mem,
        top_mem2,
        apob: apob.as_ref(),
        tseg,
        e820: &e820,
    };
    let tp1 = Tp1 {
        ctx,
        config: B::CONFIG,
        identity,
        memory: &memory,
        microcode,
    };
    let mut silicon: Silicon<S> = match S::silicon_init::<B>(tp1) {
        Ok(silicon) => silicon,
        Err(why) => stop::<S>(why),
    };

    let (roots, ecam) = enumerate::<S>(&ctx, &silicon);
    S::after_resources::<B>(&mut silicon, &ctx);
    B::mainboard_final(&mut ctx.board());
    ioapics(&ctx, &silicon);
    let hosts: Vec<acpi::HostBridge, { super::MAX_DIES }> = roots
        .iter()
        .map(|root| acpi::HostBridge::new(root, &ecam))
        .collect();
    let rsdp = acpi::install(
        silicon.acpi,
        &dies,
        &hosts,
        &silicon.ioapics,
        acpi::Ecam {
            base: S::ECAM,
            buses: S::ECAM_BUSES,
        },
        &silicon.cpus.apic_ids,
        &B::acpi(),
    );
    S::ready_to_boot::<B>(&mut silicon, &ctx, &ecam);
    let mut delay = clock;
    if let Err(what) = psp::lock_df_registers(silicon.psp.as_mut(), &mut delay) {
        fail::<S>(what);
    }
    post(&fch, 0x43);

    let Some(ffs) = S::LAYOUT.ffs else {
        fail::<S>("boot media")
    };
    if boot::map_ram(&e820).is_err() {
        fail::<S>("page tables");
    }
    if !fstart_arch::x86::mp::park_aps_for_payload() {
        fail::<S>("parking the APs");
    }
    // SAFETY: the firmware is done with the CPUs and SMM is locked.
    unsafe { psp::boot_done(silicon.psp.as_mut(), &mut delay) };
    // SAFETY: CONSOLE was initialized at entry and lives forever.
    let console = unsafe { (*core::ptr::addr_of!(CONSOLE)).assume_init_ref() };
    fstart_log::info!("{}: starting the payload", S::NAME);
    post(&fch, 0x44);
    let handoff = boot::Handoff {
        console,
        e820: core::mem::replace(
            &mut e820,
            fstart_core::services::memory_detect::E820State::new(),
        ),
        rsdp,
        ffs,
        pci_root: fstart_pci::PciRootInfo {
            segment: 0,
            ecam_base: S::ECAM,
            bus_start: 0,
            bus_end: (S::ECAM_BUSES - 1) as u8,
        },
    };
    use fstart_stage::payload::MainstagePayload;
    fstart_stage::payload::BuildSelectedPayload::boot(handoff)
}

/// PCI enumeration and resource allocation below the root complex of every
/// die, each from what the DF routes to it.
fn enumerate<S: Soc>(
    ctx: &Ctx<'_, S>,
    silicon: &Silicon<S>,
) -> (Vec<pci::Root, { super::MAX_DIES }>, PciEcam) {
    let ecam_region = Region::new(S::ECAM, u64::from(S::ECAM_BUSES) << 20);
    let roots: Option<Vec<pci::Root, { super::MAX_DIES }>> = ctx
        .dies
        .iter()
        .map(|die| {
            let reserved = silicon.reserved_mmio[usize::from(die.index())];
            pci::Root::new(&map::root_complex(die)?, ecam_region, reserved)
        })
        .collect();
    let Some(roots) = roots else {
        fail::<S>("PCI root")
    };
    let mut ecam = match PciEcam::from_providers(&roots) {
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
    (roots, ecam)
}

/// The FCH's IOAPIC (GSI 0-23, ID 0) in virtual-wire mode, then one per
/// die's NBIO (ID 1 + die), all masked.
fn ioapics<S: Soc>(ctx: &Ctx<'_, S>, silicon: &Silicon<S>) {
    ctx.fch.enable_hpet_ioapic();
    // SAFETY: decoded IOAPIC MMIO, BSP only.
    let fch_ioapic = unsafe { IoApic::new(fch::IOAPIC as usize) };
    fch_ioapic.set_id(0);
    fch_ioapic.mask_all();
    fch_ioapic.route_i8259(silicon.cpus.apic_ids.first().copied().unwrap_or(0) as u8);
    for die in ctx.dies {
        // SAFETY: as above, mapped by the silicon init.
        let ioapic = unsafe { IoApic::new(silicon.ioapics[usize::from(die.index())] as usize) };
        ioapic.set_id(1 + die.index());
        ioapic.mask_all();
    }
}
