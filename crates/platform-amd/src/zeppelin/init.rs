// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin silicon phases a board's `main` calls, on the shared
//! [`Stage`]. They are split where the vendor board PEIMs run:
//!
//! ```ignore
//! let stage = stage.init_fch();
//! // board: the FCH is up — SMBus/I2C devices (VRs, redrivers)
//! let stage = stage.train_dxio(&dxio);
//! // board: links are trained — PHY presets, wait for the BMC
//! let stage = stage.init_chipset().start_cpus();
//! ```
use super::{Config, DxioConfig, acpi, silicon};
use crate::noncar::{self, Cpus, Mapped, Silicon, Stage, Stop, layout::Region, mp, psp, smm};
use fstart_driver_amd::{
    ccx, dxio,
    fch::{smi::FchSmi, taishan},
    soc::zeppelin::{self as soc, MAX_DIES, Zeppelin},
};
use silicon::{SmuPages, SmuPolicy};

/// After the DF and the FCH reset init.
pub struct FchUp {
    nonpci: [u32; MAX_DIES],
}

/// After DXIO link training.
pub struct Trained(silicon::Trained);

/// After USB, the SMU, the late DF and the NBIO and FCH environment.
pub struct Chipset(silicon::State);

impl Stage<Zeppelin, Mapped> {
    /// The DF MMIO/IO map and init, then the FCH reset init.
    pub fn init_fch(self) -> Stage<Zeppelin, FchUp> {
        let ctx = self.ctx();
        let memory = self.memory();
        let clock = ctx.clock;
        let nonpci = silicon::pre_mp_reset(
            ctx.dies,
            ctx.fch,
            self.identity(),
            (memory.top_mem, memory.top_mem2),
            memory.apob,
            &clock,
        )
        .unwrap_or_else(|why| self.stop(why.into()));
        self.into_phase(FchUp { nonpci })
    }
}

impl Stage<Zeppelin, FchUp> {
    /// NBIO and the BSP, the DRAM map, the IOAPIC and IOMMU BARs, then DXIO
    /// link training of the board's engines `dxio`.
    pub fn train_dxio(self, dxio: &DxioConfig) -> Stage<Zeppelin, Trained> {
        static mut DXIO_BUFFERS: dxio::zeppelin::Buffers = dxio::zeppelin::Buffers::new();
        let ctx = self.ctx();
        let memory = self.memory();
        let clock = ctx.clock;
        // SAFETY: firmware BSS in the reserved early region below 4 GiB,
        // handed out once here.
        let buffers = unsafe { &mut *core::ptr::addr_of_mut!(DXIO_BUFFERS) };
        let trained = silicon::pre_mp_dxio(
            ctx.dies,
            ctx.fch,
            ctx.msr,
            &Config {
                dxio: *dxio,
                ..*self.config()
            },
            self.identity(),
            (memory.top_mem, memory.top_mem2),
            self.phase.nonpci,
            buffers,
            &clock,
        )
        .unwrap_or_else(|why| self.stop(why.into()));
        self.into_phase(Trained(trained))
    }
}

impl Stage<Zeppelin, Trained> {
    /// USB, the SMU, the late DF, the NBIO and FCH environment.
    pub fn init_chipset(self) -> Stage<Zeppelin, Chipset> {
        /// DRAM the SMU uses with DMA: firmware BSS in the reserved early
        /// region.
        static mut SMU_PAGES: SmuPages = SmuPages([[0; 4096]; 2 * MAX_DIES]);
        static mut SMU_POLICY: SmuPolicy = SmuPolicy([0; 0x80]);
        let Stage {
            machine,
            phase: Trained(trained),
        } = self;
        let stage = Stage { machine, phase: () };
        let ctx = stage.ctx();
        let memory = stage.memory();
        let clock = ctx.clock;
        // SAFETY: reserved firmware BSS below 4 GiB, handed out once here.
        let (smu_pages, smu_policy) = unsafe {
            (
                &mut *core::ptr::addr_of_mut!(SMU_PAGES),
                &mut *core::ptr::addr_of_mut!(SMU_POLICY),
            )
        };
        let state = silicon::pre_mp_finish(
            ctx.dies,
            ctx.fch,
            stage.config(),
            stage.identity(),
            memory.apob,
            trained,
            smu_pages,
            smu_policy,
            &clock,
        );
        stage.into_phase(Chipset(state))
    }
}

impl Stage<Zeppelin, Chipset> {
    /// MP init, SMM with the PSP's SMM info, then the PCI interrupt router.
    pub fn start_cpus(self) -> Stage<Zeppelin, Cpus<Zeppelin>> {
        let Stage {
            machine,
            phase: Chipset(state),
        } = self;
        let stage = Stage { machine, phase: () };
        let silicon = try_start_cpus(&stage, state).unwrap_or_else(|why| stage.stop(why));
        stage.into_phase(Cpus(silicon))
    }
}

fn try_start_cpus(
    stage: &Stage<Zeppelin, ()>,
    state: silicon::State,
) -> Result<Silicon<Zeppelin>, Stop> {
    let ctx = stage.ctx();
    let mut clock = ctx.clock;
    let (dies, fch) = (ctx.dies, ctx.fch);
    let config = stage.config();
    let memory = stage.memory();
    let policy = state.cpu_policy();
    let cpus = mp::start::<Zeppelin, _>(
        dies,
        stage.microcode(),
        move |msr, thread| ccx::zeppelin::cpu_init(msr, &policy, thread),
        clock,
    )?;
    silicon::cpus_up(dies, &mut clock);
    // SAFETY: BSP only after MP init; ACPI MMIO and the ACPI blocks are
    // decoded since the FCH init.
    let smi = unsafe { FchSmi::new(taishan::ACPI_PORTS, config.fch.smi_gevents) };
    let tseg = smm::install(
        <Zeppelin as noncar::Soc>::NAME,
        &cpus.mp,
        &smi,
        memory.tseg,
        <Zeppelin as noncar::Soc>::TSEG_SIZE,
    )?;
    // The PSP gets the SMM info now, LockDFReg at ready-to-boot and
    // BootDone right before the payload.
    let psp = psp::notify_smm(
        state.psp_bars[0],
        <Zeppelin as noncar::Soc>::PSP,
        &tseg,
        &clock,
    );
    silicon::post(fch, 0x42);
    // PCI: the interrupt router first.
    silicon::route_interrupts(fch, config)?;

    Ok(Silicon {
        cpus,
        psp,
        ioapics: core::array::from_fn(|die| state.ioapics.get(die).copied().unwrap_or(0)),
        // The non-PCI MMIO pool.
        reserved_mmio: core::array::from_fn(|die| {
            state.nonpci.get(die).map_or(Region::new(0, 0), |&base| {
                Region::new(base.into(), soc::NONPCI_MMIO_SIZE.into())
            })
        }),
        acpi: &acpi::ACPI,
        devices: heapless::Vec::new(),
        // The FCH GPIO controller (AMDI0030) on the IRQ the board routes
        // it to, as the vendor DSDT has it: three banks.
        aml: config
            .fch
            .irq_routes
            .iter()
            .find(|route| route.source == taishan::irq::GPIO)
            .and_then(|route| fstart_driver_amd::fch::aml::gpio(route.apic, 0x300).ok())
            .unwrap_or_default(),
        tables: Default::default(),
        framebuffer: None,
        state,
    })
}
