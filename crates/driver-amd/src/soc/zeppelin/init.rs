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
use crate::{
    ccx::{self, Thread, tsc::Tsc},
    cpu::{Package, Revision},
    dxio,
    fch::{Fch, NativeBus, post, smi::FchSmi, taishan},
    psp,
    soc::{Ctx, Init},
};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_pci::PciEcam;

/// After the DF and the FCH reset init.
pub struct FchUp {
    nonpci: [u32; MAX_DIES],
}

impl Init for Zeppelin {
    const NAME: &'static str = "zeppelin";
    const ECAM: u64 = super::ECAM;
    const ECAM_BUSES: u16 = super::ECAM_BUSES;
    const PSP: psp::Variant = psp::Variant::Zp;

    type Config = Config;
    type Identity = Identity;
    /// The DXIO engines, in place of the configuration's: a board may
    /// only know them once its I2C devices answer.
    type Lanes<'a> = &'a DxioConfig;
    type Runtime = ();
    type FchUp = FchUp;
    type Trained = Trained;
    type Chipset = State;
    type CpuPolicy = ccx::zeppelin::CpuPolicy;
    type State = State;

    fn fch_pre_init(fch: &Fch<NativeBus>, _: &Config, _: Option<Tsc>) -> Result<(), &'static str> {
        silicon::fch_pre_init(fch).map_err(|_| "FCH pre-init")
    }

    fn identify(
        fch: &Fch<NativeBus>,
        config: &Config,
        mhz: u32,
        dies: u8,
    ) -> Result<Identity, &'static str> {
        silicon::fch_early_init(fch, config);
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
            .filter(|_| package.dies() == Some(dies) && mhz != 0)
            .ok_or("CPU identification")?;
        Ok(Identity { revision, package })
    }

    /// The DF MMIO/IO map and init, then the FCH reset init.
    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<FchUp, &'static str> {
        let nonpci = silicon::pre_mp_reset(
            ctx.dies,
            ctx.fch,
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
        static mut DXIO_BUFFERS: dxio::zeppelin::Buffers = dxio::zeppelin::Buffers::new();
        // SAFETY: firmware BSS in the reserved early region below 4 GiB,
        // handed out once here.
        let buffers = unsafe { &mut *core::ptr::addr_of_mut!(DXIO_BUFFERS) };
        silicon::pre_mp_dxio(
            ctx.dies,
            ctx.fch,
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
            ctx.dies,
            ctx.fch,
            ctx.config,
            ctx.identity,
            ctx.memory.apob,
            trained,
            smu_pages,
            smu_policy,
            &ctx.clock,
        ))
    }

    fn cpu_policy(_: &Ctx<'_, Self>, state: &State) -> Result<Self::CpuPolicy, &'static str> {
        Ok(state.cpu_policy())
    }

    fn cpu_init(policy: &Self::CpuPolicy, msr: &NativeMsr, thread: Thread) -> bool {
        ccx::zeppelin::cpu_init(msr, policy, thread).is_ok()
    }

    fn cpus_up(ctx: &Ctx<'_, Self>, state: State) -> Result<State, &'static str> {
        silicon::cpus_up(ctx.dies, &mut { ctx.clock });
        Ok(state)
    }

    fn smi(ctx: &Ctx<'_, Self>) -> FchSmi {
        // SAFETY: BSP only after MP init; ACPI MMIO and the ACPI blocks are
        // decoded since the FCH init.
        unsafe { FchSmi::new(taishan::ACPI_PORTS, ctx.config.fch.smi_gevents) }
    }

    /// PCI: the interrupt router first.
    fn before_enumeration(ctx: &Ctx<'_, Self>, _: &mut State) -> Result<(), &'static str> {
        post(ctx.fch, 0x42);
        silicon::route_interrupts(ctx.fch, ctx.config)
    }

    fn after_resources(ctx: &Ctx<'_, Self>, _: &mut State) {
        silicon::after_resources(ctx.fch);
    }

    /// The vendor firmware's ReadyToBoot, up to the board's own work.
    fn ready_to_boot(
        ctx: &Ctx<'_, Self>,
        _: &mut State,
        pci: &PciEcam,
    ) -> Result<(), &'static str> {
        let sata = pci
            .devices()
            .find(|a| pci.device(*a).is_some_and(|d| d.read32(0) == 0x7901_1022));
        silicon::ready_to_boot(ctx.dies, ctx.fch, sata);
        Ok(())
    }

    /// The end of the vendor ReadyToBoot, after the board's work.
    fn end_of_post(ctx: &Ctx<'_, Self>, _: &mut State) {
        silicon::end_of_post(ctx.dies);
    }

    fn psp_bar(state: &State) -> u32 {
        state.psp_bars[0]
    }

    fn ioapic(state: &State, die: u8) -> u32 {
        state.ioapics.get(usize::from(die)).copied().unwrap_or(0)
    }

    /// The non-PCI MMIO pool.
    fn nonpci_mmio(state: &State, die: u8) -> (u64, u64) {
        state
            .nonpci
            .get(usize::from(die))
            .map_or((0, 0), |&base| (base.into(), NONPCI_MMIO_SIZE.into()))
    }
}
