// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Zeppelin silicon init. Generic over the register
//! transports, so the host model of the dies can log the whole sequence
//! (`tests.rs`); the board hooks come in as closures.
use super::Config;
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_driver_amd::{
    apob::Apob,
    ccx::{
        self, CacWeights, Thread,
        tsc::{Clock, DelayNs},
    },
    df::{self, map},
    dxio,
    fch::{self, Bus, Fch, early, taishan},
    nbio::{self, iohc, iommu},
    psp,
    smu::v9 as smu,
    soc::zeppelin::{self as soc, Die, MAX_DIES, Package, Revision},
};
use fstart_log::Hex;
use fstart_pci::{ConfigRegionAccess, PciAddress};
use tock_registers::interfaces::Writeable;
use zerocopy::IntoBytes;

/// What the CPU identification found.
#[derive(Debug, Clone, Copy)]
pub struct Identity {
    pub revision: Revision,
    pub package: Package,
}

/// The silicon state the later phases need.
#[derive(Debug, Clone, Copy)]
pub struct State {
    pub identity: Identity,
    /// The non-PCI MMIO pool of each die.
    pub nonpci: [u32; MAX_DIES],
    /// The NBIO IOAPIC of each die.
    pub ioapics: [u32; MAX_DIES],
    pub psp_bars: [u32; MAX_DIES],
    pub cac_weights: Option<CacWeights>,
}
impl State {
    /// What every CPU's CCX init needs.
    pub fn cpu_policy(&self) -> ccx::zeppelin::CpuPolicy {
        ccx::zeppelin::CpuPolicy {
            revision: self.identity.revision,
            psp_bars: self.psp_bars,
            cac_weights: self.cac_weights,
        }
    }
}

/// DRAM the SMU uses with DMA: an AGM log and a perfmon page per die and the
/// policy table.
#[repr(C, align(4096))]
pub struct SmuPages(pub [[u8; 4096]; 2 * MAX_DIES]);
#[repr(C, align(64))]
pub struct SmuPolicy(pub [u8; 0x80]);

/// The DMA memory of the SMU and the DXIO firmware: reserved DRAM below
/// 4 GiB the firmware keeps using until reset.
pub struct Dma {
    pub smu_pages: &'static mut SmuPages,
    pub smu_policy: &'static mut SmuPolicy,
    pub dxio: &'static mut dxio::zeppelin::Buffers,
}

/// A POST code to the BMC's POST snoop.
pub fn post(fch: &Fch<impl Bus>, code: u8) {
    fch.bus().reg(fch::POST_CODE).set(code);
}

/// FCH setup before the console: ACPI MMIO, LPC/SPI, SMBus, CF9, legacy I/O.
pub fn fch_pre_init(fch: &Fch<impl Bus>) -> Result<(), early::Error> {
    taishan::pre_init(fch, true)
}

/// Right after the console: the AC power-loss policy.
pub fn fch_early_init(fch: &Fch<impl Bus>, config: &Config) {
    early::set_power_failure_state(fch, config.fch.power_failure);
}

