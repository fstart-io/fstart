// SPDX-License-Identifier: MIT OR Apache-2.0
//! SP7 / WH silicon bring-up on the shared AMD boot flow.
//!
//! | POST | Step |
//! |---|---|
//! | 20 | DFv4.5 bus routes, MMIO and I/O planning (broadcast to all live local IODs) |
//! | 30 | WH NBIO (IOHC TOM, 64 KiB-aligned IOAPIC, 24-bridge INTx routes, PSP BAR), FCH |
//! | 38 | WH PERST release & MPIO v2 (`DOORBELL_V2`) idle probe |
//! | 3a | Chipset (no-op) |
//! | 50 | BSP microcode (BSP-only MP) |
//! | 58 | SMM in 16 MiB TSEG (`rs::smm`, `psp::Variant::Gn`) |
//! | 60 | PCI enumeration |
//! | 70 | ACPI, SMBIOS |

extern crate alloc;

use core::num::NonZeroU32;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{
    Bars, Config, ECAM_BASE, ECAM_BUSES, LaneDescriptor, Sp7, TSEG_SIZE, Variant, place_bars,
    resolve_wh_roots,
};
use crate::acpi::{Cpu, Ioapic, RootBridgeAcpi};
use crate::flow::{Ctx, Mapped, Silicon as Up, Tables};
use crate::layout::Region;
use crate::soc::{Entered, Fabric, Init};
use fstart_arch::x86::cpu::msr::NativeMsr;
use fstart_arch::x86::ioapic::IoApic;
use fstart_arch::x86::mp::{CpuIdentity, MpHandle};
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::apob::{smbios::dram_ranges, sysmap, v2::ApobV2};
use fstart_driver_amd::ccx::{self, mp::Fam17hCpu, tsc::Tsc, wh as cpu};
use fstart_driver_amd::df::plan::RootBridgePlan;
use fstart_driver_amd::df::{RootBridge, plan, v4, wh as df_wh};
use fstart_driver_amd::dxio::rs::mailbox::{DOORBELL_V2, Encoding, Mailbox, Poll};
use fstart_driver_amd::fch::{FchHw, Native, qilian};
use fstart_driver_amd::nbio::{self, ioapic::nbio_ioapic_id, iohc, wh as nbio_wh};
use fstart_driver_amd::psp::{self, C2pTransport};
use fstart_driver_amd::smn::Smn;
use fstart_pci::{ConfigRegionAccess, PciAddress, PciEcam, ecam::GlobalEcam};
use heapless::Vec;
use tock_registers::interfaces::Readable;

/// POST `code` and log the step (generic so non-SP7 boards do not link strings).
fn step<V: Variant>(code: u8, name: &str) {
    fstart_driver_amd::fch::post_native(code);
    fstart_log::info!("{}: [{:#x}] {}", V::NAME, code, name);
}

/// The handles of the SP7 flow.
pub struct Hw {
    pub fch: Native<Tsc>,
    pub clock: Tsc,
    pub revision: cpu::Revision,
}

/// Per-flow state carried outside [`State`]: the validated APOB V2 in DRAM.
pub struct FlowState {
    pub apob: ApobV2<'static>,
}

/// Per-thread init policy for WH CPUs.
#[derive(Clone, Copy)]
pub struct WhThreads {
    microcode: Option<ccx::microcode::Patch<'static>>,
}

fn wh_thread_init(policy: &WhThreads, msr: &NativeMsr) -> bool {
    if let Some(patch) = &policy.microcode {
        let load = ccx::microcode::Load::default();
        // SAFETY: ring 0, caching on; patch lives in the verified identity-mapped FFS.
        if unsafe { patch.apply(msr, patch.address(), CpuIdentity::current().signature, load) }
            .is_err()
        {
            return false;
        }
    }
    true
}

/// What the silicon steps hand on.
pub struct State {
    dies: Vec<u8, 2>,
    roots: Vec<RootBridge, 8>,
    plan: plan::Plan,
    bars: [Bars; 8],
    psp_bar: u32,
    psp: Option<psp::boot::Psp>,
}

