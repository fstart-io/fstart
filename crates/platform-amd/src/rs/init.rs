// SPDX-License-Identifier: MIT OR Apache-2.0
//! The RS SoCs on the flow: their silicon steps at the flow's phases and
//! the flow's services for them. The order is the vendor's where the flow
//! lets it be: the fabric and the NBIO before the links, MPIO, then USB,
//! the CPUs, PCI, the tables.
//!
//! | POST | Step |
//! |---|---|
//! | 20 | DF4 topology, MMIO and I/O routes |
//! | 30 | NBIO, IOMMU, IOAPICs, CPM GPIO |
//! | 31 | SMU |
//! | 38 | MPIO |
//! | 3a | USB |
//! | 50 | MP |
//! | 58 | SMM, the PSP's SMM info and EINJ |
//! | 60 | PCI |
//! | 70 | ACPI, SMBIOS |
extern crate alloc;

use super::{Config, ECAM_BASE, ECAM_BUSES, MAX_THREADS, Rs, TSEG_SIZE, Variant};
use crate::flow::{Ctx, Mapped, Silicon as Up, Tables};
use crate::layout::Region;
use crate::soc::{Entered, Fabric, Init};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_arch::x86::mp::{CpuIdentity, MpHandle};
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::apob::{self, Apob, sysmap};
use fstart_driver_amd::ccx::{self, mp::Fam17hCpu, rs as cpu, tsc::Tsc};
use fstart_driver_amd::df::plan::RootBridgePlan;
use fstart_driver_amd::df::{RootBridge, plan, v4};
use fstart_driver_amd::dxio::rs::{self as mpio, session};
use fstart_driver_amd::fch::{FchHw, Native, songshan};
use fstart_driver_amd::nbio::{self, iohc, rs::Ios};
use fstart_driver_amd::psp::{self, C2pTransport};
use fstart_driver_amd::smu::{self, Smu, v13};
use fstart_pci::{PciAddress, PciEcam, ecam::GlobalEcam};
use heapless::Vec;
use tock_registers::interfaces::Readable;

/// POST `code` and log the step. Generic, as every function with a string
/// here: the SoC's code is only built for the boards that run it.
fn step<V: Variant>(code: u8, name: &str) {
    fstart_driver_amd::fch::post_native(code);
    fstart_log::info!("{}: [{:#x}] {}", V::NAME, code, name);
}

/// The handles of the flow.
pub struct Hw {
    pub fch: Native<Tsc>,
    pub clock: Tsc,
    pub revision: cpu::Revision,
}

/// What an IOS's NBIO placed in its non-PCI MMIO.
#[derive(Clone, Copy, Default)]
pub(super) struct Bars {
    pub iommu: Option<nbio::iommu::Iommu>,
    pub ioapic: Option<u64>,
}

/// What the silicon steps hand on.
pub struct State {
    roots: Vec<RootBridge, 4>,
    plan: plan::Plan,
    bars: [Bars; 4],
    cores: apob::rs::CoreMap,
    launch: Vec<cpu::Launch, MAX_THREADS>,
    /// The trained ports in PCI.
    ports: Vec<mpio::ports::Binding, 32>,
    /// The root port of every board port, MPIO's and the PSP's, by its
    /// first lane, and whether its link is up.
    lanes: Vec<(u8, PciAddress, bool), 32>,
    /// IOS0's PSP BAR, below 4 GiB: the PSP answers there
    /// ([`Init::init_fch`]).
    psp_bar: u32,
    /// The normal-mode PSP mailbox's one owner, from the PSP BAR on: its
    /// commands use retained storage, and none follows a failed one.
    psp: Option<psp::boot::Psp>,
    /// The PSP's EINJ service, once SMM opened it.
    einj: Option<Einj>,
}

/// The PSP's ACPI EINJ service: its RAS mailbox register and its command
/// buffer ([`psp::einj`]).
#[derive(Clone, Copy)]
struct Einj {
    mailbox: u64,
    buffer: u64,
}

impl State {
    #[inline]
    pub(super) fn roots(&self) -> &[RootBridge] {
        &self.roots
    }

    #[inline]
    pub(super) fn bars(&self, ios: u8) -> Bars {
        self.bars[usize::from(ios)]
    }

