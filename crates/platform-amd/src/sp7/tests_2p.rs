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
    let roots = fabric::resolve_roots(&routes(), &ios, &nodes, 1).unwrap();
    plan::plan_for(
        &plan::Input {
            tom: 0x8000_0000,
            tom2: 0x100_0000_0000,
            ecam_base: super::DEFAULT_ECAM.base,
            ecam_size: super::DEFAULT_ECAM.size(),
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
        ecam_base: super::DEFAULT_ECAM.base,
        bus_start: 0,
        bus_end: 0xff,
    };
    let fabric = crate::soc::Fabric {
        segment,
        segments: 1,
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
        segments: 1,
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
                    also: None,
                    base: 0xd000_0000 + i as u64 * 0x10_0000,
                    registers: iommu,
                }),
                bridges: None,
                cxl: None,
            }
        })
        .collect();
    let domains: Vec<Domain> = (0..8).map(|d| Domain::cpu(d / 4)).collect();
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
            firmware_first: None,
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

/// The table of `tables` with `signature`.
fn table<'a>(tables: &'a crate::acpi::Tables, signature: &[u8; 4]) -> &'a [u8] {
    tables
        .extra
        .iter()
        .find(|t| &t[..4] == signature)
        .unwrap_or_else(|| panic!("no {}", core::str::from_utf8(signature).unwrap()))
}

/// The IVHD blocks of an IVRS: `(type, segment, device entries)`.
fn ivhds(ivrs: &[u8]) -> Vec<(u8, u16, &[u8])> {
    let mut out = Vec::new();
    let mut at = 48;
    while at + 4 <= ivrs.len() {
        let kind = ivrs[at];
        let length = usize::from(u16::from_le_bytes([ivrs[at + 2], ivrs[at + 3]]));
        let header = if kind == 0x10 { 24 } else { 40 };
        let segment = u16::from_le_bytes([ivrs[at + 16], ivrs[at + 17]]);
        out.push((kind, segment, &ivrs[at + header..at + length]));
        at += length;
    }
    out
}

/// The IOAPIC handles of an IVHD's special entries (type `0x48`, variety 1).
fn ioapics(entries: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < entries.len() {
        let kind = entries[at];
        if kind == 0x48 && entries[at + 7] == 1 {
            out.push(entries[at + 4]);
        }
        at += match kind {
            0..0x40 => 4,
            0x40..0x80 => 8,
            _ => 8 + 16 + 2 + usize::from(entries[at + 21]),
        };
    }
    out
}