/// FCH IOAPIC ID.
const FCH_IOAPIC_ID: u8 = 0;

/// WH NBIO IOAPIC ID for physical root bridge `rb`: `0xF1 + rb`.
const fn ioapic_id(rb: u8) -> u8 {
    nbio_ioapic_id(fstart_driver_amd::cpu::Silicon::Gn, rb)
}

/// Probe live local (socket 0) DF IODs (`00:18.0`, `00:19.0`) via PCI Vendor ID `0x1022`.
fn probe_local_dies() -> Vec<u8, 2> {
    let mut dies = Vec::new();
    for die in 0u8..2 {
        if let Some(addr) = df_wh::node_address(0, die, 0) {
            // SAFETY: PCI bus 0 ECAM is active; reading Vendor/Device ID is side-effect free.
            let id = unsafe { GlobalEcam.read(addr, 0) };
            if (id & 0xffff) == 0x1022 {
                let _ = dies.push(die);
            }
        }
    }
    dies
}

/// One-shot guards enforcing exclusive mutable access to static boot buffers.
static PSP_STORAGE_CLAIMED: AtomicBool = AtomicBool::new(false);
static mut PSP_STORAGE: psp::Storage = psp::Storage::new();

static ACPI_TABLES_CLAIMED: AtomicBool = AtomicBool::new(false);
const ACPI_TABLE_CAPACITY: usize = 0x8_0000;
#[repr(C, align(4096))]
struct AcpiBuffer([u8; ACPI_TABLE_CAPACITY]);
static mut ACPI_TABLES: AcpiBuffer = AcpiBuffer([0; ACPI_TABLE_CAPACITY]);

static SMBIOS_TABLES_CLAIMED: AtomicBool = AtomicBool::new(false);
#[repr(C, align(4096))]
struct SmbiosBuffer([u8; fstart_acpi::smbios::BUFFER_SIZE]);
static mut SMBIOS_TABLES: SmbiosBuffer = SmbiosBuffer([0; fstart_acpi::smbios::BUFFER_SIZE]);

/// Open the PSP mailbox at `bar` at most once per boot (generic over `V` so
/// error strings stay scoped to SP7 boards).
fn psp_session<V: Variant>(bar: u32) -> Result<psp::boot::Psp, &'static str> {
    if PSP_STORAGE_CLAIMED.swap(true, Ordering::AcqRel) {
        return Err("PSP re-entry");
    }
    // SAFETY: mapped below 4 GiB by `init_fch`; BSP owns the normal-mode mailbox.
    let mailbox = psp::Mailbox::new(
        unsafe { psp::MmioTransport::new(bar.into()) },
        psp::Variant::Gn,
    );
    // SAFETY: guarded by `PSP_STORAGE_CLAIMED`, exclusively borrowed once per boot.
    let storage = unsafe { &mut *core::ptr::addr_of_mut!(PSP_STORAGE) };
    psp::Session::new(mailbox, storage).map_err(|_| {
        fstart_log::error!("{}: PSP session failed", V::NAME);
        "PSP"
    })
}

/// SMM report shared between the SMM handler and normal mode.
static mut SMM_REPORT: crate::rs::smm::Report = crate::rs::smm::Report::NONE;

impl<V: Variant> Init for Sp7<V> {
    const NAME: &'static str = V::NAME;

    type Config = Config;
    type Identity = cpu::Revision;
    type Hw = Hw;
    /// Static board PCIe lane table.
    type Lanes<'a> = &'a [LaneDescriptor];
    type Runtime = ();
    type FchUp = State;
    type Trained = State;
    type Chipset = State;
    type Cpu = Fam17hCpu<Self, WhThreads>;
    type State = State;