    #[inline]
    pub(super) fn ios(&self) -> impl Iterator<Item = Ios<GlobalEcam>> + '_ {
        // SAFETY: the bus is routed to the IOS; the BSP owns the SMN pairs
        // during the silicon init.
        self.roots
            .iter()
            .map(|rb| unsafe { Ios::new(GlobalEcam, rb.index, rb.bus_base) })
    }

    /// The physical CCD of every thread in MP order: the BSP's, then the
    /// APs' in release order.
    #[inline]
    pub(super) fn thread_ccds(&self) -> alloc::vec::Vec<u8> {
        let bsp = self.cores.ccds.first().map(|ccd| ccd.id);
        bsp.into_iter()
            .chain(self.launch.iter().map(|ap| ap.ccd))
            .collect()
    }

    /// The root port of the board port whose first lane is `lane`.
    #[inline]
    pub(super) fn port(&self, lane: u8) -> Option<PciAddress> {
        self.lanes
            .iter()
            .find(|&&(first, ..)| first == lane)
            .map(|&(_, port, _)| port)
    }

    /// The FCH's IOS; [`v4::root_bridges`] made sure there is one.
    #[inline]
    pub(super) fn primary(&self) -> Ios<GlobalEcam> {
        self.ios()
            .find(|ios| ios.fch())
            .expect("the topology has the FCH's IOS")
    }
}

/// The non-PCI MMIO an NBIO's blocks go to: the IOMMU's 512 KiB, the
/// IOAPIC above it, and on IOS0 ([`nbio::rs::PSP_IOS`]) the PSP's 1 MiB.
const IOAPIC_OFFSET: u64 = nbio::iommu::MMIO_SIZE;
const PSP_OFFSET: u64 = 1 << 20;

impl<V: Variant> Init for Rs<V> {
    const NAME: &'static str = V::NAME;

    type Config = Config;
    type Identity = cpu::Revision;
    type Hw = Hw;
    /// The board's MPIO ports.
    type Lanes<'a> = &'a [session::Config<'a>];
    type Runtime = ();
    type FchUp = State;
    type Trained = State;
    type Chipset = State;
    type Cpu = Fam17hCpu<Self, cpu::Threads>;
    type State = State;

    /// ECAM at 0xE000_0000, the FCH taken over from the PSP.
    fn enter(_: &Config, msr: &NativeMsr) -> Entered<Self> {
        fstart_driver_amd::fch::post_native(0x10);
        ccx::msr::enable_ecam(msr, ECAM_BASE, ECAM_BUSES);
        fstart_pci::ecam::init(ECAM_BASE as usize);
        // SAFETY: exclusive FCH programming before the console.
        unsafe { fstart_driver_amd::fch::acpimmio::enable_acpimmio() };
        let clock = Tsc::measured(msr);
        // SAFETY: BSP only, single-threaded cold boot; ACPI MMIO and ECAM
        // are decoded.
        let fch = unsafe { Native::new(Tsc::native(msr)) };
        songshan::take_over(&fch);
        let revision = cpu::Revision::of(CpuIdentity::current());
        Entered {
            hw: Hw {
                fch,
                clock: clock.unwrap_or_else(|| Tsc::native(msr)),
                revision: revision.unwrap_or(cpu::Revision::RsdnA2),
            },
            clock,
            pre_init: revision.map(drop).ok_or("CPU revision"),
        }
    }

    fn identify(hw: &Hw, _: &Config, _: Option<Tsc>) -> Result<cpu::Revision, &'static str> {
        let signature = CpuIdentity::current().signature;
        fstart_log::info!(
            "{}: CPU {} ({:#x}), TSC {} MHz",
            V::NAME,
            hw.revision.name(),
            signature,
            hw.clock.mhz()
        );
        Ok(hw.revision)
    }

    fn pci_segment(_: &Hw) -> PciRootInfo {
        PciRootInfo {
            segment: 0,
            ecam_base: ECAM_BASE,
            bus_start: 0,
            bus_end: (ECAM_BUSES - 1) as u8,
        }
    }

    /// Milan's mailbox rules and APOB container.
    fn psp(_: &Hw) -> psp::Variant {
        psp::Variant::Gn
    }