/// The silicon init before MP (AmdSocSp4ZpPei, AmdFabricZpPei,
/// FchTaishanPei, AmdNbioBaseZPPei, AmdCcxZenZpPei), then the native
/// silicon init up to the APs.
#[allow(clippy::too_many_arguments)]
pub fn pre_mp<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    fch: &Fch<impl Bus>,
    msr: &impl MsrAccess,
    config: &Config,
    identity: Identity,
    (top_mem, top_mem2): (u64, Option<u64>),
    apob: Option<&Apob>,
    dma: Dma,
    clock: &mut impl Clock,
    fch_reset_done: impl FnOnce(),
    dxio_training_done: impl FnOnce(),
) -> Result<State, &'static str> {
    let Identity { revision, package } = identity;
    let count = dies.len() as u8;
    // DF MMIO/IO distribution, then the per-die DF init.
    let tom2 = top_mem2.unwrap_or(0);
    let plan = map::plan(top_mem, tom2, u64::from(soc::ECAM_BUSES) << 20, count)
        .map_err(|_| "DF MMIO plan")?;
    for w in &plan.mmio {
        fstart_log::debug!(
            "DF MMIO pair {}: {}..{} -> die {}",
            w.pair,
            Hex(w.base),
            Hex(w.end()),
            w.die
        );
    }
    map::program(&plan, dies);
    let mut nonpci = [0; MAX_DIES];
    for die in dies {
        nonpci[usize::from(die.index())] =
            df::zeppelin::nonpci_mmio_base(&dies[0].df(), die.index())
                .ok_or("non-PCI MMIO pool")?;
    }
    for die in dies {
        df::zeppelin::early_init(&die.df(), count, revision);
    }
    if let Some(apob) = apob {
        for die in dies {
            if let Some((rate, size)) = df::zeppelin::scrub_init(&die.df(), die.index(), apob) {
                fstart_log::debug!("DF {}: scrub rate {} for {}", die.index(), rate, Hex(size));
            }
        }
    }

    // FchTaishanPei, then the board PEIMs that wait for its SMBus PPI.
    taishan::reset_init(fch, revision);
    taishan::sata::reset(&dies[0].smn(), clock);
    taishan::multi::reset(
        fch.bus(),
        dies,
        package,
        |die| nonpci[usize::from(die)],
        clock,
    );
    fch_reset_done();

    // AmdNbioBaseZPPei, AmdCcxZenZpPei.
    for die in dies {
        nbio::zeppelin::early_init(die, revision);
        // A locked BAR gets reported as a fixed resource by the root complex.
        let psp = nonpci[usize::from(die.index())] + soc::NONPCI_PSP;
        if let Err(bar) = iohc::map_psp(&die.smn(), psp) {
            fstart_log::error!(
                "PSP BAR of die {} is locked at {}",
                die.index(),
                Hex(bar.into())
            );
        }
    }
    ccx::zeppelin::bsp_init(msr, revision, Thread::current());
    smu::print_name(&dies[0], clock);
    for die in dies {
        iohc::dram_map(die, top_mem as u32, top_mem2);
    }
    post(fch, 0x40);

    let mut ioapics = [0; MAX_DIES];
    for die in dies {
        // The NBIO IOAPIC 1 MiB below the top of the pool (0xfeb00000 on
        // die 0). The vendor firmware sets its ID to 0x81 + die; the ACPI
        // code assigns the IDs.
        let nonpci = nonpci[usize::from(die.index())];
        let ioapic = nonpci + soc::NONPCI_IOAPIC;
        iohc::IOAPIC_BAR.map(&die.smn(), ioapic);
        ioapics[usize::from(die.index())] = ioapic;
        iommu::init_bar(die, nonpci + soc::NONPCI_IOMMU);
    }
    let psp_bars = psp::bars(dies);

    // AmdNbioPcieZPPei
    // SAFETY: BSP-only; the DMA buffer is reserved below 4 GiB.
    let topology = unsafe {
        dxio::zeppelin::init(
            dies,
            &config.dxio.descriptors,
            config.subsystem_id,
            dma.dxio,
            clock,
        )
    }
    .map_err(|_| "DXIO training")?;
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
    dxio_training_done();

    // FchTaishanPei USB, then the DXE drivers.
    let devices = &config.fch.devices;
    taishan::usb::init(&dies[0].smn(), 0, package, clock);
    taishan::multi::usb(dies, devices, package, clock);
    let table = smu::PolicyTable::SP4;
    dma.smu_policy.0[..table.as_bytes().len()].copy_from_slice(table.as_bytes());
    let page = |i: usize| dma.smu_pages.0[i].as_ptr() as u64;
    let buffers = smu::DmaBuffers {
        policy: dma.smu_policy.0.as_ptr() as u64,
        agm_log: core::array::from_fn(page),
        perfmon: core::array::from_fn(|i| page(MAX_DIES + i)),
    };
    // SAFETY: the pages and the policy are reserved DRAM below 4 GiB.
    if unsafe { smu::init(dies, &buffers, clock) }.is_err() {
        fstart_log::error!("zeppelin: SMU initialization failed");
    }
    let cac_weights = smu::cac_weights(&dies[0], clock).ok();
    let sockets = df::zeppelin::sockets(&dies[0].df());
    for die in dies {
        if let Some(apob) = apob {
            df::zeppelin::late_init(&die.df(), count, sockets, revision, apob);
        }
        map::route_vga(&die.df(), dies[0].ioms_fabric_id());
    }
    for die in dies {
        nbio::zeppelin::pre_enumeration_init(die);
    }
    fch.acpi_decode(taishan::ACPI_PORTS);
    taishan::env_init(fch, &dies[0].smn(), config.subsystem_id);
    if devices[0].sata {
        taishan::sata::init(fch, 0, &dies[0].smn(), config.subsystem_id, clock);
    }
    taishan::multi::xgbe_init(&dies[0].smn());
    taishan::multi::env(
        fch.bus(),
        dies,
        devices,
        |die| nonpci[usize::from(die)],
        config.subsystem_id,
        clock,
    );
    post(fch, 0x41);
    Ok(State {
        identity,
        nonpci,
        ioapics,
        psp_bars,
        cac_weights,
    })
}

/// The SMU's MP-services and CCX-done callbacks, once the APs ran their init.
pub fn cpus_up<P: ConfigRegionAccess + Copy>(dies: &[Die<P>], delay: &mut impl DelayNs) {
    if smu::aps_ready(dies, delay).is_err() {
        fstart_log::error!("zeppelin: SMU messages after AP launch failed");
    }
    if smu::ccx_done(dies, delay).is_err() {
        fstart_log::error!("zeppelin: SMU CCX-done messages failed");
    }
}

/// The PCI interrupt router, before enumeration.
pub fn route_interrupts(fch: &Fch<impl Bus>, config: &Config) -> Result<(), &'static str> {
    taishan::irq::write_routing(fch.bus(), config.fch.irq_routes)
        .map_err(|_| "PCI interrupt routing")
}

/// After the resource allocation, before the board's final hook.
pub fn after_resources(fch: &Fch<impl Bus>) {
    fch.acpi_ports(taishan::ACPI_PORTS);
}

/// The ReadyToBoot of the vendor firmware: the SATA controller of die 0 at
/// `sata`, the other dies' FCHs, the FCH late init, the board's hook, then
/// the DF extended config off.
pub fn ready_to_boot<P: ConfigRegionAccess + Copy>(
    dies: &[Die<P>],
    fch: &Fch<impl Bus>,
    sata: Option<PciAddress>,
    board: impl FnOnce(),
) {
    if let Some(sata) = sata {
        taishan::sata::late(&dies[0].smn(), 0, dies[0].pci(), sata);
    }
    taishan::multi::late(dies, |_| None);
    taishan::late_init(fch);
    board();
    for die in dies {
        df::zeppelin::disable_ext_cfg(&die.df());
    }
}
