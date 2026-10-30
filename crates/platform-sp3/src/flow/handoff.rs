//! The end of the flow: ACPI tables, the boot media (FFS) and the payload
//! (Linux or CrabEFI, as `fstart_payload` selects), with the firmware's
//! e820 and RSDP (INTEGRATION_NOTES §10).

extern crate alloc;

use alloc::vec::Vec;

use fstart_core::services::memory_detect::{E820Entry, E820Kind, E820State};
use fstart_driver_uart::ns16550::Ns16550;
use fstart_stage::boot::{MemoryPolicy, MemoryWindow};
use fstart_stage::payload::MainstagePayload;

use super::{Ctx, OrHalt};
use crate::layout::{self, Span};

#[cfg(any(
    fstart_payload = "halt",
    fstart_payload = "linux",
    fstart_payload = "crabefi"
))]
type Payload = fstart_stage::payload::BuildSelectedPayload;
#[cfg(not(any(
    fstart_payload = "halt",
    fstart_payload = "linux",
    fstart_payload = "crabefi"
)))]
type Payload = fstart_stage::payload::HaltPayload;

/// Publish the load policy (RAM from e820 minus the running image) and
/// mount + authenticate the FFS at `ctx.ffs` (ROM2 or ROM3, where the flash anchor
/// says): the locator and trust block the composer put into the reset
/// image (`fstart_anchor_bytes`) describe it.
pub(crate) fn mount_boot_media(ctx: &Ctx) {
    let window = ctx.ffs;
    let (writable, reserved): (Vec<MemoryWindow>, Vec<MemoryWindow>) = {
        let to_window = |e: &E820Entry| MemoryWindow {
            start: e.addr,
            size: e.size,
        };
        let ram = |e: &&E820Entry| e.kind == E820Kind::Ram as u32;
        // The running image (text, in-place data: `layout::RESET`) and its
        // BSS, heap and stacks (`layout::EARLY_REGION`) are reserved by
        // span: `running_stage_windows` assumes data and BSS are
        // contiguous, which this layout does not keep.
        let image = MemoryWindow {
            start: layout::RESET.destination,
            size: u64::from(layout::RESET.size),
        };
        (
            ctx.e820
                .entries()
                .iter()
                .filter(ram)
                .map(to_window)
                .collect(),
            ctx.e820
                .entries()
                .iter()
                .filter(|e| !ram(e))
                .map(to_window)
                .chain([
                    image,
                    span_window(layout::EARLY_REGION),
                    span_window(window),
                ])
                .collect(),
        )
    };
    let policy = MemoryPolicy {
        writable: &writable,
        reserved: &reserved,
        entry_alignment: 1,
    };
    // SAFETY: platform RAM only, with the stage, early region and flash
    // excluded; no device has a DMA buffer outside the early region.
    unsafe { fstart_stage::directory::set_load_policy(&policy) }.or_halt("load policy");
    let locator = fstart_stage::anchor::media_locator().or_halt("FFS locator");
    if !locator.validate(window.size) {
        super::fail("FFS locator");
    }
    let ffs = fstart_stage::fixed_helpers::MemoryMappedFfs::new(
        window.base,
        usize::try_from(locator.image_size).or_halt("FFS size"),
    );
    ffs.mount().or_halt("FFS mount");
    ffs.verify().or_halt("FFS verify");
}

fn span_window(span: Span) -> MemoryWindow {
    MemoryWindow {
        start: span.base,
        size: span.size,
    }
}

/// What the payload launcher reads.
pub(crate) struct Handoff<'a> {
    e820: E820State,
    rsdp: Option<u64>,
    smbios: Option<u64>,
    console: &'a Ns16550,
    ecam: (u64, u64),
    ffs: Span,
}

impl fstart_stage::payload::X86UefiPayloadContext for Handoff<'_> {
    fn console(&self) -> Option<&dyn fstart_core::services::Console> {
        Some(self.console)
    }

    fn e820(&self) -> &[E820Entry] {
        self.e820.entries()
    }

    fn firmware_region(&self) -> (u64, u64) {
        (self.ffs.base, self.ffs.size)
    }

    fn acpi_rsdp(&self) -> Option<u64> {
        self.rsdp
    }

    fn smbios(&self) -> Option<u64> {
        self.smbios
    }

    fn pci_root(&self) -> Option<fstart_pci::PciRootInfo> {
        Some(fstart_pci::PciRootInfo {
            segment: 0,
            ecam_base: self.ecam.0,
            bus_start: 0,
            bus_end: ((self.ecam.1 >> 20) - 1) as u8,
        })
    }
}

#[cfg(feature = "payload-linux")]
impl fstart_stage::payload::X86LinuxPayloadContext for Handoff<'_> {
    fn e820(&self) -> &[E820Entry] {
        self.e820.entries()
    }

    fn acpi_rsdp(&self) -> Option<u64> {
        self.rsdp
    }
}