/// A Dell-like 2P B0 whose ABL used two PCI segments (APCB `DfPciMmioSize`
/// 512 MiB at `0x1000_0000_0000`: socket 0 on segment 0, socket 1 on
/// segment 1, 42 buses per root bridge), through the root bridges, the DF
/// plan (ECAM above 4 GiB), the PCI roots and their allocator, then MCFG,
/// DSDT (`_SEG`), IVRS (segments, the third root bridge of each IOD under
/// its IOD's first IOMMU), SRAT/SLIT (32 across sockets) and HMAT. The
/// same map squeezed into one segment by the bus planner resolves too.
#[test]
fn two_segments_of_b0() {
    use super::ccx::msr::EcamWindow;
    let window = EcamWindow::new(0x1000_0000_0000, 2).unwrap();
    let fids = [
        0x20u16, 0x21, 0x22, 0x60, 0x61, 0x62, 0xa0, 0xa1, 0xa2, 0xe0, 0xe1, 0xe2,
    ];
    let routes: Vec<df_wh::BusRoute> = (fids.iter().enumerate())
        .map(|(i, &destination)| df_wh::BusRoute {
            segment: (i / 6) as u8,
            buses: BusRange {
                base: (i % 6) as u8 * 42,
                limit: (i % 6) as u8 * 42 + 41,
            },
            destination,
        })
        .collect();
    let ios: Vec<df_wh::Ios> = (0..2)
        .flat_map(|die| fabric::assumed_ios(&df_wh::devices::B0, die))
        .collect();
    let nodes = [(0, 0), (0, 1), (1, 0), (1, 1)];

    // One segment decodes it only after the bus planner moved socket 1.
    assert_eq!(
        fabric::resolve_roots(&routes, &ios, &nodes, 1),
        Err(fabric::RouteError::Segment)
    );
    let (requests, fch) = fabric::bus_requests(&routes, &ios, &nodes, 3, None, None).unwrap();
    let squeezed = df_wh::buses::plan(&requests, fch, 256).unwrap().unwrap();
    let squeezed: Vec<df_wh::BusRoute> = (squeezed.iter())
        .map(|a| df_wh::BusRoute {
            segment: a.segment,
            buses: a.buses,
            destination: a.fabric_id,
        })
        .collect();
    let one = fabric::resolve_roots(&squeezed, &ios, &nodes, 1).unwrap();
    assert_eq!(one.len(), 12);
    assert!(one.iter().all(|rb| rb.segment == 0));
    // Two segments take the ABL's map as it is.
    assert_eq!(df_wh::buses::plan(&requests, fch, window.buses()), Ok(None));

    let roots = fabric::resolve_roots(&routes, &ios, &nodes, window.segments.into()).unwrap();
    assert_eq!(roots.iter().filter(|rb| rb.segment == 1).count(), 6);
    assert!(roots.iter().all(|rb| u16::from(rb.socket) == rb.segment));
    let plan: fabric::Plan = plan::plan_for(
        &plan::Input {
            tom: 0x8000_0000,
            tom2: 0x100_0000_0000,
            ecam_base: window.base,
            ecam_size: window.size(),
            phys_addr_bits: 52,
            root_bridges: &roots,
            mmio_pairs: df_wh::MMIO_PAIRS,
        },
        &plan::Policy::df3(),
    )
    .unwrap();
    // Nothing routed overlaps the ECAM; below 4 GiB is [TOM, 0xFEC0_0000).
    assert!(
        (plan.mmio.iter())
            .all(|r| r.window.end() <= window.base || r.window.base >= window.base + window.size())
    );
    assert!(
        plan.root_bridges
            .iter()
            .all(|rb| rb.below.base >= 0x8000_0000)
    );

    let segment = PciRootInfo {
        segment: 0,
        ecam_base: window.base,
        bus_start: 0,
        bus_end: 0xff,
    };
    let fabric = crate::soc::Fabric {
        segment,
        segments: window.segments.into(),
        bridges: &plan.root_bridges,
    };
    let pci = PciEcam::new(fabric.roots().unwrap()).expect("12 roots on two segments");
    for (root, rb) in pci.roots().iter().zip(&plan.root_bridges) {
        let info = root.info();
        assert_eq!(info.segment, rb.segment);
        assert_eq!(info.ecam_base, window.base + (u64::from(rb.segment) << 28));
        assert_eq!(info.bus_start, rb.bus.base);
    }
    // The primary owns the whole window.
    let primary = pci
        .roots()
        .iter()
        .position(|r| r.info().bus_start == 0)
        .unwrap();
    assert!(
        pci.roots()[primary]
            .motherboard()
            .iter()
            .any(|w| (w.base, w.size) == (window.base, window.size()))
    );
    // The payload gets both segments.
    let segments = crate::soc::segments(segment, window.segments.into());
    assert_eq!(segments.len(), 2);
    assert_eq!(segments[1].ecam_base, window.base + 0x1000_0000);

    let keys: Vec<(u8, u8)> = plan
        .root_bridges
        .iter()
        .map(|rb| (rb.socket, rb.index))
        .collect();
    let also = fabric::iommu_also(&keys, false);
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
                domain: Some(u32::from(rb.socket)),
                ioapic: Some(Ioapic {
                    id: nbio_wh::ioapic_id(IOAPIC_ID_BASE, i).unwrap(),
                    base: 0xc000_0000 + i as u64 * 0x10_0000,
                }),
                ioapic_features: 0,
                iommu: (phys < 2).then_some(crate::acpi::Iommu {
                    base: 0xd000_0000 + i as u64 * 0x10_0000,
                    registers: iommu,
                    also: also[i],
                }),
                bridges: None,
                cxl: None,
            }
        })
        .collect();
    let threads: Vec<Cpu> = (0..64u32)
        .map(|i| Cpu {
            apic_id: i,
            domain: i >> 5,
            aml: Vec::new(),
        })
        .collect();
    let domains = [Domain::cpu(0), Domain::cpu(1)];
    let memory = [
        MemoryAffinity {
            base: 0x100_0000_0000,
            end: 0x200_0000_0000,
            domain: 0,
        },
        MemoryAffinity {
            base: 0x200_0000_0000,
            end: 0x300_0000_0000,
            domain: 1,
        },
    ];
    let targets = [
        crate::acpi::hmat::dram(0, 0, 6400, 16),
        crate::acpi::hmat::dram(1, 1, 6400, 16),
    ];
    let hmat = crate::acpi::hmat::hmat(
        &[(0, 0), (1, 1)],
        &targets,
        Some(crate::acpi::hmat::half_xgmi([32_000; 4])),
    )
    .unwrap();
    let soc = super::acpi::soc(0x800, true);
    let board = Board::default();
    let extra = [hmat];
    let tables = build(&Inputs {
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
        power: None,
        acpi_smi: None,
        bmc_sx_notify: None,
        tpm: None,
        devices: &[],
        ivrs_devices: &[],
        sb_aml: &[],
        tables: &extra,
        apei: None,
        board: &board,
    })
    .unwrap();

    // MCFG: one allocation per segment, buses 0..=FF.
    let mcfg = table(&tables, b"MCFG");
    assert_eq!(mcfg.len(), 44 + 2 * 16);
    for s in 0..2u16 {
        let entry = &mcfg[44 + 16 * usize::from(s)..][..16];
        let base = u64::from_le_bytes(entry[..8].try_into().unwrap());
        assert_eq!(base, window.base + (u64::from(s) << 28));
        assert_eq!(u16::from_le_bytes([entry[8], entry[9]]), s);
        assert_eq!((entry[10], entry[11]), (0, 0xff));
    }

    // IVRS: socket 1's IOMMUs on segment 1; every NBIO IOAPIC (12, the
    // IOMMU-less third root bridges' through their IOD's first IOMMU) and
    // the FCH's exactly once, across the 10h and the 11h blocks alike.
    let ivrs = table(&tables, b"IVRS");
    for kind in [0x10, 0x11] {
        let blocks: Vec<_> = ivhds(ivrs).into_iter().filter(|b| b.0 == kind).collect();
        assert_eq!(blocks.len(), 8);
        assert_eq!(blocks.iter().filter(|b| b.1 == 1).count(), 4);
        let mut ids: Vec<u8> = blocks.iter().flat_map(|b| ioapics(b.2)).collect();
        ids.sort_unstable();
        let mut want: Vec<u8> = (0..12).map(|i| IOAPIC_ID_BASE + i).collect();
        want.insert(0, super::FCH_IOAPIC_ID);
        assert_eq!(ids, want, "IVHD {kind:#x}");
    }

    // SLIT: 32 between the sockets; HMAT passed through.
    let slit = table(&tables, b"SLIT");
    assert_eq!(&slit[44..48], &[10, 32, 32, 10]);
    assert_eq!(&table(&tables, b"HMAT")[..4], b"HMAT");
    assert_eq!(&table(&tables, b"SRAT")[..4], b"SRAT");

    // DSDT: socket 1's root bridges are on segment 1.
    if let Ok(dsl) =
        crate::acpi::tests::iasl_disassemble(&fstart_acpi::platform::build_dsdt(&tables.dsdt))
    {
        assert_eq!(dsl.matches("Name (_SEG, One)").count(), 6, "{dsl}");
        assert_eq!(dsl.matches("Name (_SEG, Zero)").count(), 6);
    } else {
        std::eprintln!("iasl not found, skipping the DSDT disassembly");
    }
}

