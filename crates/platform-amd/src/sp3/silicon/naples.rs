//! Naples silicon steps: DF2, one NBIO and SMU V9 per ZP die, per-die
//! DXIO, Taishan FCHs on every die and the per-die SMU thread release
//! (INTEGRATION_NOTES §3–§9, the Naples paragraphs).
extern crate alloc;
use super::cpu::{Sp3Cpu, release_all, this_thread};
use super::hw::{Pci, Tsc, msr, secondary_fch};
use super::hw::{SMU_POLL_US, SMU_TIMEOUT_US};
use super::steps::*;
use fstart_arch::x86::cpu::msr::MsrAccess;
use fstart_arch::x86::mp::MpHandle;
use fstart_driver_amd::ccx::microcode;
use fstart_driver_amd::df::regs;
use fstart_driver_amd::df::v2::Df2;
use fstart_driver_amd::dxio::wire::MAX_ENGINES;
use fstart_driver_amd::dxio::{DMA_SIZE, DmaBuffer, Dxio, Unit};
use fstart_driver_amd::nbio::iohc::DramTops;
use fstart_driver_amd::nbio::iohc::PSP_BAR_SIZE as BAR_SIZE;
use fstart_driver_amd::smn::Smn;
use fstart_driver_amd::smu::Smu as SmuMailbox;
use fstart_driver_amd::smu::v9;
use fstart_driver_amd::sp3::ccx::release::{self, Launch, SmuPwr};
use fstart_driver_amd::sp3::ccx::x2apic::{self, ApicMode};
use fstart_driver_amd::sp3::ccx::{Ccx, brand, mca, power, security};
use fstart_driver_amd::sp3::dxio;
use fstart_driver_amd::sp3::dxio::lanes::Engine;
use fstart_driver_amd::sp3::dxio::ports::{FchPerst, PortSetup};
use fstart_driver_amd::sp3::fch;
use fstart_driver_amd::sp3::fch::multi::{self, NaplesDie};
use fstart_driver_amd::sp3::fch::taishan::{self, usb::DieUsb};
use fstart_driver_amd::sp3::nbio::fuse::Fuses;
use fstart_driver_amd::sp3::nbio::ioapic::nbio_ioapic_id;
use fstart_driver_amd::sp3::nbio::{Nbio, naples as nbio_naples};
use fstart_driver_amd::sp3::topology::MAX_ROOT_BRIDGES;
use fstart_driver_amd::window::{SmnWindow, Window};
use tock_registers::interfaces::Readable;
use tock_registers::interfaces::Writeable;
use zerocopy::IntoBytes;
pub type DfVersion = Df2;
/// SMU V9 mailbox of one die.
pub type Smu = SmuMailbox<Pci, Tsc>;
/// Per-die handles, physical order (= `Topology::root_bridges`: one root
/// bridge per die).
pub struct Silicon {
    pub nbios: heapless::Vec<Nbio<Pci>, MAX_ROOT_BRIDGES>,
    pub smus: heapless::Vec<Smu, MAX_ROOT_BRIDGES>,
    /// The threads MP brings up and the order the SMU releases them in,
    /// once the CPU driver is made.
    threads: u16,
    order: heapless::Vec<Launch, { release::MAX_THREADS }>,
    cac: power::CacWeights,
}
const IOMMU_BAR: u64 = 512 << 10;
const IOAPIC_BAR: u64 = 0x1000;

/// Populated UMCs per node from the APOB SPD copy (bit = UMC).
fn umc_present(ctx: &Ctx) -> [u8; 8] {
    let mut present = [0u8; 8];
    for (node, n) in ctx.topology.nodes.iter().enumerate() {
        present[node] = fstart_driver_amd::apob::mem::spd(&ctx.apob, n.socket, n.die)
            .map(|t| {
                t.dimms()
                    .filter(|(_, _, r)| r.present())
                    .fold(0u8, |m, (c, _, _)| m | 1 << (c & 7))
            })
            .unwrap_or(0);
    }
    present
}

impl Steps for Silicon {
    type DfLayout = DfVersion;

