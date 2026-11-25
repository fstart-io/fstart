// SPDX-License-Identifier: MIT OR Apache-2.0
//! A 2P B0 system (2 sockets × 2 IODs × 3 IOS: 12 root bridges, 1024
//! threads) through what the flow does with its fabric: the ABL's bus map
//! resolved to root bridges, the DF plan, the PCI roots and their
//! allocator, then the ACPI tables (DSDT, MADT, SRAT, IVRS, ...) with the
//! shared `_CPC`, laid out in the SP7 table storage.
extern crate std;

use std::vec::Vec;

use fstart_driver_amd::ccx::cppc::{Integers, cpc_entries};
use fstart_driver_amd::df::{BusRange, plan, wh as df_wh};
use fstart_driver_amd::nbio::wh as nbio_wh;
use fstart_driver_amd::smu::CppcInfo;
use fstart_pci::{PciEcam, PciRootInfo};

use super::{IOAPIC_ID_BASE, fabric};
use crate::acpi::{
    Board, Cpu, Domain, Inputs, Ioapic, MemoryAffinity, Numa, RootBridgeAcpi, apei, build,
};

/// The 2P B0 bus map: 12 root bridges of 21 buses, not in fabric ID order.
fn routes() -> Vec<df_wh::BusRoute> {
    let fids = [
        0x20u16, 0x21, 0x22, 0x60, 0x61, 0x62, 0xa0, 0xa1, 0xa2, 0xe0, 0xe1, 0xe2,
    ];
    let mut routes: Vec<_> = (fids.iter().enumerate())
        .map(|(i, &destination)| df_wh::BusRoute {
            segment: 0,
            buses: BusRange {
                base: i as u8 * 21,
                limit: i as u8 * 21 + 20,
            },
            destination,
        })
        .collect();
    routes.reverse();
    routes
}

fn plan() -> fabric::Plan {
    let ios: Vec<df_wh::Ios> = (0..2)
        .flat_map(|die| fabric::assumed_ios(&df_wh::devices::B0, die))
        .collect();
    let nodes = [(0, 0), (0, 1), (1, 0), (1, 1)];
    let roots = fabric::resolve_roots(&routes(), &ios, &nodes).unwrap();
    plan::plan_for(
        &plan::Input {
            tom: 0x8000_0000,
            tom2: 0x100_0000_0000,
            ecam_base: super::ECAM_BASE,
            ecam_size: u64::from(super::ECAM_BUSES) << 20,
            phys_addr_bits: 52,
            root_bridges: &roots,
            mmio_pairs: df_wh::MMIO_PAIRS,
        },
        &plan::Policy::df3(),
    )
    .unwrap()
}

/// 1024 threads, socket in APIC ID bit 9, as `sp7::tables` orders them:
/// the first thread of every core first.
fn cpus(cpc: &[u8]) -> Vec<Cpu> {
    let mut cpus: Vec<Cpu> = (0..1024u32)
        .map(|i| Cpu {
            apic_id: i,
            domain: (i >> 9) * 4 + (i >> 7 & 3),
            aml: cpc.to_vec(),
        })
        .collect();
    cpus.sort_unstable_by_key(|cpu| (cpu.apic_id & 1, cpu.apic_id));
    cpus
}

