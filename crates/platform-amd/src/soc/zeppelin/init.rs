// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin silicon init as the steps of [`Init`], on the native
//! register transports. The steps end where the vendor board PEIMs run:
//! after [`Init::init_fch`] the FCH is up for a board's SMBus/I2C devices
//! (VRs, redrivers), after [`Init::train_dxio`] the links are trained (PHY
//! presets, the BMC).
use super::{
    Config, DxioConfig, MAX_DIES, NONPCI_MMIO_SIZE, Zeppelin,
    silicon::{self, Identity, SmuPages, SmuPolicy, State, Trained},
};
use crate::soc::{Ctx, DieInit, Entered, Hw, Init, Roots};
use fstart_arch::x86::{
    cpu::{amd::smm::InstalledTseg, msr::NativeMsr},
    mp::MpHandle,
};
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::{
    ccx::{self, mp::Fam17hCpu, tsc::Tsc},
    cpu::{Package, Revision},
    dxio,
    fch::{post, smi::FchSmi, taishan},
    psp::{self, boot::Psp},
};
use fstart_pci::PciEcam;

/// After the DF and the FCH reset init.
pub struct FchUp {
    nonpci: [u32; MAX_DIES],
}

/// The silicon after MP, with die 0's PSP mailbox once it took the SMM
/// info.
pub struct Up {
    pub silicon: State,
    bsp_apic: u8,
    psp: Option<Psp>,
}

impl Zeppelin {
    /// The NBIO IOAPIC of `die`.
    pub fn ioapic(state: &State, die: u8) -> u32 {
        state.ioapics.get(usize::from(die)).copied().unwrap_or(0)
    }

    /// The non-PCI MMIO pool of `die`, base and size.
    fn nonpci_mmio(state: &State, die: u8) -> (u64, u64) {
        state
            .nonpci
            .get(usize::from(die))
            .map_or((0, 0), |&base| (base.into(), NONPCI_MMIO_SIZE.into()))
    }
}

impl Init for Zeppelin {
    const NAME: &'static str = "zeppelin";

    type Config = Config;
    type Identity = Identity;
    type Hw = Hw<Self>;
    /// The DXIO engines, in place of the configuration's: a board may
    /// only know them once its I2C devices answer.
    type Lanes<'a> = &'a DxioConfig;
    type Runtime = ();
    type FchUp = FchUp;
    type Trained = Trained;
    type Chipset = State;
    type Cpu = Fam17hCpu<Self, ccx::zeppelin::CpuPolicy>;
    type State = Up;

    fn enter(_: &Config, msr: &NativeMsr) -> Entered<Self> {
        Hw::enter(msr, super::ECAM, super::ECAM_BUSES, |fch, _| {
            silicon::fch_pre_init(fch).map_err(|_| "FCH pre-init")
        })
    }