    /// The fabric (DF4: the IOS the part has, bound to the PSP's bus maps,
    /// then the planner's MMIO and I/O routes), the NBIO of every IOS, its
    /// IOMMU and IOAPIC, the board's GPIO, the ACPI blocks and the SMU.
    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<State, &'static str> {
        let config = ctx.config;
        let fch = &ctx.hw.fch;
        step::<V>(0x20, "DF4");
        // SAFETY: the BSP owns the DF's indirect pair during the init.
        let df = unsafe { v4::Df::new(GlobalEcam, 0) };
        if df
            .reg(v4::SYSTEM_CONFIG)
            .is_set(v4::SystemConfig::OTHER_SOCKET_PRESENT)
        {
            return Err("one RS socket");
        }
        let ios = V::ios(&fch.smn()).ok_or("IOS fuses")?;
        let routes = v4::bus_routes(&df).map_err(|_| "DF4 bus maps")?;
        let roots = v4::root_bridges(0, &ios, &routes).map_err(|_| "DF4 bus maps")?;
        let tops = ccx::msr::memory_limits(ctx.msr).map_err(|_| "TOP_MEM")?;
        let plan = plan::plan(
            &plan::Input {
                tom: tops.0,
                tom2: tops.1.unwrap_or(0),
                ecam_base: ECAM_BASE,
                ecam_size: u64::from(ECAM_BUSES) << 20,
                phys_addr_bits: fstart_arch::x86::physical_address_bits() as u8,
                root_bridges: &roots,
                mmio_pairs: v4::MMIO_PAIRS,
            },
            &plan::Policy::df3(),
        )
        .map_err(|_| "DF plan")?;
        v4::program(&df, &plan.mmio, &plan.io);
        for rb in &plan.root_bridges {
            fstart_log::info!(
                "rs: IOS {} buses {:#x}-{:#x} mmio {:#x}+{:#x} above {:#x} io {:#x}",
                rb.index,
                rb.bus.base,
                rb.bus.limit,
                rb.below.base,
                rb.below.size,
                rb.above.map_or(0, |w| w.base),
                rb.io.map_or(0, |w| w.base)
            );
        }
        let mut state = State {
            roots,
            plan,
            bars: Default::default(),
            cores: apob::rs::core_map(ctx.memory.apob.ok_or("APOB")?, 0)
                .map_err(|_| "APOB core map")?,
            launch: Vec::new(),
            ports: Vec::new(),
            lanes: Vec::new(),
            psp_bar: 0,
            psp: None,
            einj: None,
        };

        step::<V>(0x30, "NBIO");
        let dense = ctx.hw.revision.dense();
        state.ios().for_each(|n| nbio::rs::early(&n));
        for n in state.ios() {
            nbio::rs::base_init(&n, config.nbio);
            // SAFETY: D0F0x90 of the IOS's host bridge.
            let slot1 = unsafe {
                fstart_pci::ConfigRegister::new(
                    GlobalEcam,
                    fstart_pci::PciAddress::new(0, n.bus(), 0, 0),
                    iohc::NB_TOP_OF_DRAM_SLOT1,
                )
            };
            let tops = iohc::DramTops {
                tom: tops.0,
                tom2: tops.1,
            };
            iohc::set_tom(&n, &slot1, tops, iohc::TopStyle::Tom3);
        }
        // The IOMMU, the IOAPIC above it and on IOS0 the PSP in the root
        // bridge's non-PCI MMIO. The PSP BAR is IOS0's IOHC register.
        let mut psp_bar = None;
        for rb in &state.plan.root_bridges {
            // SAFETY: as [`State::ios`].
            let n = unsafe { Ios::new(GlobalEcam, rb.index, rb.bus.base) };
            let pool = rb.below_pools.non_pci.base;
            let (iommu, ioapic) = (pool as u32, pool + IOAPIC_OFFSET);
            if !nbio::rs::map_iommu(&n, iommu) {
                return Err("IOMMU capability");
            }
            nbio::rs::iommu_init(&n, dense, config.nbio.clock_gating);
            nbio::rs::map_ioapic(&n, ioapic, super::tables::ioapic_id(n.index));
            state.bars[usize::from(n.index)] = Bars {
                // SAFETY: the IOMMU's registers in the non-PCI MMIO,
                // identity mapped uncached.
                iommu: unsafe { nbio::rs::iommu(&n, iommu) },
                ioapic: Some(ioapic),
            };
            if n.index == nbio::rs::PSP_IOS {
                let bar = pool + PSP_OFFSET;
                iohc::map_psp(&n, bar as u32).map_err(|_| "PSP BAR")?;
                psp_bar = Some(bar);
            }
        }
        // The PSP answers in the window: a window routed to an IOS without
        // the PSP's registers reads all ones.
        let psp_bar = psp_bar.ok_or("PSP BAR")?;
        // SAFETY: the BAR just mapped, uncached in the identity map; read
        // only.
        let c2p = unsafe { psp::MmioTransport::new(psp_bar, psp::Variant::Gn) }
            .read(psp::mailbox::C2pReg::Cmd);
        fstart_log::info!("rs: PSP BAR {:#x}, C2P {:#x}", psp_bar, c2p);
        if c2p == u32::MAX {
            return Err("PSP BAR");
        }
        state.psp_bar = u32::try_from(psp_bar).map_err(|_| "PSP BAR")?;
        state.psp = Some(psp_session(state.psp_bar)?);
        // Every IOAPIC masked before a device can interrupt; the FCH's has
        // ID 0 and passes the 8259's interrupt through to the BSP.
        fstart_driver_amd::fch::acpimmio::enable_hpet_ioapic(fch.acpi());
        // SAFETY: the decoded IOAPICs, BSP only.
        unsafe {
            let fch_ioapic =
                fstart_arch::x86::ioapic::IoApic::new(fstart_driver_amd::fch::IOAPIC as usize);
            fch_ioapic.set_id(super::tables::FCH_IOAPIC_ID);
            fch_ioapic.mask_all();
            for ioapic in state.bars.iter().filter_map(|b| b.ioapic) {
                fstart_arch::x86::ioapic::IoApic::new(ioapic as usize).mask_all();
            }
        }
        fstart_driver_amd::fch::gpio::configure_all(fch.acpi(), config.gpio);
        fstart_driver_amd::fch::acpimmio::acpi_ports(
            fch.acpi(),
            super::acpi::soc(V::PMBASE, false).ports,
        );

