// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin DRAM stage: the PSP released the BSP at the reset vector of
//! the image it copied to DRAM. `psp_reset.S` enters long mode and continues
//! at the architectural `_start_ram` (stack, BSS, data, IDT), which calls the
//! board's `fstart_main`.
use super::Board;
use fstart_driver_amd::{
    fam17h::{
        apob::{Apob, MemoryMap},
        msr::{self, Msr, NativeMsr},
        tsc,
    },
    lpc::Lpc,
    zeppelin::{self as soc, Package, Revision, topology},
};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_log::Hex;
use fstart_pci::ecam::GlobalEcam;

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

static mut CONSOLE: core::mem::MaybeUninit<Ns16550> = core::mem::MaybeUninit::uninit();

fn run<B: Board>() -> ! {
    post(0x30);
    // SAFETY: BSP only, single-threaded cold boot.
    let mut msr = unsafe { NativeMsr::new() };
    // MMCONF first, everything else uses it.
    msr::enable_ecam(&mut msr, soc::ECAM, soc::ECAM_BUSES);
    fstart_pci::ecam::init(soc::ECAM as usize);
    let pci = GlobalEcam;
    {
        // SAFETY: exclusive LPC decode programming before the console.
        let mut lpc = unsafe { Lpc::new(pci) };
        lpc.enable_port80();
        B::early_init(&mut lpc);
    }
    post(0x31);
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
    post(0x32);
    let mhz = unsafe { tsc::frequency_mhz() }.unwrap_or(0);
    let (_, ext_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
    let ext_eax = fstart_arch::x86::cpuid(0x8000_0001).0;
    let package = Package::from_cpuid(ext_ebx);
    let revision = Revision::from_cpuid(ext_eax);
    fstart_log::info!(
        "fstart: AMD Zeppelin {}, package {}, TSC {} MHz",
        revision.map_or("unknown", Revision::name),
        package.name(),
        mhz
    );
    let dies = topology::die_count(&pci);
    fstart_log::info!("zeppelin: {} die(s)", dies);
    if revision.is_none() || package.dies() != Some(dies) {
        fstart_log::error!("zeppelin: unsupported CPU or topology");
        halt()
    }
    match msr::memory_limits(&mut msr) {
        Ok((low, high)) => fstart_log::info!(
            "zeppelin: TOP_MEM {}, TOM2 {}",
            Hex(low),
            Hex(high.unwrap_or(0))
        ),
        Err(_) => {
            fstart_log::error!("zeppelin: TOP_MEM wasn't set up by the PSP");
            halt()
        }
    }
    let map = match apob() {
        Ok(map) => map,
        Err(error) => {
            fstart_log::error!("zeppelin: invalid APOB (error {})", error as u8);
            halt()
        }
    };
    fstart_log::info!("zeppelin: APOB top of memory {}", Hex(map.top));
    for hole in map.holes() {
        fstart_log::info!(
            "zeppelin: APOB hole type {} {}..{}",
            hole.kind,
            Hex(hole.base),
            Hex(hole.end())
        );
    }
    let _ = msr.read(msr::SYSCFG);
    fstart_log::info!("zeppelin: console checkpoint reached, halting");
    post(0x3f);
    halt()
}

fn apob() -> Result<MemoryMap, fstart_driver_amd::fam17h::apob::Error> {
    // SAFETY: the PSP wrote the APOB here before releasing the BSP and the
    // memory map keeps it reserved.
    let bytes =
        unsafe { core::slice::from_raw_parts(soc::APOB_BASE as *const u8, soc::APOB_SIZE) };
    Apob::parse_with_capacity(bytes, soc::APOB_SIZE)?.memory_map()
}