    /// DF PEI init and, as this flow has no PEI/DXE split, the DXE-entry PIE
    /// power management right after (SOC_DF §3, DF_ZP §3.9).
    fn df_init(ctx: &mut Ctx) -> Result<(), &'static str> {
        use fstart_driver_amd::sp3::df::naples;
        let policy = naples::Policy::default();
        let rev = ctx.identity.revision;
        naples::init(&ctx.df, &ctx.topology, rev, &policy, &umc_present(ctx));
        naples::late_init(&ctx.df, &ctx.topology, rev, &policy, false);
        Ok(())
    }

    /// NBIO base init per die, TOM, PSP BAR on every die (PSP_APOB §3.2:
    /// AGESA only die 0; a superset).
    fn nbio_early(ctx: &mut Ctx) -> Result<Silicon, &'static str> {
        let policy = ctx.policy.nbio;
        let rev = ctx.identity.revision;
        let nbios: heapless::Vec<Nbio<Pci>, MAX_ROOT_BRIDGES> = ctx
            .topology
            .root_bridges
            .iter()
            // SAFETY: discovered root bridges with routed buses.
            .map(|rb| unsafe { Nbio::new(ctx.pci, ctx.generation, rb, Fuses::NONE) })
            .collect();
        let tops = DramTops {
            tom: ctx.tops.tom,
            tom2: ctx.tops.tom2,
        };
        for (i, n) in nbios.iter().enumerate() {
            nbio_naples::base_init(n, &policy, rev, None);
            n.set_tom(tops);
            n.set_psp_bar(ctx.non_pci[i].alloc(BAR_SIZE).step("PSP BAR space")?);
        }
        ctx.psp_bar = nbios[0].psp_bar().step("PSP BAR")?;
        msr()
            .reg(fstart_driver_amd::ccx::msr::PSP_ADDRESS)
            .set(ctx.psp_bar);
        Ok(Silicon {
            nbios,
            smus: heapless::Vec::new(),
            threads: 0,
            order: heapless::Vec::new(),
            cac: power::CacWeights::new(),
        })
    }

    /// SMU V9 DXE entry per die: policy table, AGM log, perfmon pages.
    fn smu_init(ctx: &mut Ctx, si: &mut Silicon) -> Result<(), &'static str> {
        for n in &si.nbios {
            let smu = Smu::new(n.smn(), v9::MAILBOX, ctx.timer, v9::ID_LIMIT)
                .with_timeout_us(SMU_TIMEOUT_US)
                .with_poll_us(SMU_POLL_US);
            let table = v9::PolicyTable::new(&v9::Policy::default());
            let dma = dma_alloc(0x1000, 0x1000)?;
            dma[..table.as_bytes().len()].copy_from_slice(table.as_bytes());
            let agm = dma_alloc(0x1000, 0x1000)?;
            let perfmon = dma_alloc(0x1000, 0x1000)?;
            let buffers = v9::DxeBuffers {
                policy_table: dma.as_ptr() as u64,
                agm_log: agm.as_ptr() as u64,
                perfmon: perfmon.as_ptr() as u64,
            };
            v9::dxe_init(&smu, n.bus(), &buffers).step("SMU DXE init")?;
            let _ = si.smus.push(smu);
        }
        Ok(())
    }

    /// USB of die 0 and the secondary FCHs of dies 1..7 (FCH_SP3 §4.1).
    fn fch_secondaries(ctx: &mut Ctx, si: &Silicon) -> Result<(), &'static str> {
        taishan::usb::reset(&ctx.fch, DieUsb::default_for(0), ctx.resume);
        for (node, n) in si.nbios.iter().enumerate().skip(1) {
            let hw = secondary_fch(n.bus(), ctx.timer);
            multi::naples_die(&hw, &NaplesDie::default_for(node as u8), ctx.resume);
        }
        Ok(())
    }

    /// DXIO per die, each run to completion (PCD 0x1E = 0, PCIE_DXIO §3.3).
    fn dxio(
        ctx: &mut Ctx,
        si: &Silicon,
        engines: &heapless::Vec<Engine<'static>, MAX_ENGINES>,
    ) -> Result<(), &'static str> {
        let generation = ctx.generation;
        let policy = ctx.policy.dxio;
        let mut units: heapless::Vec<Unit<'_, Smu>, MAX_ROOT_BRIDGES> = heapless::Vec::new();
        for (rb, smu) in ctx.topology.root_bridges.iter().zip(&si.smus) {
            let buffer = dma_alloc(DMA_SIZE, 0x1000)?;
            // SAFETY: a firmware-owned, identity-mapped buffer for this unit.
            let dma =
                unsafe { DmaBuffer::new(buffer.as_mut_ptr(), DMA_SIZE, buffer.as_ptr() as u64) }
                    .step("DXIO DMA buffer")?;
            let dxio = Dxio::new(smu, v9::msg::DXIO);
            let unit = (rb.socket, rb.index);
            let ancillary = Default::default();
            if let Some(unit) = Unit::new(generation, unit, dxio, dma, engines, &policy, &ancillary)
                .step("DXIO engines")?
            {
                let _ = units.push(unit);
            }
        }
        // SAFETY: the BSP owns these buses' SMN index/data pairs during init.
        let window = |smn| unsafe { SmnWindow::new(smn, fch::acpimmio::SMN_BASE) };
        let windows: heapless::Vec<_, 8> = ctx
            .topology
            .root_bridges
            .iter()
            .map(|rb| {
                (
                    (rb.socket, rb.index),
                    window(unsafe { Smn::on_bus(ctx.pci, rb.bus_base) }),
                )
            })
            .collect();
        let fchs = windows
            .iter()
            .map(|(k, w)| (*k, w as &dyn Window))
            .collect();
        // The donor also drives PERST on bus 0x40 (die 1 of the equal split).
        let also = si.nbios.get(1).map(|n| window(n.smn()));
        // SAFETY: live FCHs; DXIO owns their PERST GPIOs.
        let perst =
            unsafe { FchPerst::new(generation, fchs, also.as_ref().map(|w| w as &dyn Window)) };
        let mut setup = PortSetup::new(generation, &si.nbios, engines, perst);
        trained(dxio::train(
            generation, &mut units, &mut setup, &ctx.timer, &policy,
        ))?;
        setup.finish();
        setup.clear_early_buses();
        ctx.ports =
            dxio::ports::attached(&ctx.topology, ctx.pci_root.segment, engines, setup.ports());
        Ok(())
    }

    /// SATA env (die 0) and the xGbE ports off (FCH_SP3 §3.2 af49).
    fn fch_env(ctx: &Ctx, si: &Silicon) -> Result<(), &'static str> {
        taishan::sata::env(&ctx.fch, true);
        taishan::xgbe::disable(&ctx.fch, 0xf);
        for (node, n) in si.nbios.iter().enumerate().skip(1) {
            let hw = secondary_fch(n.bus(), ctx.timer);
            multi::naples_die_env(&hw, NaplesDie::default_for(node as u8).sata);
        }
        Ok(())
    }

    /// CCX on the BSP, SMU release per die, CCX on every thread, CCX-done per
    /// die (CCX_SP3 §2, NBIO_SMU §3.8 V9).
    /// CCX on the BSP and the driver every thread runs, with the patches
    /// of `microcode`; the order the threads are released in.
    fn cpu_driver(
        ctx: &mut Ctx,
        si: &mut Silicon,
        microcode: Option<&'static [u8]>,
    ) -> Result<Sp3Cpu, &'static str> {
        let generation = ctx.generation;
        let policy = ctx.policy.ccx;
        let mut dies = [[None; 4]; 2];
        for (rb, n) in ctx.topology.root_bridges.iter().zip(&si.nbios) {
            // SMU::PWR::THREAD_CONFIGURATION of the die.
            let cfg = n.smn().read32(SmuPwr::NAPLES.thread_cfg);
            dies[usize::from(rb.socket & 1)][usize::from(rb.index & 3)] = Some(cfg);
        }
        let order = release::naples_order(&dies[..usize::from(ctx.topology.sockets)])
            .step("thread release order")?;
        let threads = order.len() + 1;
        let decision = x2apic::decide(generation, policy.apic, threads);
        if decision.mode == ApicMode::X2Apic {
            return Err("APIC mode");
        }
        let ccx = Ccx::new(ctx.identity, decision.mode, policy.table).step("CCX")?;
        let cpu = msr();
        let bsp = this_thread(generation);
        ccx.run_tables(&cpu, &bsp);
        brand::write(&cpu, &brand::read(generation, &si.smus[0]));
        let candidates = microcode_candidates(microcode);
        let patch = microcode::select(ctx.identity.signature, candidates.iter().copied());
        if let Some(p) = &patch {
            // SAFETY: ring 0 with caching enabled; the FFS copy of the patch is
            // identity mapped and stays reserved.
            unsafe { ccx.load_microcode(&cpu, &bsp, p) }.step("microcode")?;
        }
        ccx.early(&cpu, &bsp);
        // SAFETY: DfGlobalCtrl of node 0, read only.
        let df_global = unsafe { ctx.df.node(0, regs::GLOBAL_CTRL) }.get();
        let wdt = policy
            .wdt
            .and_then(|w| power::wdt_setting(generation, w, df_global));
        let cac = power::read_cac_weights(generation, &si.smus[0]).step("CAC weights")?;
        let driver = Sp3Cpu::new(
            ccx,
            policy,
            patch,
            wdt,
            policy.wdt_severity,
            cac.clone(),
            [Some(ctx.psp_bar), None],
        );
        si.threads = threads as u16;
        si.order = order;
        si.cac = cac;
        Ok(driver)
    }

    fn threads(si: &Silicon) -> u16 {
        si.threads
    }

    fn release_aps(ctx: &Ctx, si: &Silicon, aps: u16) -> Result<(), u32> {
        let generation = ctx.generation;
        if usize::from(aps) != si.order.len() {
            return Err(0);
        }
        let node = |l: &Launch| {
            ctx.topology
                .root_bridges
                .iter()
                .position(|rb| rb.socket == l.socket && rb.index == l.die)
        };
        release_all(&si.order, |l| {
            let n = node(l).ok_or(release::ReleaseError::BadTopology {
                socket: l.socket,
                die: l.die,
            })?;
            release::launch(&si.nbios[n].smn(), &si.smus[n], l, generation)
        })
    }

    /// Every thread ran its CCX steps: the BSP's last ones, then the SMU's
    /// CCX-done.
    fn cpus_up(
        ctx: &mut Ctx,
        si: &mut Silicon,
        _: &MpHandle,
        apic_ids: &[u32],
    ) -> Result<(), &'static str> {
        let generation = ctx.generation;
        let policy = ctx.policy.ccx;
        let cpu = msr();
        let bsp = this_thread(generation);
        let cac = si.cac.clone();
        security::set_smee(&cpu, policy.smee);
        mca::init(&cpu, &bsp);
        power::write_cac_weights(&cpu, &cac);
        ctx.apic_ids = apic_ids.to_vec();
        // TODO(naples): the QOS bit (PCD 0x17) and the per-revision removals
        // are not decoded (NBIO_SMU open item); the SP3 base mask is sent.
        let features = v9::feature_mask(false, 0);
        for (i, smu) in si.smus.iter().enumerate() {
            v9::ccx_done(smu, features).step("SMU CCX done")?;
            v9::after_ap_launch(smu, i == 0).step("SMU after AP launch")?;
        }
        Ok(())
    }

    fn pci_pre(ctx: &mut Ctx, si: &mut Silicon) -> Result<(), &'static str> {
        let generation = ctx.generation;
        set_fch_ioapic_id(generation);
        for (i, n) in si.nbios.iter().enumerate() {
            n.set_ioapic_bar(ctx.non_pci[i].alloc(IOAPIC_BAR).step("IOAPIC BAR space")?);
            n.set_ioapic_id(nbio_ioapic_id(generation, i as u8));
            n.set_iommu_bar(ctx.non_pci[i].alloc(IOMMU_BAR).step("IOMMU BAR space")?);
        }
        Ok(())
    }

    fn pci_post(_ctx: &Ctx, si: &Silicon) -> Result<(), &'static str> {
        for n in &si.nbios {
            n.nbif_hw_init_lock();
            n.iommu_enable();
        }
        Ok(())
    }

    /// Taishan has no late task beyond the shared `common::late` one.
    fn fch_late(_ctx: &Ctx, _si: &Silicon) -> Result<(), &'static str> {
        Ok(())
    }

    /// Every die's root bus: each die has its USB controllers.
    fn usb_buses(ctx: &Ctx, si: &Silicon) -> Result<[u8; 8], &'static str> {
        let mut buses = [0; 8];
        si.nbios
            .iter()
            .zip(buses.iter_mut())
            .for_each(|(n, b)| *b = n.bus());
        let _ = ctx;
        Ok(buses)
    }

    fn usb_bus_count(ctx: &Ctx) -> u8 {
        ctx.topology.root_bridges.len() as u8
    }

    /// Naples domains (one per die unless interleaved, ACPI_SMM §3.5) are not
    /// wired: one domain.
    fn numa(_ctx: &Ctx, _si: &Silicon) -> Option<NumaPlan> {
        None
    }

    fn nbios(&self) -> &[Nbio<Pci>] {
        &self.nbios
    }
}