        step::<V>(0x31, "SMU");
        let smu = smu_of::<V>(&state, ctx.clock);
        let mut test = [0xaa55_aa55, 0, 0, 0, 0, 0];
        smu::SmuMessages::send(&smu, smu::TEST, &mut test).map_err(|_| "SMU test")?;
        let mut version = [0; 6];
        smu::SmuMessages::send(&smu, v13::GET_VERSION, &mut version).map_err(|_| "SMU")?;
        fstart_log::info!("rs: SMU firmware {:#x}", version[0]);
        Ok(state)
    }

    /// MPIO: the board's ports sent, mapped, placed in PCI, configured, the
    /// board's PERST released, trained and updated; the untrained bridges
    /// hidden.
    fn train_dxio(
        ctx: &Ctx<'_, Self>,
        mut state: State,
        ports: &[session::Config<'_>],
    ) -> Result<State, &'static str> {
        step::<V>(0x38, "MPIO");
        let trained = super::mpio::train(ctx, &state, ports)?;
        (state.ports, state.lanes) = (trained.ports, trained.lanes);
        state.ios().for_each(|n| nbio::rs::after_training(&n));
        Ok(state)
    }

    /// The xHCIs through the SMU.
    fn init_chipset(ctx: &Ctx<'_, Self>, state: State, (): ()) -> Result<State, &'static str> {
        step::<V>(0x3a, "USB");
        let smu = smu_of::<V>(&state, ctx.clock);
        songshan::usb::ColdBoot::new(&ctx.config.usb)
            .and_then(|mut usb| usb.run(state.primary().smn(), &smu))
            .map_err(|_| "USB")?;
        Ok(state)
    }

    fn cpu_driver(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        microcode: Option<&'static [u8]>,
    ) -> Result<Self::Cpu, &'static str> {
        step::<V>(0x50, "MP");
        state.launch = cpu::launch_order(&state.cores).map_err(|_| "CPU topology")?;
        let signature = CpuIdentity::current().signature;
        let patch = microcode.and_then(|bytes| {
            ccx::microcode::Bundle::<Self>::parse(bytes)
                .ok()?
                .select(signature)
                .ok()
        });
        match &patch {
            Some(patch) => fstart_log::info!("rs: microcode patch {:#x}", patch.revision()),
            None => fstart_log::warn!("rs: no microcode patch for this CPU"),
        }
        Ok(Fam17hCpu::multi_socket(
            V::NAME,
            cpu::Threads {
                revision: ctx.hw.revision,
                policy: ctx.config.ccx,
                microcode: patch,
            },
            cpu::thread_init,
        ))
    }

    fn threads(_: &Ctx<'_, Self>, state: &State) -> Result<u16, &'static str> {
        Ok(state.cores.threads() as u16)
    }

    /// One AP at a time, each released once the previous one checked in.
    fn release_aps(_: &Ctx<'_, Self>, state: &State, aps: u16) -> Result<(), u32> {
        if usize::from(aps) != state.launch.len() {
            return Err(0);
        }
        let smn = state.primary();
        let smn = smn.smn();
        for (i, &ap) in state.launch.iter().enumerate() {
            let index = i as u32 + 1;
            if !cpu::release(smn, ap) {
                return Err(index);
            }
            if !fstart_arch::x86::wait_us(50_000, || {
                u32::from(fstart_arch::x86::mp::ap_checkins()) >= index
            }) {
                fstart_log::error!("rs: AP {} on CCD {} did not check in", index, ap.ccd);
                return Err(index);
            }
        }
        Ok(())
    }

    fn cpu_errors(cpu: &Self::Cpu) -> u32 {
        cpu.errors()
    }

    fn cpus_up(
        _: &Ctx<'_, Self>,
        state: State,
        _: &MpHandle,
        _: &[u32],
    ) -> Result<State, &'static str> {
        Ok(state)
    }

    fn fabric<'a>(ctx: &Ctx<'_, Self>, state: &'a State) -> Fabric<'a> {
        Fabric {
            segment: Self::pci_segment(ctx.hw),
            bridges: &state.plan.root_bridges,
        }
    }

    fn display_port(ctx: &Ctx<'_, Self>, state: &State, _: &PciEcam) -> Option<PciAddress> {
        state.port(ctx.config.display?)
    }

    fn route_legacy_vga(_: &Ctx<'_, Self>, _: &State, root: &RootBridgePlan) {
        // SAFETY: the BSP owns the DF.
        let df = unsafe { v4::Df::new(GlobalEcam, 0) };
        v4::route_vga(&df, root.fabric_id);
    }

    /// The transmit requester ID of every visible PCIe root port, at the
    /// devfn MPIO's remap gave its slot: the identity of what the port
    /// sends itself (its MSIs). A port MPIO did not train (the Onyx's
    /// PSP-trained BMC link) needs it as much as the others: left at its
    /// reset value 0, its MSIs reach the IOMMU as 00:00.0's, which blocks
    /// them (INVALID_DEVICE_REQUEST, posted write to the interrupt range).
    fn after_resources(_: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        for n in state.ios() {
            let devfns = mpio::ports::remap(n.index, &state.ports);
            for slot in 0..nbio::rs::pcie_bridges(n.index) {
                let devfn = devfns[usize::from(slot)];
                if fstart_pci::EcamDevice::new(n.bus(), devfn >> 3, devfn & 7).is_present() {
                    nbio::rs::set_requester(&n, slot, n.bus(), devfn);
                }
            }
        }
        Ok(())
    }

    /// What the board has soldered behind its ports is there, in the
    /// topology it has it in ([`super::links::Soldered`]).
    fn ready_to_boot(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        pci: &PciEcam,
    ) -> Result<(), &'static str> {
        for soldered in &ctx.config.mpio.required {
            if !state
                .port(soldered.lane)
                .is_some_and(|port| soldered.behind.found(pci, port))
            {
                fstart_log::error!(
                    "{}: lane {}: not the board's device behind the port",
                    V::NAME,
                    soldered.lane
                );
                return Err("MPIO endpoints");
            }
        }
        Ok(())
    }

    /// Direct ACPI mode: the OS gets SCI_EN set and no SMI command port.
    /// Then the PSP's locks: no DF write follows and SMM was locked when
    /// it was installed ([`psp::boot::lock`]: LockDFReg, then SmmLock, as
    /// the vendors send them once PCI is enumerated and at SMM's
    /// ready-to-lock). They need the SMM info, which `install_smm` made
    /// sure of; a refusal halts.
    fn finalize(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        songshan::acpi_mode(&ctx.hw.fch, &super::acpi::soc(V::PMBASE, false).ports);
        let psp = state.psp.as_mut().ok_or("PSP")?;
        psp::boot::lock(psp, &ctx.clock).map_err(|(what, error)| {
            let (why, value) = error.parts();
            fstart_log::error!("rs: PSP {}: {} {:#x}", what, why, value);
            "PSP locks"
        })?;
        fstart_log::info!("rs: PSP locked the DF and SMM");
        Ok(())
    }

    /// The PSP's ReadyToBoot, BootDone last ([`psp::boot::ready_to_boot`]);
    /// before it, what the PSP asked of SMM during the boot. The TPM's
    /// establishment flag only for a TPM the firmware started: the Onyx's
    /// PSP never completed it for a TPM nobody had (the vendors send it
    /// when `0xFED4_0000` answers, which it did there). A refusal halts:
    /// the OS must not get a PSP that still takes commands from outside
    /// SMM.
    fn boot_done(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        let report = &raw const SMM_REPORT;
        // SAFETY: firmware data, written only by the SMM handler on a PSP
        // SMI; a torn read only garbles this line.
        let (requests, command) = unsafe {
            (
                fstart_core::mmio::read32(&raw const (*report).p2c_requests),
                fstart_core::mmio::read32(&raw const (*report).p2c_command),
            )
        };
        fstart_log::info!(
            "rs: PSP SMIs answered: {} (last P2C command {:#x}), report at {:#x}",
            requests,
            command,
            report as u64
        );
        let psp = state.psp.as_mut().ok_or("PSP")?;
        // SAFETY: the APs are parked and SMM is locked.
        unsafe { psp::boot::ready_to_boot(psp, &ctx.clock, ctx.board.discrete_tpm) }.map_err(
            |(what, error)| {
                let (why, value) = error.parts();
                fstart_log::error!("rs: PSP {}: {} {:#x}", what, why, value);
                "PSP ReadyToBoot"
            },
        )?;
        fstart_log::info!("rs: PSP boot done");
        Ok(())
    }
}

