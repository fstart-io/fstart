// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin silicon init a board's `main` calls.
use super::{Config, acpi, silicon};
use crate::noncar::{self, Ctx, Silicon, Stop, layout::Region, mp, psp, smm};
use fstart_driver_amd::{
    ccx, dxio,
    fch::{smi::FchSmi, taishan},
    soc::zeppelin::{self as soc, MAX_DIES, Zeppelin},
};
use silicon::{Dma, SmuPages, SmuPolicy};

/// The silicon init on Zeppelin: the silicon, MP init, SMM with the PSP's
/// SMM info, the PCI interrupt router. The vendor board PEIMs run inside the
/// FCH and DXIO sequence: `fch_reset_done` after the FCH reset init,
/// `dxio_training_done` after link training.
pub fn silicon_init(
    stage: &noncar::Stage<Zeppelin>,
    fch_reset_done: impl FnOnce(&mut Config),
    dxio_training_done: impl FnOnce(),
) -> Result<Silicon<Zeppelin>, Stop> {
    /// DRAM the SMU and the DXIO firmware use with DMA: firmware BSS in
    /// the reserved early region.
    static mut SMU_PAGES: SmuPages = SmuPages([[0; 4096]; 2 * MAX_DIES]);
    static mut SMU_POLICY: SmuPolicy = SmuPolicy([0; 0x80]);
    static mut DXIO_BUFFERS: dxio::zeppelin::Buffers = dxio::zeppelin::Buffers::new();

    let (ctx, config, identity, memory, microcode) = (
        stage.ctx(),
        stage.config(),
        stage.identity(),
        stage.memory(),
        stage.microcode(),
    );
    let Ctx {
        dies,
        fch,
        msr,
        mut clock,
    } = ctx;
    // SAFETY: the DMA memory is reserved firmware BSS below 4 GiB, handed
    // out once here.
    let dma = unsafe {
        Dma {
            smu_pages: &mut *core::ptr::addr_of_mut!(SMU_PAGES),
            smu_policy: &mut *core::ptr::addr_of_mut!(SMU_POLICY),
            dxio: &mut *core::ptr::addr_of_mut!(DXIO_BUFFERS),
        }
    };
    let mut config = *config;
    let state = silicon::pre_mp(
        dies,
        fch,
        msr,
        &mut config,
        identity,
        (memory.top_mem, memory.top_mem2),
        memory.apob,
        dma,
        &mut clock,
        fch_reset_done,
        dxio_training_done,
    )?;

    let policy = state.cpu_policy();
    let cpus = mp::start::<Zeppelin, _>(
        dies,
        microcode,
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
    silicon::route_interrupts(fch, &config)?;

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