/// 1P B0: the IOMMU of each IOD's first root bridge covers its third, the
/// one without an IOMMU, so every NBIO IOAPIC is in an IVHD (else Linux
/// turns interrupt remapping off, which x2APIC needs).
#[test]
fn one_socket_ivrs_covers_every_ioapic() {
    let plan = plan();
    let one: Vec<_> = (plan.root_bridges.iter())
        .filter(|rb| rb.socket == 0)
        .copied()
        .collect();
    let segment = PciRootInfo {
        segment: 0,
        ecam_base: super::DEFAULT_ECAM.base,
        bus_start: 0,
        bus_end: 0xff,
    };
    let fabric = crate::soc::Fabric {
        segment,
        segments: 1,
        bridges: &one,
    };
    let pci = PciEcam::new(fabric.roots().unwrap()).unwrap();
    let keys: Vec<(u8, u8)> = one.iter().map(|rb| (rb.socket, rb.index)).collect();
    let also = fabric::iommu_also(&keys, false);
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
        .zip(&one)
        .enumerate()
        .map(|(i, (root, rb))| RootBridgeAcpi {
            root,
            fch: rb.primary,
            domain: None,
            ioapic: Some(Ioapic {
                id: nbio_wh::ioapic_id(IOAPIC_ID_BASE, i).unwrap(),
                base: 0xc000_0000 + i as u64 * 0x10_0000,
            }),
            ioapic_features: 0,
            iommu: (fabric::rb_location(rb.index).1 < 2).then_some(crate::acpi::Iommu {
                base: 0xd000_0000 + i as u64 * 0x10_0000,
                registers: iommu,
                also: also[i],
            }),
            bridges: None,
            cxl: None,
        })
        .collect();
    let soc = super::acpi::soc(0x800, true);
    let board = Board::default();
    let threads = [Cpu {
        apic_id: 0,
        domain: 0,
        aml: Vec::new(),
    }];
    let tables = build(&Inputs {
        soc: &soc,
        pci: segment,
        cpus: &threads,
        fch_ioapic_id: super::FCH_IOAPIC_ID,
        hpet_block_id: 0x1022_8201,
        root_bridges: &rbs,
        numa: None,
        power: None,
        acpi_smi: None,
        bmc_sx_notify: None,
        tpm: None,
        devices: &[],
        ivrs_devices: &[],
        sb_aml: &[],
        tables: &[],
        apei: None,
        board: &board,
    })
    .unwrap();
    let ivrs = table(&tables, b"IVRS");
    let blocks: Vec<_> = ivhds(ivrs).into_iter().filter(|b| b.0 == 0x11).collect();
    assert_eq!(blocks.len(), 4);
    let mut ids: Vec<u8> = blocks.iter().flat_map(|b| ioapics(b.2)).collect();
    ids.sort_unstable();
    assert_eq!(ids, [0x80, 0xf0, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5]);
}