/// The PSP's EINJ service once SMM opened it: its command buffer, which
/// must be the ABL's `ReservedRasEinj` hole, laid out as `ApeiEinjInstall`
/// leaves it for the EINJ table (the trigger action table in place, the
/// parameters cleared, the vendor types naming DF F0).
fn einj<V: Variant>(ctx: &Ctx<'_, Rs<V>>, bar: u64) -> Result<Einj, &'static str> {
    use fstart_driver_amd::apob::rs::HOLE_RAS_EINJ;
    use psp::einj::{Buffer, ERROR_TYPES, MAILBOX_OFFSET, Mailbox};
    let map =
        sysmap::system_memory_map(ctx.memory.apob.ok_or("APOB")?).map_err(|_| "APOB memory map")?;
    let hole = sysmap::memory_map(psp::Variant::Gn, map.top, map.holes())
        .map_err(|_| "memory map")?
        .into_iter()
        .find(|r| r.kind == sysmap::RegionKind::Unknown(HOLE_RAS_EINJ))
        .ok_or("no ReservedRasEinj hole")?;
    // SAFETY: IOS0's PSP BAR, mapped by `init_fch`; the BSP is the only
    // user of the RAS mailbox until the OS runs.
    let mailbox = unsafe { Mailbox::mmio(bar, psp::Variant::Gn) };
    let base = mailbox
        .buffer(&ctx.clock, &(hole.base..hole.end()))
        .map_err(|_| "the PSP's EINJ buffer")?;
    // SAFETY: the PSP's command buffer in the ABL's hole, identity mapped
    // and reserved; the PSP serves no request before BEGIN.
    let buffer = unsafe { Buffer::at(base) };
    let trigger = fstart_acpi::einj::trigger(&[psp::einj::trigger(base)]);
    let types = buffer.prepare(&trigger).map_err(|_| "EINJ trigger table")?;
    if types.is_set(ERROR_TYPES::VENDOR) {
        let df = fstart_pci::EcamDevice::new(0, 0x18, 0).read32(0);
        buffer.describe_vendor_device(0x18, 0, df);
    }
    // The OS reads the buffer through an uncached mapping; the PSP reads
    // DRAM.
    // SAFETY: ring 0, caches on.
    unsafe { fstart_arch::x86::writeback_invalidate_caches() };
    fstart_log::info!(
        "rs: PSP EINJ buffer {:#x}, error types {:#x}",
        base,
        types.get()
    );
    Ok(Einj {
        mailbox: bar + MAILBOX_OFFSET,
        buffer: base,
    })
}

/// What the PSP's SMM handler reports ([`super::smm::Report`]), in the
/// firmware's reserved DRAM.
static mut SMM_REPORT: super::smm::Report = super::smm::Report::NONE;

/// IOS0's PSP mailbox in normal mode at `bar`, its one owner. Once per
/// boot: its storage is the session's.
fn psp_session(bar: u32) -> Result<psp::boot::Psp, &'static str> {
    static mut STORAGE: psp::Storage = psp::Storage::new();
    // SAFETY: the BAR `init_fch` mapped, uncached in the identity map; the
    // BSP is the mailbox's one normal-mode user.
    let mailbox = unsafe { psp::Mailbox::mmio(bar.into(), psp::Variant::Gn) };
    // SAFETY: firmware BSS, identity mapped and reserved until reset; this
    // is its only reference (`init_fch` runs once).
    psp::Session::new(mailbox, unsafe { &mut *core::ptr::addr_of_mut!(STORAGE) }).map_err(|_| "PSP")
}