#[test]
fn two_sockets_of_b0_fit() {
    let plan = plan();
    assert_eq!(plan.root_bridges.len(), 12);
    let segment = PciRootInfo {
        segment: 0,
        ecam_base: super::ECAM_BASE,
        bus_start: 0,
        bus_end: (super::ECAM_BUSES - 1) as u8,
    };
    let fabric = crate::soc::Fabric {
        segment,
        bridges: &plan.root_bridges,
    };
    let roots = fabric.roots().expect("12 PCI roots");
    assert_eq!(roots.len(), 12);
    let pci = PciEcam::new(roots).expect("one allocator for 12 root bridges");
    assert_eq!(pci.roots().len(), 12);

    // More root bridges than one allocator owns are refused, not cut
    // short or a panic.
    let mut many: Vec<_> = plan.root_bridges.to_vec();
    for i in 12..=fstart_pci::MAX_PCI_ROOTS {
        let mut rb = many[11];
        rb.bus = BusRange {
            base: 252,
            limit: 252,
        };
        rb.index = i as u8;
        many.push(rb);
    }
    let too_many = crate::soc::Fabric {
        segment,
        bridges: &many,
    };
    assert!(too_many.roots().is_none());

    // The tables, as `sp7::tables::acpi` fills them.
    let soc = super::acpi::soc(0x800, true);
    let info = CppcInfo::from_answer((0x1f_ffff & !(1 << 16)) << 2 | 3);
    let entries = cpc_entries(
        &info,
        &Integers {
            autonomous_selection: 0,
            lowest_mhz: 400,
            nominal_mhz: 2400,
        },
    );
    let cpc = super::acpi::shared_cpc(info.revision, &entries);
    assert!(!cpc.package.is_empty() && cpc.reference.len() < 32);
    let threads = cpus(&cpc.reference);
    let iommu = fstart_acpi::ivrs::IommuRegisters {
        capability: 1 << 27 | 0x0003_000f,
        range: 0,
        misc: 0,
        control: 0,
        efr: 1 << 2,
        efr2: 0,
        counter_config: 0,
    };
    let rbs: Vec<RootBridgeAcpi<'_>> = (pci.roots().iter())
        .zip(&plan.root_bridges)
        .enumerate()
        .map(|(i, (root, rb))| {
            let (_, phys) = fabric::rb_location(rb.index);
            RootBridgeAcpi {
                root,
                fch: rb.primary,
                domain: Some(u32::from(rb.socket) * 4 + u32::from(phys)),
                ioapic: Some(Ioapic {
                    id: nbio_wh::ioapic_id(IOAPIC_ID_BASE, i).unwrap(),
                    base: 0xc000_0000 + i as u64 * 0x10_0000,
                }),
                ioapic_features: 0,
                iommu: (phys < 2).then_some(crate::acpi::Iommu {
                    base: 0xd000_0000 + i as u64 * 0x10_0000,
                    registers: iommu,
                }),
                bridges: None,
                cxl: None,
            }
        })
        .collect();
    let domains: Vec<Domain> = (0..8).map(|d| Domain { socket: d / 4 }).collect();
    let memory: Vec<MemoryAffinity> = (0..8u64)
        .map(|d| MemoryAffinity {
            base: 0x100_0000_0000 + d * (1 << 40),
            end: 0x100_0000_0000 + (d + 1) * (1 << 40),
            domain: d as u32,
        })
        .collect();
    let board = Board::default();
    let einj = fstart_acpi::einj::table_revision(
        2,
        fstart_acpi::OEM_ID,
        fstart_acpi::OEM_TABLE_ID,
        fstart_acpi::OEM_REVISION,
        &fstart_driver_amd::psp::einj::entries_wh(0xfee0_0000, 0x7000_0000, true),
    );
    let wsmt = fstart_acpi::wsmt::build_wsmt(fstart_acpi::wsmt::FIXED_COMM_BUFFERS);
    let tables = [einj, wsmt];
    let sb_aml = cpc.package.clone();
    let inputs = Inputs {
        soc: &soc,
        pci: segment,
        cpus: &threads,
        fch_ioapic_id: super::FCH_IOAPIC_ID,
        hpet_block_id: 0x1022_8201,
        root_bridges: &rbs,
        numa: Some(Numa {
            domains: &domains,
            memory: &memory,
            phys_addr_bits: 52,
        }),
        power: Some(crate::acpi::tests::power()),
        acpi_smi: None,
        bmc_sx_notify: None,
        tpm: None,
        devices: &[],
        ivrs_devices: &[],
        sb_aml: &sb_aml,
        tables: &tables,
        apei: Some(apei::Apei {
            mcg_cap: 64,
            threshold: 0,
            boot_errors: (0x7f00_0000, 1024),
        }),
        board: &board,
    };
    let tables = build(&inputs).unwrap();
    let mut storage = std::vec![0u8; super::acpi::TABLES_SIZE];
    let length = tables
        .assemble_into(&mut storage, 0x7e00_0000)
        .expect("the 2P tables fit the SP7 storage");
    std::eprintln!(
        "2P B0, 1024 threads: {length:#x} of {:#x} bytes",
        super::acpi::TABLES_SIZE
    );
    // Room for what a board adds (SPCR, SSDTs, a video ROM's VFCT).
    assert!(
        length + (64 << 10) <= super::acpi::TABLES_SIZE,
        "{length:#x}"
    );

    // A `_CPC` package in every processor device would not fit.
    let copied = cpus(&fstart_acpi::processor::cpc_aml(info.revision, &entries));
    let per_cpu = build(&Inputs {
        cpus: &copied,
        sb_aml: &[],
        ..inputs
    })
    .unwrap();
    let mut big = std::vec![0u8; 2 * super::acpi::TABLES_SIZE];
    let copied_length = per_cpu.assemble_into(&mut big, 0x7e00_0000).unwrap();
    std::eprintln!("with a _CPC per thread: {copied_length:#x} bytes");
    assert!(copied_length > super::acpi::TABLES_SIZE);

    // Root bridges 10 and 11 are PC0A and PC0B; the others keep `PCIn`.
    let dsdt = &tables.dsdt;
    let has = |name: &[u8]| dsdt.windows(name.len()).any(|w| w == name);
    for name in [
        b"PCI0", b"PCI9", b"PC0A", b"PC0B", b"MBRB", b"CPCP", b"C3FF",
    ] {
        assert!(has(name), "{}", core::str::from_utf8(name).unwrap());
    }
    assert!(!has(b"PCI:") && !has(b"PCI;"));

    // MADT: xAPIC entries for APIC IDs below FFh, x2APIC for the rest, no
    // UID FFh; the FCH IOAPIC and 12 NBIO ones.
    let fstart_acpi::platform::PlatformConfig::X86(x86) = &tables.platform else {
        panic!("x86 tables")
    };
    let uids = fstart_acpi::platform::x86::processor_uids(&x86.apic_ids);
    assert!(!uids.contains(&0xff));
    for (&uid, &id) in uids.iter().zip(&x86.apic_ids) {
        assert_eq!(uid < 0xff, id < 0xff, "UID {uid:#x} APIC ID {id:#x}");
    }
    assert_eq!(x86.ioapics.len(), 13);

    if let Ok(dsl) = crate::acpi::tests::iasl_disassemble(&fstart_acpi::platform::build_dsdt(dsdt))
    {
        for object in [
            "Device (PC0A)",
            "Device (PC0B)",
            "Name (_UID, 0x0B)",
            "Method (_CPC, 0, NotSerialized)",
            "Return (\\_SB.CPCP)",
            "Name (\\_SB.CPCP, Package (0x17)",
        ] {
            assert!(dsl.contains(object), "{object} missing");
        }
    } else {
        std::eprintln!("iasl not found, skipping the DSDT disassembly");
    }
}
