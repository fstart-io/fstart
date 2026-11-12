//! The ACPI tables of the flow, from what the silicon steps left
//! (INTEGRATION_NOTES §10).

extern crate alloc;

use alloc::vec::Vec;

use super::{Ctx, OrHalt};

/// Build and place the ACPI tables (ACPI_SMM, `crate::sp3::acpi`); returns the
/// RSDP address. The tables live in the stage heap, inside the reserved
/// early region.
#[cfg(feature = "acpi")]
pub(crate) fn acpi<S: fstart_driver_amd::sp3::soc::Steps>(
    ctx: &Ctx<'_>,
    si: &S,
    pci: &fstart_pci::PciEcam,
    board_acpi: &crate::sp3::acpi::Board<'_>,
    bmc_sx_notify: Option<crate::sp3::smm::SleepNotify>,
) -> u64 {
    use crate::sp3::acpi::{self, Cpu, Inputs, IommuAcpi, Power, RootBridgeAcpi};
    use fstart_core::acpi::AcpiSmiConfig;
    use fstart_driver_amd::nbio::ioapic::IOAPIC_FEATURES_ENABLE;
    use fstart_driver_amd::sp3::ccx::power;
    use fstart_driver_amd::sp3::fch::smi;
    use fstart_driver_amd::sp3::nbio::ioapic::{IoapicInfo, fch_ioapic_id};

    let numa_plan = S::numa(&ctx.soc, si);
    let cpus: Vec<Cpu> = ctx
        .soc
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
        .zip(&ctx.soc.topology.root_bridges)
        .filter_map(|(n, rb)| {
            n.iommu_info().map(|info| IommuAcpi {
                info,
                registers: iommu_registers(&info),
                bus_limit: rb.bus_limit,
            })
        })
        .collect();
    let root_bridges: Vec<RootBridgeAcpi<'_>> = ctx
        .soc
        .topology
        .root_bridges
        .iter()
        // The root bridges the allocator was built from.
        .zip(pci.roots())
        .zip(si.nbios())
        .enumerate()
        .map(|(i, ((&bridge, root), n))| RootBridgeAcpi {
            bridge,
            root,
            domain: numa_plan
                .as_ref()
                .and_then(|p| p.root_bridges.get(i).copied()),
            ioapic_features: n.at(IOAPIC_FEATURES_ENABLE.address()),
        })
        .collect();
    let cpu = fstart_driver_amd::sp3::soc::hw::msr();
    let pstates = power::pstates(&cpu);
    let threads_per_core =
        1 + fstart_driver_amd::sp3::soc::cpu::this_thread(ctx.soc.generation).smt() as u8;
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
    let fch = {
        use fstart_driver_amd::sp3::fch::FchHw;
        acpi::FchAcpi::read(ctx.soc.fch.mmio(), ctx.soc.fch.io())
    };
    // SAFETY: the HPET general capabilities register, read only.
    let hpet_block_id = unsafe { fstart_core::mmio::read32(0xfed0_0000 as *const u32) };
    // Naples has no CPPC through the SMU.
    #[cfg(any(feature = "rome", feature = "milan"))]
    let cppc: Vec<_> = ctx
        .soc
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
    #[cfg(not(any(feature = "rome", feature = "milan")))]
    let cppc: Vec<acpi::CppcAcpi> = Vec::new();
    let inputs = Inputs {
        generation: ctx.soc.generation,
        pci: ctx.soc.pci_root,
        cpus: &cpus,
        fch_ioapic_id: fch_ioapic_id(ctx.soc.generation),
        ioapics: &ioapics,
        hpet_block_id,
        root_bridges: &root_bridges,
        iommus: &iommus,
        numa: numa_plan.as_ref().map(|_| acpi::Numa {
            domains: &domains,
            memory: &memory,
            phys_addr_bits: ctx.soc.phys_addr_bits,
        }),
        power: Some(Power {
            pstates: &pstates,
            cstates: power::cstate_info(&ctx.soc.policy.ccx.cstates),
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
                ((crate::sp3::einj::BASE + crate::sp3::einj::SUPPORTED) as *const u64)
                    .read_volatile()
                    == 8
            }
        },
        fch: &fch,
        tpm: super::tpm::acpi(),
        bmc_sx_notify,
        cppc: &cppc,
        board: *board_acpi,
    };
    if inputs.einj {
        let targets = ctx.smm.map(|s| s.einj.targets).unwrap_or([0; 2]);
        let first = targets
            .iter()
            .copied()
            .find(|&t| t != 0)
            .unwrap_or(crate::sp3::einj::SCRATCH);
        let trigger = acpi::einj::trigger(first);
        // SAFETY: the fixed communication page is in reserved early RAM,
        // disjoint from the heap and scratch page; no concurrent OS yet.
        unsafe {
            core::ptr::copy_nonoverlapping(
                trigger.as_ptr(),
                (crate::sp3::einj::BASE + crate::sp3::einj::TRIGGER) as *mut u8,
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
    let bytes =
        fstart_driver_amd::sp3::soc::steps::dma_alloc(ACPI_SIZE, 0x1000).or_halt("DMA allocation");
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
    let pci = fstart_driver_amd::sp3::soc::hw::Pci::default();
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
