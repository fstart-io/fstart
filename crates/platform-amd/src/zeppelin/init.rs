// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin silicon init phases a board's `main` calls. They are split
//! where the vendor board PEIMs run:
//!
//! ```ignore
//! let init = zeppelin::Init::start(&stage)?;
//! // board: the FCH is up — SMBus/I2C devices (VRs, redrivers)
//! let dxio = init.train_dxio()?;
//! // board: links are trained — PHY presets, wait for the BMC
//! let silicon = dxio.finish()?;
//! ```
use super::{acpi, silicon};
use crate::noncar::{self, Ctx, Silicon, Stage, Stop, layout::Region, mp, psp, smm};
use fstart_driver_amd::{
    ccx, dxio,
    fch::{NativeBus, gpio::Gpio, smi::FchSmi, taishan},
    soc::zeppelin::{self as soc, MAX_DIES, Zeppelin},
};
use silicon::{SmuPages, SmuPolicy};

/// The silicon init after the DF and the FCH reset init.
pub struct Init<'a> {
    stage: &'a Stage<Zeppelin>,
    nonpci: [u32; MAX_DIES],
}

/// The silicon init after DXIO link training.
pub struct Dxio<'a> {
    stage: &'a Stage<Zeppelin>,
    trained: silicon::Trained,
}

impl<'a> Init<'a> {
    /// The DF MMIO/IO map and init, then the FCH reset init.
    pub fn start(stage: &'a Stage<Zeppelin>) -> Result<Self, Stop> {
        let Ctx { dies, fch, .. } = stage.ctx();
        let memory = stage.memory();
        let mut clock = stage.ctx().clock;
        let nonpci = silicon::pre_mp_reset(
            dies,
            fch,
            stage.identity(),
            (memory.top_mem, memory.top_mem2),
            memory.apob,
            &mut clock,
        )?;
        Ok(Self { stage, nonpci })
    }

    /// What the board works on: the dies, die 0's FCH, `gpio()`, the clock.
    pub fn ctx(&self) -> Ctx<'a, Zeppelin> {
        self.stage.ctx()
    }

    /// The GPIO pads of die 0.
    pub fn gpio(&self) -> Gpio<NativeBus> {
        self.stage.ctx().gpio()
    }

    /// NBIO and the BSP, the DRAM map, the IOAPIC and IOMMU BARs, then DXIO
    /// link training.
    pub fn train_dxio(self) -> Result<Dxio<'a>, Stop> {
        static mut DXIO_BUFFERS: dxio::zeppelin::Buffers = dxio::zeppelin::Buffers::new();
        let Ctx { dies, fch, msr, .. } = self.stage.ctx();
        let memory = self.stage.memory();
        let mut clock = self.stage.ctx().clock;
        // SAFETY: firmware BSS in the reserved early region below 4 GiB,
        // handed out once here.
        let buffers = unsafe { &mut *core::ptr::addr_of_mut!(DXIO_BUFFERS) };
        let trained = silicon::pre_mp_dxio(
            dies,
            fch,
            msr,
            self.stage.config(),
            self.stage.identity(),
            (memory.top_mem, memory.top_mem2),
            self.nonpci,
            buffers,
            &mut clock,
        )?;
        Ok(Dxio {
            stage: self.stage,
            trained,
        })
    }
}

impl<'a> Dxio<'a> {
    /// What the board works on: the dies, die 0's FCH, `gpio()`, the clock.
    pub fn ctx(&self) -> Ctx<'a, Zeppelin> {
        self.stage.ctx()
    }

    /// USB, the SMU, the late DF, the NBIO and FCH environment, MP init, SMM
    /// with the PSP's SMM info, then the PCI interrupt router.
    pub fn finish(self) -> Result<Silicon<Zeppelin>, Stop> {
        /// DRAM the SMU uses with DMA: firmware BSS in the reserved early
        /// region.
        static mut SMU_PAGES: SmuPages = SmuPages([[0; 4096]; 2 * MAX_DIES]);
        static mut SMU_POLICY: SmuPolicy = SmuPolicy([0; 0x80]);
        let stage = self.stage;
        let Ctx {
            dies,
            fch,
            mut clock,
            ..
        } = stage.ctx();
        let config = stage.config();
        let memory = stage.memory();
        // SAFETY: reserved firmware BSS below 4 GiB, handed out once here.
        let (smu_pages, smu_policy) = unsafe {
            (
                &mut *core::ptr::addr_of_mut!(SMU_PAGES),
                &mut *core::ptr::addr_of_mut!(SMU_POLICY),
            )
        };
        let state = silicon::pre_mp_finish(
            dies,
            fch,
            config,
            stage.identity(),
            memory.apob,
            self.trained,
            smu_pages,
            smu_policy,
            &mut clock,
        );

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
            aml: Default::default(),
            tables: Default::default(),
            framebuffer: None,
            state,
        })
    }
}