    fn identify(
        hw: &Hw<Self>,
        config: &Config,
        clock: Option<Tsc>,
    ) -> Result<Identity, &'static str> {
        silicon::fch_early_init(&hw.fch, config);
        let mhz = clock.map_or(0, |clock| clock.mhz());
        let (ext_eax, ext_ebx, _, _) = fstart_arch::x86::cpuid(0x8000_0001);
        let package = Package::from_cpuid(ext_ebx);
        let revision = Revision::from_signature(ext_eax)
            .ok()
            .filter(|revision| revision.zp());
        fstart_log::info!(
            "fstart: AMD Zeppelin {}, package {}, TSC {} MHz",
            revision.map_or("unknown", Revision::name),
            package.name(),
            mhz
        );
        let revision = revision
            .filter(|_| package.dies() == Some(hw.dies.len() as u8) && mhz != 0)
            .ok_or("CPU identification")?;
        Ok(Identity { revision, package })
    }

    fn pci_segment(_: &Hw<Self>) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: super::ECAM,
            bus_start: 0,
            bus_end: (super::ECAM_BUSES - 1) as u8,
        }
    }

    fn psp(_: &Hw<Self>) -> psp::Variant {
        psp::Variant::Zp
    }

    /// The DF MMIO/IO map and init, then the FCH reset init.
    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<FchUp, &'static str> {
        let nonpci = silicon::pre_mp_reset(
            &ctx.hw.dies,
            &ctx.hw.fch,
            ctx.identity,
            (ctx.memory.top_mem, ctx.memory.top_mem2),
            ctx.memory.apob,
            &ctx.clock,
        )?;
        Ok(FchUp { nonpci })
    }

    /// NBIO and the BSP, the DRAM map, the IOAPIC and IOMMU BARs, then DXIO
    /// link training of the board's engines `lanes`.
    fn train_dxio(
        ctx: &Ctx<'_, Self>,
        FchUp { nonpci }: FchUp,
        lanes: &DxioConfig,
    ) -> Result<Trained, &'static str> {
        static mut DXIO_BUFFERS: dxio::zeppelin::Buffers<MAX_DIES> = dxio::zeppelin::Buffers::new();
        // SAFETY: firmware BSS in the reserved early region below 4 GiB,
        // handed out once here.
        let buffers = unsafe { &mut *core::ptr::addr_of_mut!(DXIO_BUFFERS) };
        silicon::pre_mp_dxio(
            &ctx.hw.dies,
            &ctx.hw.fch,
            ctx.msr,
            &Config {
                dxio: *lanes,
                ..*ctx.config
            },
            ctx.identity,
            (ctx.memory.top_mem, ctx.memory.top_mem2),
            nonpci,
            buffers,
            &ctx.clock,
        )
    }

    /// USB, the SMU, the late DF, the NBIO and FCH environment.
    fn init_chipset(ctx: &Ctx<'_, Self>, trained: Trained, (): ()) -> Result<State, &'static str> {
        /// DRAM the SMU uses with DMA: firmware BSS in the reserved early
        /// region.
        static mut SMU_PAGES: SmuPages = SmuPages([[0; 4096]; 2 * MAX_DIES]);
        static mut SMU_POLICY: SmuPolicy = SmuPolicy([0; 0x80]);
        // SAFETY: reserved firmware BSS below 4 GiB, handed out once here.
        let (smu_pages, smu_policy) = unsafe {
            (
                &mut *core::ptr::addr_of_mut!(SMU_PAGES),
                &mut *core::ptr::addr_of_mut!(SMU_POLICY),
            )
        };
        Ok(silicon::pre_mp_finish(
            &ctx.hw.dies,
            &ctx.hw.fch,
            ctx.config,
            ctx.identity,
            ctx.memory.apob,
            trained,
            smu_pages,
            smu_policy,
            &ctx.clock,
        ))
    }

    fn cpu_driver(
        _: &Ctx<'_, Self>,
        state: &mut State,
        microcode: Option<&'static [u8]>,
    ) -> Result<Self::Cpu, &'static str> {
        Ok(Fam17hCpu::new(
            "AMD Zeppelin",
            microcode,
            state.cpu_policy(),
            |policy, msr, thread| ccx::zeppelin::cpu_init(msr, policy, thread).is_ok(),
        ))
    }

    fn threads(ctx: &Ctx<'_, Self>, _: &State) -> Result<u16, &'static str> {
        ctx.hw.threads()
    }

    fn release_aps(ctx: &Ctx<'_, Self>, _: &State, aps: u16) -> Result<(), u32> {
        ctx.hw.release_aps(aps, ctx.clock)
    }

    fn cpu_errors(cpu: &Self::Cpu) -> u32 {
        cpu.errors()
    }

    fn cpus_up(
        ctx: &Ctx<'_, Self>,
        silicon: State,
        _: &MpHandle,
        apic_ids: &[u32],
    ) -> Result<Up, &'static str> {
        silicon::cpus_up(&ctx.hw.dies, &mut { ctx.clock });
        Ok(Up {
            silicon,
            bsp_apic: apic_ids.first().copied().unwrap_or(0) as u8,
            psp: None,
        })
    }

    /// PCI: the interrupt router first.
    fn before_enumeration(ctx: &Ctx<'_, Self>, _: &mut Up) -> Result<(), &'static str> {
        post(&ctx.hw.fch, 0x42);
        silicon::route_interrupts(&ctx.hw.fch, ctx.config)
    }

    fn roots(ctx: &Ctx<'_, Self>, up: &Up) -> Option<Roots> {
        ctx.hw.roots(&Self::pci_segment(ctx.hw), |die| {
            Self::nonpci_mmio(&up.silicon, die)
        })
    }

    fn after_resources(ctx: &Ctx<'_, Self>, _: &mut Up) -> Result<(), &'static str> {
        silicon::after_resources(&ctx.hw.fch);
        Ok(())
    }

    /// The IOAPICs, then the vendor firmware's ReadyToBoot, up to the
    /// board's own work.
    fn ready_to_boot(ctx: &Ctx<'_, Self>, up: &mut Up, pci: &PciEcam) -> Result<(), &'static str> {
        ctx.hw
            .init_ioapics(up.bsp_apic, |die| Self::ioapic(&up.silicon, die));
        let sata = pci
            .devices()
            .find(|a| pci.device(*a).is_some_and(|d| d.read32(0) == 0x7901_1022));
        silicon::ready_to_boot(&ctx.hw.dies, &ctx.hw.fch, sata);
        Ok(())
    }

    /// The end of the vendor ReadyToBoot, after the board's work.
    fn end_of_post(ctx: &Ctx<'_, Self>, _: &mut Up) {
        silicon::end_of_post(&ctx.hw.dies);
    }

    fn finalize(ctx: &Ctx<'_, Self>, up: &mut Up) -> Result<(), &'static str> {
        psp::boot::lock_df_registers(up.psp.as_mut(), &ctx.clock)
    }

    /// A failed BootDone is logged: the session refuses a retry, and the
    /// transition may have happened.
    fn boot_done(ctx: &Ctx<'_, Self>, up: &mut Up) -> Result<(), &'static str> {
        // SAFETY: the flow is done with the CPUs and SMM is locked.
        unsafe { psp::boot::boot_done(up.psp.as_mut(), &ctx.clock) };
        Ok(())
    }
}

impl DieInit for Zeppelin {
    fn smi(ctx: &Ctx<'_, Self>, _: &Up) -> FchSmi {
        // SAFETY: BSP only after MP init; ACPI MMIO and the ACPI blocks are
        // decoded since the FCH init.
        unsafe { FchSmi::new(taishan::ACPI_PORTS, ctx.config.fch.smi_gevents) }
    }

    /// The PSP gets the SMM info now, LockDFReg after the last DF write and
    /// BootDone right before the payload.
    fn smm_installed(
        ctx: &Ctx<'_, Self>,
        up: &mut Up,
        tseg: &InstalledTseg,
    ) -> Result<(), &'static str> {
        up.psp = psp::boot::notify_smm(up.silicon.psp_bars[0], psp::Variant::Zp, tseg, &ctx.clock);
        Ok(())
    }
}
