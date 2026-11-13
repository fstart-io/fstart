// SPDX-License-Identifier: MIT OR Apache-2.0
//! The SoCs made of dies with one root complex each (Picasso, Zeppelin) on
//! the flow ([`crate::flow`]): what the flow's services are for them
//! ([`DieSoc`]), their reset image's layout and their host plan. A SoC
//! module ([`crate::zeppelin`], [`crate::picasso`]) says what differs.
pub mod acpi;
#[cfg(all(feature = "bundle", target_os = "none"))]
pub mod boot;
#[cfg(feature = "host")]
pub mod host;
pub mod layout;
pub mod memory;
#[cfg(all(feature = "bundle", target_os = "none"))]
mod smbios;
pub mod smm;

#[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
pub use crate::flow::Peripherals;
#[cfg(all(feature = "bundle", target_os = "none"))]
pub use crate::flow::{
    Chipset, Console, Cpus, Early, Enumerated, FchUp, Published, Ready, Stage, Trained, fail,
};
pub use crate::flow::{Ctx, Devices, Memory, Silicon, Tables};
pub use fstart_driver_amd::soc::Hw;

use crate::acpi::SocAcpi;
use crate::smbios::SocSmbios;
use fstart_amdfw_format::efs::BiosDirectorySlot;
use fstart_driver_amd::{df::Die, soc::Init};
use fstart_pci::ecam::GlobalEcam;
use layout::{Layout, Region};

/// The shared AMD PSP reset entry with the selected SoC's geometry; a
/// reset image is for one SoC.
#[cfg(all(feature = "bundle", target_os = "none"))]
mod entry {
    #[cfg(all(feature = "picasso", feature = "zeppelin"))]
    compile_error!("a noncar reset image is for one SoC: enable picasso or zeppelin, not both");
    #[cfg(feature = "picasso")]
    use crate::picasso::LAYOUT;
    #[cfg(feature = "zeppelin")]
    use crate::zeppelin::layout::LAYOUT;
    core::arch::global_asm!(
        include_str!("../../../arch/asm/amd_psp_reset.S"),
        cache_base = const LAYOUT.cache_window().base,
        cache_mask = const !(LAYOUT.cache_window().size - 1) & 0xffff_ffff,
        pdpts = const crate::noncar::layout::PDPT_COUNT,
        options(att_syntax),
    );
}

/// The most dies the flow handles: two sockets of four.
pub const MAX_DIES: usize = 8;

/// TSEG, at the top of the DRAM below 4 GiB: 8 MiB.
pub const TSEG_SIZE: u32 = 8 << 20;

/// A SoC made of dies ([`Hw`]): what the flow needs to know of it beyond
/// its silicon init, and what its tables differ in from the other such
/// SoCs'. The flow's services ([`crate::flow::Soc`]) are the same for all
/// of them.
pub trait DieSoc: fstart_driver_amd::soc::DieInit {
    const LAYOUT: Layout;
    /// Where the PSP writes the APOB.
    const APOB: Region;
    /// Where the PSP finds the BIOS directory.
    const BIOS_DIRECTORY: BiosDirectorySlot;
    /// The most CPUs of the SoC's packages.
    const MAX_CPUS: u16;
    /// What the SMBIOS tables say of the package and its memory.
    const SMBIOS: SocSmbios;
    /// What the SoC's ACPI tables differ in.
    const ACPI: &'static SocAcpi;

    /// The NBIO IOAPIC of `die`.
    fn ioapic(state: &Self::State, die: u8) -> u32;
    /// The SoC's own controllers for the tables, once the silicon init is
    /// done: the enabled fixed-address devices and further `\_SB` AML (FCH
    /// controllers and the devices behind them).
    fn acpi_devices(
        _config: &Self::Config,
        _state: &Self::State,
    ) -> Result<(Devices, alloc::vec::Vec<u8>), &'static str> {
        Ok((Devices::new(), alloc::vec::Vec::new()))
    }
    /// The DRAM of each die, for SRAT (domain = die), if the dies are NUMA
    /// nodes: several and not interleaved.
    fn die_memory(_dies: &[Die<Self, GlobalEcam>]) -> Option<DieMemory> {
        None
    }
    /// The display, at ready to boot ([`crate::flow::Soc::init_display`]).
    fn init_display(
        _silicon: &mut Silicon<Self>,
        _ctx: &Ctx<'_, Self>,
        _pci: &fstart_pci::PciEcam,
    ) -> Result<(), &'static str> {
        Ok(())
    }
    /// The board's devices for CrabEFI to drive, after the buses are set up.
    #[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
    fn peripherals(_ctx: &Ctx<'_, Self>, _config: &Self::Config) -> Peripherals {
        Peripherals::new()
    }
}

impl<S: DieSoc> crate::flow::Soc for S {
    const TSEG_SIZE: u32 = TSEG_SIZE;
    type Flow = ();

    fn max_cpus(_: &S::Config) -> u16 {
        S::MAX_CPUS
    }