/// Hand the e820, RSDP and console to the build's payload launcher.
pub(crate) fn boot(ctx: Ctx, roots: Vec<fstart_pci::PciEcam>, console: &Ns16550) -> ! {
    // The allocators' assignments are in the hardware now.
    drop(roots);
    fstart_log::info!("sp3: handing off to the payload");
    fstart_log::flush();
    Payload::boot(Handoff {
        e820: ctx.e820,
        rsdp: ctx.rsdp,
        smbios: ctx.smbios,
        console,
        ecam: ctx.ecam,
        ffs: ctx.ffs,
    })
}

/// Build and place the ACPI tables (ACPI_SMM, `crate::acpi`); returns the
/// RSDP address. The tables live in the stage heap, inside the reserved
/// early region.
#[cfg(feature = "acpi")]
pub(crate) fn acpi<B: crate::board::Board, S: super::Steps>(ctx: &Ctx, si: &S) -> u64 {
    use crate::acpi::{self, Cpu, Inputs, IommuAcpi, Power, RootBridgeAcpi, Window};
    use fstart_core::acpi::AcpiSmiConfig;
    use fstart_driver_amd::nbio::ioapic::IOAPIC_FEATURES_ENABLE;
    use fstart_driver_amd::sp3::ccx::power;
    use fstart_driver_amd::sp3::fch::smi;
    use fstart_driver_amd::sp3::nbio::ioapic::{IoapicInfo, fch_ioapic_id};

    let numa_plan = S::numa(ctx, si);
    let cpus: Vec<Cpu> = ctx
        .apic_ids
        .iter()
        .enumerate()
        .map(|(i, &apic_id)| Cpu {
            apic_id,
            domain: numa_plan
                .as_ref()
                .and_then(|n| n.cpus.get(i).copied())
                .unwrap_or(0),
        })
        .collect();
    let ioapics: Vec<IoapicInfo> = si.nbios().iter().filter_map(|n| n.ioapic_info()).collect();
    let iommus: Vec<IommuAcpi> = si
        .nbios()
        .iter()
        .zip(&ctx.topology.root_bridges)
        .filter_map(|(n, rb)| {
            n.iommu_info().map(|info| IommuAcpi {
                info,
                registers: iommu_registers(&info),
                bus_limit: rb.bus_limit,
            })
        })
        .collect();
    type Windows = (Vec<Window<u16>>, Vec<Window<u64>>);
    let windows: Vec<Windows> = ctx
        .plan
        .root_bridges
        .iter()
        .map(|rb| {
            let io = rb
                .io
                .iter()
                .map(|w| Window {
                    base: w.base as u16,
                    limit: (w.end() - 1) as u16,
                })
                .collect();
            let mmio = [Some(rb.below), rb.second_below, rb.above]
                .into_iter()
                .flatten()
                .filter(|w| w.size != 0)
                .map(|w| Window {
                    base: w.base,
                    limit: w.end() - 1,
                })
                .collect();
            (io, mmio)
        })
        .collect();
    let root_bridges: Vec<RootBridgeAcpi<'_>> = ctx
        .topology
        .root_bridges
        .iter()
        .zip(&windows)
        .zip(si.nbios())
        .enumerate()
        .map(|(i, ((&bridge, (io, mmio)), n))| RootBridgeAcpi {
            bridge,
            io,
            mmio,
            domain: numa_plan
                .as_ref()
                .and_then(|p| p.root_bridges.get(i).copied()),
            ioapic_features: n.at(IOAPIC_FEATURES_ENABLE.address()),
        })
        .collect();
    let cpu = crate::hw::msr();
    let pstates = power::pstates(&cpu);
    let threads_per_core = 1 + crate::mp::this_thread(ctx.generation).smt() as u8;
    let (domains, memory): (Vec<acpi::Domain>, Vec<acpi::MemoryAffinity>) = numa_plan
        .as_ref()
        .map(|p| {
            (
                p.domains
                    .iter()
                    .map(|&socket| acpi::Domain { socket })
                    .collect(),
                p.memory
                    .iter()
                    .map(|&(base, end, domain)| acpi::MemoryAffinity { base, end, domain })
                    .collect(),
            )
        })
        .unwrap_or_default();
    if let Some(p) = &numa_plan {
        fstart_log::info!(
            "acpi: {} NUMA domains, {} memory ranges",
            p.domains.len() as u32,
            p.memory.len() as u32
        );
    }
    let lpc_aml = B::acpi_lpc_aml();
    let board_acpi = B::acpi_board(&lpc_aml);
    let fch = {
        use fstart_driver_amd::sp3::fch::FchHw;
        acpi::FchAcpi::read(ctx.fch.mmio(), ctx.fch.io())
    };
    // SAFETY: the HPET general capabilities register, read only.
    let hpet_block_id = unsafe { fstart_core::mmio::read32(0xfed0_0000 as *const u32) };
    let cppc: Vec<_> = ctx
        .cppc
        .iter()
        .map(|c| acpi::CppcAcpi {
            info: c.info,
            base: c.base,
            smu_bar: c.smu_bar,
            lowest_mhz: c.lowest_mhz,
            nominal_mhz: c.nominal_mhz,
            slots: &c.slots,
        })
        .collect();
    let inputs = Inputs {
        generation: ctx.generation,
        cpus: &cpus,
        fch_ioapic_id: fch_ioapic_id(ctx.generation),
        ioapics: &ioapics,
        hpet_block_id,
        root_bridges: &root_bridges,
        iommus: &iommus,
        numa: numa_plan.as_ref().map(|_| acpi::Numa {
            domains: &domains,
            memory: &memory,
            phys_addr_bits: ctx.phys_addr_bits,
        }),
        power: Some(Power {
            pstates: &pstates,
            cstates: power::cstate_info(&ctx.policy.ccx.cstates),
            threads_per_core,
            c2_latency_us: power::c2_latency_us(fstart_arch::x86::mp::CpuIdentity::current()),
        }),
        acpi_smi: ctx.smm.map(|_| AcpiSmiConfig {
            smi_cmd: u32::from(fstart_driver_amd::sp3::fch::acpimmio::SMI_CMD as u16),
            acpi_enable: smi::sw::ACPI_ENABLE,
            acpi_disable: smi::sw::ACPI_DISABLE,
        }),
        einj: ctx.smm.is_some() && {
            // The initialization SMI enabled the PSP backend and checked
            // the live Rome/ECC map. Only the boot flow reads this flag;
            // SMM authorization lives in SMRAM, not in this shared page.
            unsafe {
                ((crate::einj::BASE + crate::einj::SUPPORTED) as *const u64).read_volatile() == 8
            }
        },
        fch: &fch,
        tpm: super::tpm::acpi(),
        bmc_sx_notify: B::BMC_SX_NOTIFY,
        cppc: &cppc,
        board: board_acpi,
    };
    if inputs.einj {
        let targets = ctx.smm.map(|s| s.einj.targets).unwrap_or([0; 2]);
        let first = targets
            .iter()
            .copied()
            .find(|&t| t != 0)
            .unwrap_or(crate::einj::SCRATCH);
        let trigger = acpi::einj::trigger(first);
        // SAFETY: the fixed communication page is in reserved early RAM,
        // disjoint from the heap and scratch page; no concurrent OS yet.
        unsafe {
            core::ptr::copy_nonoverlapping(
                trigger.as_ptr(),
                (crate::einj::BASE + crate::einj::TRIGGER) as *mut u8,
                trigger.len(),
            );
        }
        for (socket, &address) in targets.iter().enumerate().filter(|(_, t)| **t != 0) {
            fstart_log::info!(
                "einj: socket {} correctable scratch {:#x}",
                socket as u32,
                address
            );
        }
    }
    let tables = acpi::build(&inputs).or_halt("ACPI tables");
    const ACPI_SIZE: usize = 0x4_0000;
    let bytes = super::dma_alloc(ACPI_SIZE, 0x1000);
    let base = bytes.as_ptr() as u64;
    let len = tables.assemble_into(bytes, base).or_halt("ACPI layout");
    fstart_log::info!("acpi: {} bytes at {:#x}", len as u32, base);
    base
}

/// The IOMMU capability and MMIO registers IVRS copies (ACPI_SMM §3.6).
#[cfg(feature = "acpi")]
fn iommu_registers(
    info: &fstart_driver_amd::sp3::nbio::iommu::IommuInfo,
) -> fstart_acpi::ivrs::IommuRegisters {
    use fstart_pci::{ConfigRegionAccess, PciAddress};
    let pci = crate::hw::Pci::default();
    let function = PciAddress::new(0, info.bus, info.devfn >> 3, info.devfn & 7);
    let cap = u16::from(info.cap_offset);
    // SAFETY: the IOMMU function and its enabled MMIO block, read only.
    let cfg = |off: u16| unsafe { pci.read(function, cap + off) };
    let mmio = |off: u64| unsafe {
        let p = (info.base + off) as *const u32;
        u64::from(fstart_core::mmio::read32(p))
            | u64::from(fstart_core::mmio::read32(p.add(1))) << 32
    };
    fstart_acpi::ivrs::IommuRegisters {
        capability: cfg(0),
        range: cfg(0xc),
        misc: cfg(0x10),
        control: mmio(0x18),
        efr: mmio(0x30),
        efr2: mmio(0x1a0),
        counter_config: mmio(0x4000),
    }
}