/// The SMU, through the FCH's IOS.
fn smu_of<V: Variant>(state: &State, clock: Tsc) -> Smu<GlobalEcam, Tsc> {
    let primary = state.primary();
    // SAFETY: the BSP owns the SMN pair of the FCH's root bus.
    let smn = unsafe { fstart_driver_amd::smn::Smn::on_bus(GlobalEcam, primary.bus()) };
    Smu::new(smn, v13::MAILBOX, clock, V::SMU_ID_LIMIT).with_timeout_us(2_000_000)
}

impl<V: Variant> crate::flow::Soc for Rs<V> {
    const TSEG_SIZE: u32 = TSEG_SIZE;
    type Flow = ();

    /// The APOB where the BIOS directory puts it, the memory map with the
    /// firmware's DRAM and TSEG reserved, the cache plan and the FFS.
    fn map(
        _: &mut Hw,
        _: &mut Config,
        board: &crate::board::Facts,
        msr: &NativeMsr,
        _: Tsc,
    ) -> Result<Mapped<Self>, &'static str> {
        use crate::memory::{self, Reservation, TopOfMemory};
        let (tom, tom2) = ccx::msr::memory_limits(msr).map_err(|_| "TOP_MEM")?;
        // SAFETY: the PSP wrote the APOB here before releasing the BSP and
        // the memory map keeps it reserved.
        let apob =
            unsafe { Apob::from_address(V::APOB.base, psp::Variant::Gn) }.map_err(|_| "APOB")?;
        let map = sysmap::system_memory_map(&apob).map_err(|_| "APOB memory map")?;
        let regions =
            sysmap::memory_map(psp::Variant::Gn, map.top, map.holes()).map_err(|_| "memory map")?;
        let tseg = memory::place_tseg(
            &regions,
            tom,
            u64::from(TSEG_SIZE),
            &[V::LAYOUT.early, V::APOB],
        )
        .ok_or("TSEG placement")?;
        let reservations = [
            Region::new(0, 0x1000),
            Region::new(fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS, 0x1000),
            Region::new(0x9_f000, 0x6_1000),
            V::APOB,
            V::LAYOUT.early,
            tseg,
        ]
        .map(Reservation::reserved);
        let e820 = memory::build(V::NAME, &regions, TopOfMemory { tom, tom2 }, &reservations)
            .map_err(|_| "memory map")?;
        // RAM below 4 GiB WB in the MTRRs, above through TOM2, as SP3.
        crate::sp3::flow::cache_ram(&regions);
        if board.discrete_tpm {
            // SAFETY: the boot flash below 4 GiB, read only.
            let flash = unsafe {
                core::slice::from_raw_parts(
                    V::LAYOUT.flash.base as *const u8,
                    V::LAYOUT.flash.size as usize,
                )
            };
            crate::tpm::start(
                crate::media::reset_image(
                    flash,
                    fstart_amdfw_format::efs::BiosDirectorySlot::Family19h,
                ),
                true,
            )?;
        }
        let ffs = V::LAYOUT.ffs.ok_or("boot media")?;
        fstart_arch::x86::boot::enable_boot_media_rom_cache();
        crate::media::mount(e820.entries(), &V::LAYOUT, ffs).map_err(|error| error.step())?;
        let nvram = V::NVRAM.map(|region| {
            // SAFETY: the variant reserves this read-only view of boot flash.
            unsafe { core::slice::from_raw_parts(region.base as *const u8, region.size as usize) }
        });
        let mut options = fstart_options::Report::new();
        if nvram.is_some() {
            let mut level = fstart_log::max_level();
            crate::options::resolve(
                nvram,
                &fstart_log::option::KNOBS,
                &mut level,
                &mut options,
                |_| Ok(()),
            );
            // SAFETY: BSP only, before MP.
            unsafe { fstart_log::set_max_level(level) };
        }
        Ok(Mapped {
            top_mem: tom,
            top_mem2: tom2,
            apob: Some(apob),
            tseg: tseg.base,
            e820,
            ffs: ffs.base..ffs.end(),
            nvram,
            options,
            flow: (),
        })
    }

    #[cfg_attr(not(feature = "payload-uefi"), allow(unused_mut))]
    fn launch(
        mut handoff: fstart_payload::Handoff<'_>,
        _ctx: &Ctx<'_, Self>,
        _state: &State,
        (): &(),
        _options: &fstart_options::Report,
    ) -> ! {
        #[cfg(feature = "payload-uefi")]
        if let (Some(nvram), Some(ffs)) = (V::NVRAM, V::LAYOUT.ffs) {
            use fstart_driver_amd::fch::{SPI_BASE, lpc, spi::flash::Extent};
            use fstart_driver_amd::window::Window;
            let base = _ctx
                .hw
                .fch
                .lpc()
                .at(lpc::SPI_BASE_ADDRESS)
                .read(lpc::SpiBase::BASE);
            handoff.settings = Some(crate::options::settings(_options));
            if base == SPI_BASE >> 8 {
                let flash_base = V::LAYOUT.flash.base;
                let extent = Extent {
                    offset: (nvram.base - flash_base) as u32,
                    size: nvram.size as u32,
                };
                static mut STORAGE: Option<crate::options::FlashStorage> = None;
                // SAFETY: BSP only. It now hands exclusive ownership of SPI100
                // to CrabEFI, from DRAM, and writes only the reserved extent.
                // The command engine must match the ROM view of the FFS.
                let storage = unsafe {
                    let probe = core::slice::from_raw_parts(ffs.base as *const u8, 64);
                    let storage = &mut *core::ptr::addr_of_mut!(STORAGE);
                    *storage = crate::options::storage(
                        _ctx.clock,
                        extent,
                        (ffs.base - flash_base) as u32,
                        probe,
                    );
                    storage
                };
                handoff.variable_storage = storage
                    .as_mut()
                    .map(|s| s as &mut dyn fstart_payload::crabefi::StorageBackend);
            } else {
                fstart_log::warn!(
                    "options: SPI100 not decoded at its fixed base, settings are volatile"
                );
            }
        }
        fstart_payload::boot(handoff, ())
    }

    /// The PSP's SMM ([`super::smm`]) in TSEG on every thread, then SW
    /// SMI D0: SmmInfo, which the boot needs, and, if the board publishes
    /// EINJ, RasEinj ENABLE, which it does not. The SMI command port stays
    /// routed: the CPUs' local SMIs (MCA) arrive through it. The OS gets
    /// ACPI directly ([`Init::finalize`]).
    fn install_smm(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        mp: &MpHandle,
        (): &(),
    ) -> Result<(), &'static str> {
        use super::smm::{Config, Smi};
        use fstart_driver_amd::fch::smi;
        step::<V>(0x58, "SMM");
        let psp_bar = state.psp_bar;
        let report = &raw mut SMM_REPORT;
        let fch = &ctx.hw.fch;
        let smi = Smi {
            fch,
            config: Config {
                ports: super::acpi::soc(V::PMBASE, false).ports,
                psp_bar,
                einj: ctx.config.einj,
                report: report as u64,
            },
        };
        crate::tseg::install(V::NAME, mp, &smi, ctx.memory.tseg, TSEG_SIZE)?;
        smi::software_smi(fch.io(), crate::smm::sw::PSP_SMM_INFO);
        // SAFETY: firmware data only the handler writes. It reports
        // SmmInfo, then, if the board publishes EINJ and the PSP took
        // SmmInfo, RasEinj ENABLE. The SMI command port stays routed: the
        // CPUs' machine-check SMIs come through it.
        let (smm_info, einj_status) = crate::smm::status::answers(
            &ctx.clock,
            ctx.config.einj,
            || unsafe { fstart_core::mmio::read32(&raw const (*report).smm_info) },
            || unsafe { fstart_core::mmio::read32(&raw const (*report).einj) },
        );
        // The PSP's locks and BootDone need the SMM info: without it the
        // DF lock breaks DMA, and the payload must not get an unlocked DF.
        crate::smm::status::smm_info("rs", smm_info)?;
        // EINJ is optional: without it the boot goes on, without the table.
        let Some(einj_status) = einj_status else {
            return Ok(());
        };
        match einj_status {
            0 => {
                fstart_log::info!("rs: PSP RasEinj ENABLE status 0");
                state.einj = einj(ctx, psp_bar.into())
                    .inspect_err(|what| fstart_log::error!("rs: no EINJ: {}", what))
                    .ok();
            }
            status => fstart_log::error!("rs: PSP RasEinj ENABLE status {:#x}", status),
        }
        Ok(())
    }

    fn tables(
        ctx: &Ctx<'_, Self>,
        silicon: &mut Up<Self>,
        pci: &PciEcam,
        (): &(),
        identity: &crate::acpi::SmbiosIdentity<'_>,
        board: crate::acpi::Board,
    ) -> Tables {
        step::<V>(0x70, "ACPI, SMBIOS");
        if let Some(einj) = silicon.state.einj {
            silicon.tables.push(fstart_acpi::einj::table(
                fstart_acpi::OEM_ID,
                fstart_acpi::OEM_TABLE_ID,
                fstart_acpi::OEM_REVISION,
                &psp::einj::entries(einj.mailbox, einj.buffer),
            ));
        }
        let soc = super::acpi::soc(V::PMBASE, ctx.config.legacy_devices);
        let rsdp = super::tables::acpi::<V>(ctx, silicon, pci, &soc, &board);
        let smbios = super::tables::smbios::<V>(ctx, silicon, pci, identity);
        Tables {
            rsdp,
            smbios,
            s5: fstart_core::description::AcpiSleepControl::new(
                soc.ports.control,
                crate::acpi::S5_SLEEP_TYPE,
            ),
        }
    }

    fn map_payload_ram(
        e820: &fstart_core::services::memory_detect::E820State,
        pci: &PciEcam,
    ) -> Result<(), fstart_core::services::ServiceError> {
        crate::noncar::boot::map_ram(e820, pci)
    }
}