    /// The APOB where the PSP's directory puts it, the e820 with the
    /// firmware's DRAM and TSEG reserved, WB DRAM and WP flash, then the
    /// FFS of the layout.
    #[cfg(all(feature = "bundle", target_os = "none"))]
    fn map(
        hw: &mut Hw<S>,
        _: &mut S::Config,
        msr: &fstart_arch::x86::cpu::msr::NativeMsr,
        _: fstart_driver_amd::ccx::tsc::Tsc,
    ) -> Result<crate::flow::Mapped<S>, &'static str> {
        use fstart_driver_amd::{apob::Apob, ccx::cache};
        use fstart_log::Hex;
        let (top_mem, top_mem2) =
            fstart_driver_amd::ccx::msr::memory_limits(msr).map_err(|_| "TOP_MEM from the PSP")?;
        // SAFETY: the PSP wrote the APOB here before releasing the BSP and
        // the memory map keeps it reserved.
        let bytes = unsafe {
            core::slice::from_raw_parts(S::APOB.base as *const u8, S::APOB.size as usize)
        };
        let apob = Apob::new(bytes, S::psp(hw)).ok();
        let map = apob
            .as_ref()
            .and_then(|apob| {
                let map = fstart_driver_amd::apob::sysmap::first_system_memory_map(apob).ok()?;
                map.sorted().ok()
            })
            .ok_or("APOB")?;
        fstart_log::info!(
            "{}: TOM {}, TOM2 {}, APOB top {}",
            S::NAME,
            Hex(top_mem),
            Hex(top_mem2.unwrap_or(0)),
            Hex(map.top)
        );
        let tseg_size = u64::from(TSEG_SIZE);
        let tseg = memory::tseg_base(&map, top_mem, tseg_size);
        let e820 = memory::e820(&map, top_mem, tseg_size, &[S::LAYOUT.early, S::APOB])
            .map_err(|_| "memory map")?;

        // The early cache plan: WB DRAM, WP flash.
        let address_bits = fstart_arch::x86::physical_address_bits() as u8;
        // SAFETY: CPUID reported MTRRs; the cache plan rejects CPUs without.
        let mtrrs = unsafe { fstart_arch::x86::mtrr::variable_count() } as usize;
        cache::Plan::new(memory::low_top(&map, top_mem), address_bits, mtrrs)
            .and_then(|plan| cache::install(msr, &plan))
            .map_err(|_| "MTRR setup")?;

        let ffs = S::LAYOUT.ffs.ok_or("boot media")?;
        boot::mount(&e820, S::LAYOUT.early, ffs).map_err(|_| "boot media")?;
        Ok(crate::flow::Mapped {
            top_mem,
            top_mem2,
            apob,
            tseg,
            e820,
            ffs: ffs.base..ffs.end(),
            flow: (),
        })
    }

    /// The board's handler in TSEG with the FCH as the SMI owner, then the
    /// PSP's SMM info.
    #[cfg(all(feature = "bundle", target_os = "none"))]
    fn install_smm(
        ctx: &Ctx<'_, S>,
        state: &mut S::State,
        mp: &fstart_arch::x86::mp::MpHandle,
        (): &(),
    ) -> Result<(), &'static str> {
        let tseg = smm::install(S::NAME, mp, &S::smi(ctx, state), ctx.memory.tseg, TSEG_SIZE)?;
        S::smm_installed(ctx, state, &tseg)
    }

    fn acpi_devices(
        config: &S::Config,
        state: &S::State,
    ) -> Result<(Devices, alloc::vec::Vec<u8>), &'static str> {
        <S as DieSoc>::acpi_devices(config, state)
    }

    fn init_display(
        silicon: &mut Silicon<S>,
        ctx: &Ctx<'_, S>,
        pci: &fstart_pci::PciEcam,
    ) -> Result<(), &'static str> {
        <S as DieSoc>::init_display(silicon, ctx, pci)
    }

    /// A host bridge per die from the root bridges `pci` was built from,
    /// with what the display and the SoC's controllers contributed.
    #[cfg(all(feature = "bundle", target_os = "none"))]
    fn tables(
        ctx: &Ctx<'_, S>,
        silicon: &mut Silicon<S>,
        pci: &fstart_pci::PciEcam,
        (): &(),
        identity: &crate::acpi::SmbiosIdentity<'_>,
        board: crate::acpi::Board,
    ) -> Tables {
        let ioapics: [u32; MAX_DIES] =
            core::array::from_fn(|die| S::ioapic(&silicon.state, die as u8));
        let rsdp = acpi::install::<S>(
            &ctx.hw.dies,
            pci.roots(),
            &ioapics,
            S::pci_segment(ctx.hw),
            silicon,
            &board,
        );
        let smbios = smbios::install::<S>(identity, ctx, &silicon.cpus);
        Tables {
            rsdp,
            smbios,
            s5: fstart_core::description::AcpiSleepControl::new(
                S::ACPI.ports.control,
                crate::acpi::S5_SLEEP_TYPE,
            ),
        }
    }

    #[cfg(all(feature = "bundle", target_os = "none"))]
    fn map_payload_ram(
        e820: &fstart_core::services::memory_detect::E820State,
        pci: &fstart_pci::PciEcam,
    ) -> Result<(), fstart_core::services::ServiceError> {
        boot::map_ram(e820, pci)
    }

    #[cfg(all(feature = "bundle", target_os = "none", feature = "payload-uefi-basic"))]
    fn peripherals(ctx: &Ctx<'_, S>, config: &S::Config) -> Peripherals {
        <S as DieSoc>::peripherals(ctx, config)
    }
}

/// A board's facts: its SoC and the SoC's configuration. Host planning and
/// the board's `main` read the same values; the flow is the board's `main`.
pub trait Board: 'static {
    type Soc: DieSoc;
    const CONFIG: &'static <Self::Soc as Init>::Config;
}

/// The board's first phase ([`crate::flow::enter`]) with its configuration
/// of the SoC.
#[cfg(all(feature = "bundle", target_os = "none"))]
pub fn start<B: Board>() -> Early<B::Soc>
where
    <B::Soc as Init>::Config: Clone,
{
    crate::flow::enter(B::CONFIG.clone())
}

/// The DRAM ranges of the dies, the domain being the die.
pub type DieMemory = heapless::Vec<fstart_acpi::numa::MemoryAffinity, 48>;