    fn enter(_: &Config, msr: &NativeMsr) -> Entered<Self> {
        fstart_driver_amd::fch::post_native(0x10);
        ccx::msr::enable_ecam(msr, ECAM_BASE, ECAM_BUSES);
        fstart_pci::ecam::init(ECAM_BASE as usize);
        // SAFETY: exclusive FCH programming before console.
        unsafe { fstart_driver_amd::fch::acpimmio::enable_acpimmio() };
        let clock = Tsc::measured(msr);
        // SAFETY: BSP only, ACPI MMIO and ECAM decoded.
        let fch = unsafe { Native::new(Tsc::native(msr)) };
        qilian::take_over(&fch);
        let signature = CpuIdentity::current().signature;
        let revision = cpu::Revision::from_signature(signature);
        let fallback = cpu::Revision {
            signature,
            model: cpu::MODEL_MIN,
            stepping: 0,
        };
        Entered {
            hw: Hw {
                fch,
                clock: clock.unwrap_or_else(|| Tsc::native(msr)),
                revision: revision.unwrap_or(fallback),
            },
            clock,
            pre_init: revision.map(drop).ok_or("WH CPU revision"),
        }
    }

    fn identify(hw: &Hw, _: &Config, _: Option<Tsc>) -> Result<cpu::Revision, &'static str> {
        fstart_log::info!(
            "{}: WH model {:#x} stepping {:#x} ({:#x}), TSC {} MHz",
            V::NAME,
            hw.revision.model,
            hw.revision.stepping,
            hw.revision.signature,
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

    fn psp(_: &Hw) -> psp::Variant {
        psp::Variant::Gn
    }

    fn init_fch(ctx: &Ctx<'_, Self>) -> Result<State, &'static str> {
        let config = ctx.config;
        let fch = &ctx.hw.fch;
        step::<V>(0x20, "DFv4.5");
        let dies = probe_local_dies();
        if !dies.contains(&0) {
            return Err("DF node0 absent");
        }
        // SAFETY: BSP owns DF indirect registers on node (0, 0) during cold init.
        let node0 = unsafe { df_wh::Node::new(GlobalEcam, 0, 0) }.ok_or("DF node0")?;
        if node0
            .df()
            .reg(v4::SYSTEM_CONFIG)
            .is_set(v4::SystemConfig::OTHER_SOCKET_PRESENT)
        {
            return Err("multi-socket SP7 not yet supported");
        }
        let routes = node0.bus_routes().map_err(|_| "DFv4.5 bus maps")?;
        let roots = resolve_wh_roots(&routes, &dies)?;
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

        // Broadcast MMIO and I/O routing rules to EVERY live local IOD (`0x18`, `0x19`).
        for &die in &dies {
            // SAFETY: BSP owns DF routing tables on live local IOD `die`.
            let node = unsafe { df_wh::Node::new(GlobalEcam, 0, die) }.ok_or("DF node")?;
            v4::program(node.df(), &plan.mmio, &plan.io);
        }

        step::<V>(0x30, "WH NBIO (IOAPIC, TOM, PSP BAR) & FCH");
        let dram_tops = iohc::DramTops {
            tom: tops.0,
            tom2: tops.1,
        };
        let mut bars = [Bars::default(); 8];
        let mut psp_bar: Option<u32> = None;
        for rb in &plan.root_bridges {
            let idx = usize::from(rb.index);
            if idx >= bars.len() {
                return Err("NBIO index out of range");
            }
            let (rb_bars, maybe_psp) =
                place_bars(rb.index, rb.below_pools.non_pci).ok_or("NBIO BAR placement")?;
            bars[idx] = rb_bars;
            let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
            // SAFETY: root bridge bus is routed; BSP owns SMN during cold init.
            let nb = unsafe { nbio_wh::RootBridge::new(GlobalEcam, rb_on_iod, rb.bus.base) }
                .ok_or("WH NBIO handle")?;
            // Program NB TOP_OF_DRAM on every root bridge's IOHC.
            // SAFETY: D0F0x90 of this root bridge's host bridge.
            let slot1 = unsafe {
                fstart_pci::ConfigRegister::new(
                    GlobalEcam,
                    PciAddress::new(0, nb.bus(), 0, 0),
                    iohc::NB_TOP_OF_DRAM_SLOT1,
                )
            };
            iohc::set_tom(&nb, &slot1, dram_tops, iohc::TopStyle::Tom3);

            if let Some(ioapic_base) = rb_bars.ioapic {
                let id = ioapic_id(rb.index);
                nb.init_ioapic(ioapic_base, id);
                // SAFETY: IOAPIC MMIO just enabled on this root bridge.
                let ioapic = unsafe { IoApic::new(ioapic_base as usize) };
                ioapic.set_id(id);
                ioapic.mask_all();
            }
            nb.init_ioapic_routes();
            // Program the PSP BAR on die 0 RB 0's IOHC and verify C2P responds.
            if let Some(pbar) = maybe_psp {
                iohc::map_psp(&nb, pbar).map_err(|_| "PSP BAR")?;
                psp_bar = Some(pbar);
            }
            fstart_log::info!(
                "{}: RB{} (iod {} rb {}) bus {:#x}-{:#x} mmio {:#x}+{:#x} non_pci {:#x}+{:#x}",
                V::NAME,
                rb.index,
                rb.index / nbio_wh::ROOT_BRIDGES_PER_IOD,
                rb_on_iod,
                rb.bus.base,
                rb.bus.limit,
                rb.below.base,
                rb.below.size,
                rb.below_pools.non_pci.base,
                rb.below_pools.non_pci.size
            );
        }

        let psp_bar = psp_bar.ok_or("PSP BAR")?;
        // SAFETY: PSP BAR just programmed on IOHC and identity-mapped uncached.
        let c2p =
            unsafe { psp::MmioTransport::new(psp_bar.into()) }.read(psp::mailbox::C2pReg::Cmd);
        fstart_log::info!("{}: PSP BAR {:#x}, C2P {:#x}", V::NAME, psp_bar, c2p);
        if c2p == u32::MAX {
            return Err("PSP BAR");
        }
        let psp = psp_session::<V>(psp_bar)?;

        // FCH IOAPIC + HPET + board GPIO pads.
        fstart_driver_amd::fch::acpimmio::enable_hpet_ioapic(fch.acpi());
        // SAFETY: fixed FCH IOAPIC MMIO at 0xFEC0_0000, BSP only.
        let fch_ioapic = unsafe { IoApic::new(fstart_driver_amd::fch::IOAPIC as usize) };
        fch_ioapic.set_id(FCH_IOAPIC_ID);
        fch_ioapic.mask_all();
        let bsp_apic = (fstart_arch::x86::cpuid(1).1 >> 24) as u8;
        fch_ioapic.route_i8259(bsp_apic);

        fstart_driver_amd::fch::gpio::configure_all(fch.acpi(), config.gpio);
        fstart_driver_amd::fch::acpimmio::acpi_ports(
            fch.acpi(),
            super::acpi::soc(V::PMBASE, false).ports,
        );

        Ok(State {
            dies,
            roots,
            plan,
            bars,
            psp_bar,
            psp: Some(psp),
        })
    }

    fn train_dxio(
        ctx: &Ctx<'_, Self>,
        state: State,
        lanes: &[LaneDescriptor],
    ) -> Result<State, &'static str> {
        step::<V>(0x38, "WH PERST & MPIO v2 idle probe");
        // Release board PCIe slot PERST# pads via Qilian FCH GPIO.
        if !qilian::release_perst(&ctx.hw.fch, ctx.config.mpio.perst) {
            return Err("PERST release");
        }
        // Probe WH MPIO mailbox v2 (`DOORBELL_V2 = 0x0C91_0954`) readiness on bus 0
        // (no ASK/DELI lane-training payload is sent in this bring-up step).
        // SAFETY: BSP owns SMN on bus 0 during cold init.
        let smn = unsafe { Smn::on_bus(GlobalEcam, 0) };
        let poll = Poll {
            reads: const { NonZeroU32::new(1000).unwrap() },
            interval_us: const { NonZeroU32::new(100).unwrap() },
        };
        // SAFETY: BSP owns MPIO mailbox on bus 0.
        let mut mpio = unsafe { Mailbox::new_with_doorbell(poll, Encoding::LowByte, DOORBELL_V2) };
        match mpio.wait_idle(&smn, &ctx.clock, const { NonZeroU32::new(100).unwrap() }) {
            Ok(()) => fstart_log::info!(
                "{}: MPIO v2 mailbox idle ({} board lane descriptors; training deferred)",
                V::NAME,
                lanes.len()
            ),
            Err(_) => fstart_log::warn!(
                "{}: MPIO v2 mailbox busy/unavailable during early bring-up; continuing with PSP-trained links",
                V::NAME
            ),
        }
        Ok(state)
    }

    fn init_chipset(_ctx: &Ctx<'_, Self>, state: State, (): ()) -> Result<State, &'static str> {
        step::<V>(0x3a, "Chipset (no-op)");
        Ok(state)
    }

    fn cpu_driver(
        _ctx: &Ctx<'_, Self>,
        _state: &mut State,
        microcode: Option<&'static [u8]>,
    ) -> Result<Self::Cpu, &'static str> {
        step::<V>(0x50, "BSP microcode (BSP-only MP)");
        let signature = CpuIdentity::current().signature;
        let patch = microcode.and_then(|bytes| {
            ccx::microcode::Bundle::<Self>::parse(bytes)
                .ok()?
                .select(signature)
                .ok()
        });
        match &patch {
            Some(p) => fstart_log::info!("{}: microcode patch {:#x}", V::NAME, p.revision()),
            None => fstart_log::warn!("{}: no microcode patch for this CPU", V::NAME),
        }
        Ok(Fam17hCpu::multi_socket(
            V::NAME,
            WhThreads { microcode: patch },
            wh_thread_init,
        ))
    }

    fn threads(_: &Ctx<'_, Self>, _: &State) -> Result<u16, &'static str> {
        // Early bring-up defaults safely to BSP-only until APOB V2 3/3 dynamic
        // CCD/core enumeration and WH ThreadEnable AP launch are validated on hardware.
        Ok(1)
    }

    fn release_aps(_: &Ctx<'_, Self>, _: &State, aps: u16) -> Result<(), u32> {
        if aps == 0 {
            return Ok(());
        }
        Err(0)
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

    fn fabric<'a>(hw_ctx: &Ctx<'_, Self>, state: &'a State) -> Fabric<'a> {
        Fabric {
            segment: Self::pci_segment(hw_ctx.hw),
            bridges: &state.plan.root_bridges,
        }
    }

    fn route_legacy_vga(_: &Ctx<'_, Self>, state: &State, root: &RootBridgePlan) {
        // Broadcast VGA routing to every live local DF node (`0x18`, `0x19`).
        for &die in &state.dies {
            // SAFETY: BSP owns DF VGA routing register on live local IOD `die`.
            if let Some(node) = unsafe { df_wh::Node::new(GlobalEcam, 0, die) } {
                node.route_legacy_vga(root.fabric_id);
            }
        }
    }

    fn after_resources(_: &Ctx<'_, Self>, _: &mut State) -> Result<(), &'static str> {
        Ok(())
    }

    fn ready_to_boot(_: &Ctx<'_, Self>, _: &mut State, _: &PciEcam) -> Result<(), &'static str> {
        Ok(())
    }

    fn finalize(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        qilian::acpi_mode(&ctx.hw.fch, &super::acpi::soc(V::PMBASE, false).ports);
        let psp = state.psp.as_mut().ok_or("PSP session")?;
        psp::boot::lock(psp, &ctx.clock).map_err(|(what, error)| {
            let (why, value) = error.parts();
            fstart_log::error!("{}: PSP {}: {} {:#x}", V::NAME, what, why, value);
            "PSP lock"
        })?;
        Ok(())
    }

    fn boot_done(ctx: &Ctx<'_, Self>, state: &mut State) -> Result<(), &'static str> {
        let psp = state.psp.as_mut().ok_or("PSP session")?;
        // SAFETY: APs are parked and SMM is locked.
        unsafe { psp::boot::ready_to_boot(psp, &ctx.clock, ctx.board.discrete_tpm) }.map_err(
            |(what, error)| {
                let (why, value) = error.parts();
                fstart_log::error!("{}: PSP {}: {} {:#x}", V::NAME, what, why, value);
                "PSP BootDone"
            },
        )?;
        Ok(())
    }
}

impl<V: Variant> crate::flow::Soc for Sp7<V> {
    const TSEG_SIZE: u32 = TSEG_SIZE;
    type Flow = FlowState;

    fn map(
        _: &mut Hw,
        _: &mut Config,
        _: &crate::board::Facts,
        msr: &NativeMsr,
        _: Tsc,
    ) -> Result<Mapped<Self>, &'static str> {
        use crate::memory::{self, Reservation, TopOfMemory};
        let (tom, tom2) = ccx::msr::memory_limits(msr).map_err(|_| "TOP_MEM")?;
        // SAFETY: PSP writes APOB V2 at `V::APOB_BASE` before releasing the BSP;
        // the validated total size is reserved below.
        let apob = unsafe { ApobV2::from_address(V::APOB_BASE, V::APOB_MAX_SIZE as usize) }
            .map_err(|_| "APOB V2")?;
        let apob_len = (apob.total_size() as u64).next_multiple_of(0x1000);
        let apob_region = Region::new(V::APOB_BASE, apob_len);
        let map = apob.system_memory_map().map_err(|_| "APOB V2 sysmap")?;
        let regions = sysmap::memory_map(psp::Variant::Gn, map.top, map.holes().iter().copied())
            .map_err(|_| "memory map")?;
        let tseg = memory::place_tseg(
            &regions,
            tom,
            u64::from(TSEG_SIZE),
            &[V::LAYOUT.early, apob_region],
        )
        .ok_or("TSEG placement")?;
        let reservations = [
            Region::new(0, 0x1000),
            Region::new(fstart_arch::x86::mp::AP_BOOTSTRAP_ADDRESS, 0x1000),
            Region::new(0x9_f000, 0x6_1000),
            apob_region,
            V::LAYOUT.early,
            tseg,
        ]
        .map(Reservation::reserved);
        let e820 = memory::build(V::NAME, &regions, TopOfMemory { tom, tom2 }, &reservations)
            .map_err(|_| "memory map")?;
        if let Err(step) = crate::memory::cache_ram(&regions) {
            crate::flow::fail::<Self>(step);
        }
        let ffs = V::LAYOUT.ffs.ok_or("boot media")?;
        fstart_arch::x86::boot::enable_boot_media_rom_cache();
        crate::media::mount(e820.entries(), &V::LAYOUT, ffs).map_err(|error| error.step())?;
        Ok(Mapped {
            top_mem: tom,
            top_mem2: tom2,
            apob: None,
            tseg: tseg.base,
            e820,
            ffs: ffs.base..ffs.end(),
            nvram: None,
            options: fstart_options::Report::new(),
            flow: FlowState { apob },
        })
    }

    fn install_smm(
        ctx: &Ctx<'_, Self>,
        state: &mut State,
        mp: &MpHandle,
        _: &FlowState,
    ) -> Result<(), &'static str> {
        use crate::rs::smm::{Config as SmmConfig, Smi};
        use fstart_driver_amd::fch::smi;
        step::<V>(0x58, "SMM");
        if state.psp_bar == 0 {
            return Err("PSP BAR");
        }
        let report = &raw mut SMM_REPORT;
        let fch = &ctx.hw.fch;
        let smi = Smi {
            fch,
            config: SmmConfig {
                ports: super::acpi::soc(V::PMBASE, false).ports,
                psp_bar: state.psp_bar,
                einj: ctx.config.einj,
                report: report as u64,
            },
        };
        crate::tseg::install(V::NAME, mp, &smi, ctx.memory.tseg, TSEG_SIZE)?;
        smi::software_smi(fch.io(), crate::smm::sw::PSP_SMM_INFO);
        let (smm_info, _) = crate::smm::status::answers(
            &ctx.clock,
            false,
            || unsafe { fstart_core::mmio::read32(&raw const (*report).smm_info) },
            || unsafe { fstart_core::mmio::read32(&raw const (*report).einj) },
        );
        // Require PSP SmmInfo before DF lock / OS handoff.
        crate::smm::status::smm_info(V::NAME, smm_info)?;
        Ok(())
    }

    fn tables(
        ctx: &Ctx<'_, Self>,
        silicon: &mut Up<Self>,
        pci: &PciEcam,
        flow: &FlowState,
        identity: &crate::acpi::SmbiosIdentity<'_>,
        board: crate::acpi::Board,
    ) -> Tables {
        step::<V>(0x70, "ACPI, SMBIOS");
        let soc = super::acpi::soc(V::PMBASE, ctx.config.legacy_devices);
        let rsdp = build_acpi::<V>(ctx, silicon, pci, &soc, &board);
        let smbios = build_smbios::<V>(ctx, silicon, pci, flow, identity);
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

fn build_acpi<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    silicon: &Up<Sp7<V>>,
    pci: &PciEcam,
    soc: &crate::acpi::SocAcpi,
    board: &crate::acpi::Board,
) -> Option<u64> {
    if ACPI_TABLES_CLAIMED.swap(true, Ordering::AcqRel) {
        return None;
    }

    let state = &silicon.state;
    // Match enumerated PCI roots to DF root bridges by `bus_base`, never blind `.zip()`.
    let mut root_bridges: alloc::vec::Vec<RootBridgeAcpi<'_>> = alloc::vec::Vec::new();
    for root in pci.roots() {
        let bus_base = root.info().bus_start;
        let Some(rb) = state.roots.iter().find(|rb| rb.bus_base == bus_base) else {
            continue;
        };
        let bars = state.bars[usize::from(rb.index)];
        let rb_on_iod = rb.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
        // Validate IOAPIC ID and redirection pin count before publishing MADT entry.
        let ioapic = bars.ioapic.and_then(|base| {
            let expected_id = ioapic_id(rb.index);
            // SAFETY: IOAPIC MMIO mapped by `init_fch` and identity-mapped uncached.
            let dev = unsafe { IoApic::new(base as usize) };
            let pins = dev.redirection_entries();
            if expected_id != u8::MAX && dev.id() == expected_id && (1..=256).contains(&pins) {
                Some(Ioapic {
                    id: expected_id,
                    base,
                })
            } else {
                fstart_log::warn!(
                    "{}: skipping invalid IOAPIC on RB{} (id {:#x}, pins {})",
                    V::NAME,
                    rb.index,
                    dev.id(),
                    pins
                );
                None
            }
        });
        root_bridges.push(RootBridgeAcpi {
            root,
            fch: rb.primary,
            domain: None,
            ioapic,
            ioapic_features: nbio::ioapic::IOAPIC_FEATURES_ENABLE.address()
                + (u32::from(rb_on_iod) * nbio_wh::RB_SMN_STRIDE),
            iommu: None,
        });
    }

    // Read active WH IOHC DevFn assignments from the primary root bridge so
    // `_PRT` reflects programmed bridge addresses instead of assuming RS defaults.
    let active_bridges = state
        .roots
        .iter()
        .find(|rb| rb.primary)
        .and_then(|primary| {
            let rb_on_iod = primary.index % nbio_wh::ROOT_BRIDGES_PER_IOD;
            // SAFETY: primary root bridge bus is routed and BSP owns SMN.
            let nb = unsafe { nbio_wh::RootBridge::new(GlobalEcam, rb_on_iod, primary.bus_base) }?;
            let devfns = core::array::from_fn(|slot| nb.bridge_devfn(slot));
            let all_zero = devfns.iter().all(|&(d, f)| d == 0 && f == 0);
            (!all_zero).then(|| super::acpi::bridges_from_devfns(devfns))
        });
    static mut ACTIVE_BRIDGES: [crate::acpi::prt::Bridge; super::acpi::BRIDGES.len()] =
        super::acpi::BRIDGES;
    let mut effective_soc = *soc;
    if let Some(bridges) = active_bridges {
        // SAFETY: guarded by `ACPI_TABLES_CLAIMED`, written at most once before
        // immutable borrow for the remainder of `build_acpi`.
        unsafe {
            let dst = core::ptr::addr_of_mut!(ACTIVE_BRIDGES);
            (*dst) = bridges;
            effective_soc.bridges = &*dst;
        }
    }

    let cpus: alloc::vec::Vec<Cpu> = silicon
        .cpus
        .apic_ids
        .iter()
        .map(|&apic_id| Cpu {
            apic_id,
            domain: 0,
            aml: alloc::vec::Vec::new(),
        })
        .collect();

    // SAFETY: BSP reading architectural C-state MSRs.
    let msr = unsafe { NativeMsr::new() };
    let fch = &ctx.hw.fch;
    let mut sb_aml = fstart_driver_amd::fch::aml::FchAcpi::read(fch.mmio(), fch.io())
        .sb_aml()
        .unwrap_or_default();
    sb_aml.extend_from_slice(&silicon.aml);

    // SAFETY: guarded by `ACPI_TABLES_CLAIMED`, exclusively borrowed once per boot.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(ACPI_TABLES)).0 };
    crate::acpi::install(
        &crate::acpi::Soc {
            soc: &effective_soc,
            pci: <Sp7<V> as Init>::pci_segment(ctx.hw),
            cpus: &cpus,
            fch_ioapic_id: FCH_IOAPIC_ID,
            root_bridges: &root_bridges,
            numa: None,
            cstates: ccx::power::cstate_info(&msr),
            threads_per_core: ccx::Thread::current().threads_per_core(),
            devices: &silicon.devices,
            ivrs_devices: &[],
            sb_aml: &sb_aml,
            tables: &silicon.tables,
            abl: &[],
        },
        None,
        ctx.board,
        board,
        storage,
    )
    .inspect_err(|_| fstart_log::error!("{}: ACPI tables do not fit", V::NAME))
    .ok()
}

fn build_smbios<V: Variant>(
    ctx: &Ctx<'_, Sp7<V>>,
    silicon: &Up<Sp7<V>>,
    pci: &PciEcam,
    flow: &FlowState,
    identity: &crate::acpi::SmbiosIdentity<'_>,
) -> Option<u64> {
    if SMBIOS_TABLES_CLAIMED.swap(true, Ordering::AcqRel) {
        return None;
    }

    let regions = flow
        .apob
        .system_memory_map()
        .ok()
        .and_then(|map| {
            sysmap::memory_map(psp::Variant::Gn, map.top, map.holes().iter().copied()).ok()
        })
        .map(|regions| dram_ranges(&regions))
        .unwrap_or_default();
    let smt = ccx::Thread::current().threads_per_core() > 1;
    // SAFETY: guarded by `SMBIOS_TABLES_CLAIMED`, exclusively borrowed once per boot.
    let storage = unsafe { &mut (*core::ptr::addr_of_mut!(SMBIOS_TABLES)).0 };
    let address = storage.as_ptr() as u64;
    crate::smbios::build_with(
        storage,
        &crate::smbios::System {
            soc: &crate::smbios::SocSmbios {
                // SMBIOS 3.x has no dedicated code for SP7 yet.
                socket_upgrade: 0x02,
                max_dimm_mib: ctx.config.max_dimm_mib,
                channels_per_die: 16,
            },
            identity,
            rom_size: V::ROM_SIZE,
            apob: None,
            regions: &regions,
            cpus: &silicon.cpus,
            pci,
            bridges: &super::acpi::BRIDGES,
            ports: &[],
        },
        |apic_id| (0, apic_id >> u32::from(smt)),
        || (alloc::vec::Vec::new(), false),
    )
    .inspect_err(|what| fstart_log::error!("{}: SMBIOS: {}", V::NAME, what))
    .ok()
    .map(|_| address)
}
