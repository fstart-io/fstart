//! Whole-set tests of the SP3 generations on the common builder
//! ([`crate::acpi`]): every table assembles with valid checksums, the
//! per-generation FADT/MADT values, NUMA gating, x2APIC, CPPC, the DSDT
//! through `iasl` when it is installed, and the table sets byte for byte.

use std::vec::Vec;

use crate::sp3::silicon::Generation;
use fstart_acpi::devices::IpmiKcsAcpi;
use fstart_acpi::ivrs::IommuRegisters;
use fstart_core::acpi::AcpiSmiConfig;
use fstart_core::description::PciRootInfo;
use fstart_driver_amd::ccx::power::{CstateInfo, Pstate};
use fstart_driver_amd::smu::v11::CppcInfo;
use fstart_driver_amd::sp3::nbio::ioapic::{fch_ioapic_id, nbio_ioapic_id};
use fstart_driver_amd::sp3::topology::RootBridge;

use super::cppc::{self, Channel};
use super::{FchAcpi, einj, fadt_flags, of};
use crate::acpi::{
    Board, Cpu, Domain, Inputs, Ioapic, Iommu, MemoryAffinity, Numa, Power, RootBridgeAcpi, Tables,
    build, numa,
};

const TABLE_ADDR: u64 = 0x7F00_0000;

/// A 1P (or 2P) S8030-like system: 4 NBIOs per socket, bus 0 on NBIO3
/// (the FCH NBIO, IOAPIC 0xF4 → GSI 120 as on the donor), as the flow
/// fills the inputs from it (`flow::handoff`).
struct System {
    generation: Generation,
    cpus: Vec<Cpu>,
    bridges: Vec<RootBridge>,
    /// The FCH's devices.
    sb_aml: Vec<u8>,
    /// \[EINJ\], PCCT.
    tables: Vec<Vec<u8>>,
    board: Board,
}

/// The root bridge of `bridge` as the allocator had it: the primary with
/// the legacy I/O, each with one window below and one above 4 GiB.
fn pci_root(bridge: &RootBridge) -> &'static fstart_pci::PciRoot {
    use fstart_pci::PciRole::{Forwarded, Own, Window};
    use fstart_pci::PciRoot;
    // The primary forwards the legacy ports and VGA memory too and owns
    // the ECAM window.
    let (io, vga, ecam) = if bridge.primary {
        (0, 0x2_0000, 256 << 20)
    } else {
        (0x1000, 0, 0)
    };
    let root = PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base: 0xE000_0000,
            bus_start: bridge.bus_base,
            bus_end: bridge.bus_limit,
        },
        [
            Forwarded.io(io, 0x2000 - io),
            Window.io(0x1000, 0x1000),
            Forwarded.mmio(0xa_0000, vga),
            Window.mmio(0xC000_0000, 0x0800_0000),
            Window.mmio(0x100_0000_0000, 0x10_0000_0000),
            Own.mmio(0xE000_0000, ecam),
        ],
    )
    .unwrap();
    Box::leak(Box::new(root))
}

/// One PCC channel serving every thread in `apic_ids`.
fn channel(apic_ids: &[u32]) -> Channel<'static> {
    Channel {
        info: CppcInfo::from_answer(0x007F_FFFF),
        base: 0x7FF0_0000,
        smu_bar: 0xF110_0000,
        lowest_mhz: 1500,
        nominal_mhz: 2500,
        slots: Box::leak(apic_ids.to_vec().into_boxed_slice()),
    }
}

fn cpus(apic_ids: &[u32], channels: &[Channel]) -> Vec<Cpu> {
    apic_ids
        .iter()
        .map(|&apic_id| Cpu {
            apic_id,
            domain: 0,
            aml: cppc::cpc(channels, apic_id).unwrap(),
        })
        .collect()
}

fn system(generation: Generation, sockets: u8, threads: u32) -> System {
    let bridges: Vec<RootBridge> = (0..sockets)
        .flat_map(|s| {
            (0..4u8).map(move |i| RootBridge {
                socket: s,
                index: i,
                fabric_id: 0,
                // NBIO3 of socket 0 owns bus 0, as on the S8030.
                bus_base: (128 * s) + 0x20 * ((i + 1) % 4),
                bus_limit: (128 * s) + 0x20 * ((i + 1) % 4) + 0x1F,
                primary: s == 0 && i == 3,
            })
        })
        .collect();
    let apic_ids: Vec<u32> = (0..threads).collect();
    let channels = [channel(&apic_ids)];
    System {
        generation,
        cpus: cpus(&apic_ids, &channels),
        bridges,
        sb_aml: fch().sb_aml().unwrap(),
        tables: cppc::pcct(&channels).into_iter().collect(),
        board: Board {
            ipmi: Some(IpmiKcsAcpi {
                port: 0xCA2,
                spacing: 1,
                uid: 0,
                spec_revision: 0x0200,
            }),
            i8042: true,
            ..Board::default()
        },
    }
}

fn root_bridges(s: &System) -> Vec<RootBridgeAcpi<'static>> {
    s.bridges
        .iter()
        .map(|bridge| {
            let n = 4 * bridge.socket + bridge.index;
            RootBridgeAcpi {
                root: pci_root(bridge),
                fch: bridge.primary,
                domain: None,
                ioapic: Some(Ioapic {
                    id: nbio_ioapic_id(s.generation.silicon(), n),
                    base: 0xA000_0000 + (u64::from(n) << 20),
                }),
                ioapic_features: 0x1430_0000 + (u32::from(bridge.index) << 20),
                iommu: Some(Iommu {
                    base: 0xB000_0000 + (u64::from(n) << 19),
                    registers: IommuRegisters {
                        capability: 1 << 27,
                        ..Default::default()
                    },
                }),
            }
        })
        .collect()
}

const PSTATES: [Pstate; 2] = [
    Pstate {
        index: 0,
        mhz: 3000,
        microvolts: 1_100_000,
        milliwatts: 9000,
    },
    Pstate {
        index: 1,
        mhz: 2200,
        microvolts: 900_000,
        milliwatts: 5000,
    },
];

/// A Rome FCH with UART0, UART1 as COM1, I2C0 and GPIO (routes FCH_SP3
/// §3.3).
fn fch() -> FchAcpi {
    use fstart_driver_amd::sp3::fch::acpi::{I2c, LegacyCom, Uart};
    let mut f = FchAcpi {
        gpio_irq: Some(7),
        ..FchAcpi::default()
    };
    f.uarts.push(Uart { index: 0, irq: 3 }).unwrap();
    f.legacy
        .push(LegacyCom {
            com: 1,
            port: 0x3F8,
            irq: 4,
        })
        .unwrap();
    f.i2c.push(I2c { index: 0, irq: 10 }).unwrap();
    f
}

fn inputs<'a>(
    s: &'a System,
    bridges: &'a [RootBridgeAcpi<'a>],
    numa: Option<Numa<'a>>,
) -> Inputs<'a> {
    let generation = s.generation;
    Inputs {
        soc: of(generation),
        pci: crate::sp3::silicon::hw::segment(generation),
        cpus: &s.cpus,
        fch_ioapic_id: fch_ioapic_id(generation.silicon()),
        hpet_block_id: 0x1022_8201,
        root_bridges: bridges,
        numa,
        power: Some(Power {
            pstates: &PSTATES,
            cstates: Some(CstateInfo {
                io_base: 0x813,
                cc6: true,
            }),
            threads_per_core: 2,
            c2_latency_us: if generation == Generation::Naples {
                400
            } else {
                18
            },
        }),
        acpi_smi: Some(AcpiSmiConfig {
            smi_cmd: 0xB2,
            acpi_enable: 0xA0,
            acpi_disable: 0xA1,
        }),
        bmc_sx_notify: Some(fstart_driver_ipmi::SleepNotify {
            kcs_port: 0xca2,
            kcs_spacing: 1,
            sw_smi_base: 0xC0,
        }),
        tpm: Some(fstart_acpi::devices::tpm::TpmTisAcpi {
            family: fstart_acpi::devices::tpm::TpmFamily::Tpm20,
            server: true,
            log: Some(fstart_acpi::devices::tpm::LogArea {
                base: 0x7560_0000,
                size: 0x1_0000,
            }),
        }),
        devices: &[],
        sb_aml: &s.sb_aml,
        tables: &s.tables,
        board: &s.board,
    }
}

/// Assemble and return each table by signature (RSDP excluded).
fn assemble(tables: &Tables) -> Vec<([u8; 4], Vec<u8>)> {
    let mut buf = std::vec![0u8; 1 << 20];
    let len = tables.assemble_into(&mut buf, TABLE_ADDR).unwrap();
    let buf = &buf[..len];
    assert_eq!(&buf[..8], b"RSD PTR ");
    // Walk the XSDT plus the DSDT/FACS it reaches through the FADT.
    let at = |addr: u64| &buf[(addr - TABLE_ADDR) as usize..];
    let table = |addr: u64| {
        let t = at(addr);
        let n = u32::from_le_bytes(t[4..8].try_into().unwrap()) as usize;
        (t[..4].try_into().unwrap(), t[..n].to_vec())
    };
    let xsdt_addr = u64::from_le_bytes(buf[24..32].try_into().unwrap());
    let (_, xsdt) = table(xsdt_addr);
    let mut out: Vec<([u8; 4], Vec<u8>)> = xsdt[36..]
        .chunks(8)
        .map(|c| table(u64::from_le_bytes(c.try_into().unwrap())))
        .collect();
    let fadt = out.iter().find(|(s, _)| s == b"FACP").unwrap().1.clone();
    out.push(table(u64::from_le_bytes(
        fadt[140..148].try_into().unwrap(),
    )));
    out.push((*b"XSDT", xsdt));
    out
}

fn get<'a>(t: &'a [([u8; 4], Vec<u8>)], sig: &[u8; 4]) -> Option<&'a [u8]> {
    t.iter().find(|(s, _)| s == sig).map(|(_, b)| b.as_slice())
}

fn madt_entries(madt: &[u8]) -> Vec<(u8, &[u8])> {
    let mut rest = &madt[44..];
    let mut v = Vec::new();
    while let [kind, len, ..] = *rest {
        let (e, tail) = rest.split_at(usize::from(len));
        v.push((kind, e));
        rest = tail;
    }
    v
}

#[test]
fn every_generation_assembles_with_valid_checksums() {
    for g in [Generation::Naples, Generation::Rome, Generation::Milan] {
        let s = system(g, 1, 64);
        let rb = root_bridges(&s);
        let t = assemble(&build(&inputs(&s, &rb, None)).unwrap());
        for (sig, bytes) in &t {
            assert_eq!(
                bytes.iter().fold(0u8, |a, &b| a.wrapping_add(b)),
                0,
                "{}",
                std::str::from_utf8(sig).unwrap()
            );
        }
        let sigs: Vec<_> = t.iter().map(|(s, _)| *s).collect();
        for sig in [
            b"FACP", b"APIC", b"HPET", b"MCFG", b"IVRS", b"SPMI", b"DSDT",
        ] {
            assert!(sigs.contains(sig), "{g:?} {sig:?}");
        }
        assert!(!sigs.contains(b"SRAT") && !sigs.contains(b"SLIT"));
    }
}

#[test]
fn fadt_values_per_generation() {
    for (g, flags) in [
        (Generation::Naples, 0x305AD),
        (Generation::Rome, 0x305AD),
        (Generation::Milan, 0x2052D),
    ] {
        let s = system(g, 1, 16);
        let rb = root_bridges(&s);
        let t = assemble(&build(&inputs(&s, &rb, None)).unwrap());
        let f = get(&t, b"FACP").unwrap();
        let u32_at = |o: usize| u32::from_le_bytes(f[o..o + 4].try_into().unwrap());
        assert_eq!(f[45], 4, "Enterprise Server");
        assert_eq!(u16::from_le_bytes(f[46..48].try_into().unwrap()), 9);
        assert_eq!(u32_at(48), 0xB2);
        assert_eq!((f[52], f[53]), (0xA0, 0xA1));
        assert_eq!(
            (u32_at(56), u32_at(64), u32_at(76), u32_at(80)),
            (0x800, 0x804, 0x808, 0x820)
        );
        assert_eq!(f[92], 8, "GPE0 length");
        assert_eq!(u32_at(112), flags, "{g:?}");
        assert_eq!(u16::from_le_bytes(f[109..111].try_into().unwrap()), 3);
    }
}

#[test]
fn madt_ioapics_sorted_and_cumulative() {
    let s = system(Generation::Rome, 2, 128);
    let rb = root_bridges(&s);
    let t = assemble(&build(&inputs(&s, &rb, None)).unwrap());
    let io: Vec<(u8, u32)> = madt_entries(get(&t, b"APIC").unwrap())
        .into_iter()
        .filter(|(k, _)| *k == 1)
        .map(|(_, e)| (e[2], u32::from_le_bytes(e[8..12].try_into().unwrap())))
        .collect();
    let want: Vec<(u8, u32)> = core::iter::once((0xF0, 0))
        .chain((0..8u8).map(|n| (0xF1 + n, 24 + 32 * u32::from(n))))
        .collect();
    assert_eq!(io, want);
}

#[test]
fn x2apic_entries_for_256_threads() {
    let s = system(Generation::Milan, 2, 256);
    let rb = root_bridges(&s);
    let t = assemble(&build(&inputs(&s, &rb, None)).unwrap());
    let e = madt_entries(get(&t, b"APIC").unwrap());
    assert_eq!(e.iter().filter(|(k, _)| *k == 0).count(), 255);
    let x2: Vec<_> = e.iter().filter(|(k, _)| *k == 9).collect();
    assert_eq!(x2.len(), 1);
    assert_eq!(&x2[0].1[4..8], &0xFFu32.to_le_bytes());
    assert_eq!(&x2[0].1[12..16], &255u32.to_le_bytes(), "UID");
    assert_eq!(e.iter().filter(|(k, _)| *k == 0xA).count(), 1);
    let dsdt = get(&t, b"DSDT").unwrap();
    assert!(dsdt.windows(4).any(|w| w == b"C0FF"));
}

#[test]
fn srat_slit_only_with_several_domains() {
    let g = Generation::Rome;
    let mut s = system(g, 2, 64);
    s.cpus.iter_mut().for_each(|c| c.domain = c.apic_id >> 7);
    let domains = [Domain { socket: 0 }, Domain { socket: 1 }];
    let memory = [
        MemoryAffinity {
            base: 0,
            end: 0x8000_0000,
            domain: 0,
        },
        MemoryAffinity {
            base: 0x1_0000_0000,
            end: 0x10_0000_0000,
            domain: 1,
        },
    ];
    let numa = Numa {
        domains: &domains,
        memory: &memory,
        phys_addr_bits: 48,
    };
    let rb = root_bridges(&s);
    let t = assemble(&build(&inputs(&s, &rb, Some(numa))).unwrap());
    assert!(get(&t, b"SRAT").is_some() && get(&t, b"MSCT").is_some());
    assert_eq!(&get(&t, b"SLIT").unwrap()[44..], &[10, 32, 32, 10]);

    let one = Numa {
        domains: &domains[..1],
        ..numa
    };
    let t = assemble(&build(&inputs(&s, &rb, Some(one))).unwrap());
    assert!(get(&t, b"SRAT").is_none() && get(&t, b"SLIT").is_none());
}

#[test]
fn dsdt_root_bridges_in_bus_order() {
    let s = system(Generation::Milan, 1, 8);
    let rb = root_bridges(&s);
    let tables = build(&inputs(&s, &rb, None)).unwrap();
    let d = &tables.dsdt;
    for name in [
        b"PCI0", b"PCI1", b"PCI2", b"PCI3", b"LPCB", b"SYSR", b"NAPE", b"GPIO", b"FUR0", b"COM1",
        b"I2CA", b"_L0B", b"_PRW", b"XHC0", b"_CPC", b"_OSC", b"_PTS", b"_WAK", b"BSMC", b"TPM_",
    ] {
        assert!(d.windows(4).any(|w| w == name));
    }
    // PCI0 = bus 0 = NBIO3, whose IOAPIC (0xF4) has GSI 120: 3.1 INTA → 150.
    // The last occurrences are the devices (the first ones are `\_PIC`'s calls).
    let pci0 = d.windows(4).rposition(|w| w == b"PCI0").unwrap();
    let pci1 = d.windows(4).rposition(|w| w == b"PCI1").unwrap();
    let gsi150 = [0x0a, 150];
    assert!(d[pci0..pci1].windows(2).any(|w| w == gsi150));
}

/// Disassemble and recompile the assembled DSDT with `iasl` when present.
#[test]
fn dsdt_passes_iasl() {
    use std::process::Command;
    if Command::new("iasl").arg("-v").output().is_err() {
        std::eprintln!("iasl not installed; skipping");
        return;
    }
    for g in [Generation::Naples, Generation::Rome, Generation::Milan] {
        let s = system(g, 2, 128);
        let rb = root_bridges(&s);
        let t = assemble(&build(&inputs(&s, &rb, None)).unwrap());
        let dir = std::env::temp_dir().join(std::format!("sp3-acpi-{}-{g:?}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("dsdt.aml"), get(&t, b"DSDT").unwrap()).unwrap();
        let run = |args: &[&str]| {
            let o = Command::new("iasl")
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            let text = std::string::String::from_utf8_lossy(&o.stdout).into_owned()
                + &std::string::String::from_utf8_lossy(&o.stderr);
            assert!(o.status.success(), "iasl {args:?} failed:\n{text}");
            text
        };
        run(&["-d", "dsdt.aml"]);
        let compiled = run(&["-tc", "-p", "rebuilt", "dsdt.dsl"]);
        assert!(!compiled.contains("Error "), "{compiled}");
        // SP3_ACPI_KEEP=1 keeps the disassembly for inspection.
        if std::env::var_os("SP3_ACPI_KEEP").is_none() {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }
}

#[test]
fn cppc_two_sockets_have_independent_subspaces_and_local_slots() {
    let mut s = system(Generation::Milan, 2, 4);
    let channels = [
        Channel {
            info: CppcInfo::from_answer(0x007f_ffff),
            base: 0x7000_0000,
            smu_bar: 0xf100_0000,
            lowest_mhz: 400,
            nominal_mhz: 2300,
            slots: &[0, 1],
        },
        Channel {
            info: CppcInfo::from_answer(0x007f_ffff),
            base: 0x7001_0000,
            smu_bar: 0xf200_0000,
            lowest_mhz: 400,
            nominal_mhz: 2300,
            slots: &[128, 129],
        },
    ];
    s.cpus = cpus(&[0, 1, 128, 129], &channels);
    s.tables = cppc::pcct(&channels).into_iter().collect();
    let rb = root_bridges(&s);
    let built = build(&inputs(&s, &rb, None)).unwrap();
    let tables = assemble(&built);
    let pcct = get(&tables, b"PCCT").unwrap();
    assert_eq!(pcct.len(), 48 + 2 * 62);
    assert_eq!(&pcct[56..64], &channels[0].base.to_le_bytes());
    assert_eq!(&pcct[118..126], &channels[1].base.to_le_bytes());
    let dsdt = &built.dsdt;
    let start = dsdt.windows(4).position(|name| name == b"C002").unwrap();
    let end = dsdt.windows(4).position(|name| name == b"C003").unwrap();
    // Linux CPU UID2 / APIC128 belongs to subspace1, slot0, not global slot2.
    assert!(
        dsdt[start..end]
            .windows(15)
            .any(|r| r == [0x82, 12, 0, 0x0a, 32, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0])
    );
    // A thread no channel serves has no `_CPC`.
    assert_eq!(cppc::cpc(&channels, 2), Ok(Vec::new()));
    let mut bad = channels;
    bad[1].slots = &[0, 129];
    assert!(
        cppc::cpc(&bad, 0).is_err(),
        "one APIC ID cannot belong to two PCC controllers"
    );
}

#[test]
fn cppc_builds_the_pcct() {
    let s = system(Generation::Rome, 1, 8);
    let rb = root_bridges(&s);
    let t = assemble(&build(&inputs(&s, &rb, None)).unwrap());
    assert_eq!(get(&t, b"WSMT").expect("WSMT")[36], 3);
    assert!(get(&t, b"TPM2").is_some());
    let pcct = get(&t, b"PCCT").expect("PCCT");
    assert_eq!(pcct.len(), 110);
    assert_eq!(&pcct[56..64], &0x7FF0_0000u64.to_le_bytes());
}

/// The assembled table set of a 1P system and of a 2P system with a
/// domain per socket and EINJ, byte for byte.
#[test]
fn tables_are_pinned() {
    let fnv = |bytes: &[u8]| {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    };
    let pin = |tables: &Tables| {
        let mut buf = std::vec![0u8; 1 << 20];
        let len = tables.assemble_into(&mut buf, TABLE_ADDR).unwrap();
        (len, fnv(&buf[..len]))
    };
    let got: Vec<_> = [Generation::Naples, Generation::Rome, Generation::Milan]
        .into_iter()
        .map(|g| {
            let s = system(g, 1, 64);
            let rb = root_bridges(&s);
            let one = pin(&build(&inputs(&s, &rb, None)).unwrap());

            let mut s = system(g, 2, 128);
            s.cpus.iter_mut().for_each(|c| c.domain = c.apic_id % 2);
            s.tables.insert(0, einj::table());
            let mut rb = root_bridges(&s);
            rb.iter_mut()
                .zip(&s.bridges)
                .for_each(|(rb, bridge)| rb.domain = Some(bridge.socket.into()));
            let domains = [Domain { socket: 0 }, Domain { socket: 1 }];
            let memory = [
                MemoryAffinity {
                    base: 0,
                    end: 0x8000_0000,
                    domain: 0,
                },
                MemoryAffinity {
                    base: 0x1_0000_0000,
                    end: 0x10_0000_0000,
                    domain: 1,
                },
            ];
            let numa = Numa {
                domains: &domains,
                memory: &memory,
                phys_addr_bits: 48,
            };
            (one, pin(&build(&inputs(&s, &rb, Some(numa))).unwrap()))
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                (0xc360, 0x2d1a_f2e0_2bf5_49dd),
                (0x1_8700, 0x2ed6_3a70_0da3_2d1b)
            ),
            (
                (0xc920, 0xda56_331a_8cb9_fa85),
                (0x1_9300, 0x39a3_40d2_6976_dd4d)
            ),
            (
                (0xc920, 0x940a_6597_4f7a_0ecb),
                (0x1_9300, 0x66dc_12ee_7357_08d5)
            ),
        ]
    );
}

#[test]
fn fadt_flags_match_the_donor_pins() {
    let rr = fstart_acpi::fadt::Flags::ResetRegSup as u32;
    assert_eq!(fadt_flags(Generation::Naples) | rr, 0x305AD);
    assert_eq!(fadt_flags(Generation::Rome) | rr, 0x305AD);
    assert_eq!(fadt_flags(Generation::Milan) | rr, 0x2052D);
}

/// The IVHD entries of the FCH NBIO (NBIO3, bus 0) and of another one.
#[test]
fn fch_nbio_gets_the_fch_entries() {
    use fstart_acpi::ivrs::Variety;
    let s = system(Generation::Rome, 1, 8);
    let rb = root_bridges(&s);
    let t = assemble(&build(&inputs(&s, &rb, None)).unwrap());
    let ivrs = get(&t, b"IVRS").unwrap();
    // 10h + 11h per IOMMU (EFR supported), in topology order.
    let mut blocks = Vec::new();
    let mut at = 48;
    while at < ivrs.len() {
        let length = usize::from(u16::from_le_bytes([ivrs[at + 2], ivrs[at + 3]]));
        blocks.push((ivrs[at], ivrs[at + 5], &ivrs[at..at + length]));
        at += length;
    }
    assert_eq!(
        blocks.iter().map(|b| (b.0, b.1)).collect::<Vec<_>>(),
        [0x20, 0x40, 0x60, 0]
            .map(|bus| [(0x10, bus), (0x11, bus)])
            .concat()
    );
    let entries = |block: &[u8]| (block.len() - 40) / 8;
    let special = |variety, handle, source: u16, dte| {
        let kind = if variety == Variety::IoApic { 1 } else { 2 };
        let [low, high] = source.to_le_bytes();
        [0x48, 0, 0, dte, handle, low, high, kind]
    };
    let has = |block: &[u8], entry: [u8; 8]| block.windows(8).any(|w| w == entry);
    // NBIO0 on bus 0x20: its buses from 20:01.0 and its IOAPIC 0xF1.
    let other = blocks[1].2;
    assert_eq!(entries(other), 2);
    assert_eq!(other[40..44], [3, 0x08, 0x20, 0]);
    assert_eq!(other[44..48], [4, 0xff, 0x3f, 0]);
    assert!(has(other, special(Variety::IoApic, 0xF1, 0x2001, 0)));
    // NBIO3 has the FCH: also the FCH IOAPIC and the HPET behind 00:14.0.
    let fch = blocks[7].2;
    assert_eq!(entries(fch), 4);
    assert!(has(fch, special(Variety::IoApic, 0xF4, 0x0001, 0)));
    assert!(has(fch, special(Variety::IoApic, 0xF0, 0x00A0, 0xC7)));
    assert!(has(fch, special(Variety::Hpet, 0, 0x00A0, 0)));

    // No IOMMU, no IVRS.
    let mut none = root_bridges(&s);
    none.iter_mut().for_each(|rb| rb.iommu = None);
    let t = assemble(&build(&inputs(&s, &none, None)).unwrap());
    assert!(get(&t, b"IVRS").is_none());
}

const CPUS: [Cpu; 2] = [
    Cpu {
        apic_id: 0,
        domain: 0,
        aml: Vec::new(),
    },
    Cpu {
        apic_id: 0x40,
        domain: 1,
        aml: Vec::new(),
    },
];

#[test]
fn memory_covers_both_sockets_without_replicated_ranges() {
    use fstart_driver_amd::sp3::df::DramRule;
    use fstart_driver_amd::sp3::df::dram::{DramMap, Nps, memory_from_maps, numa_domains};
    let map = |base, end, dst, hole| DramMap {
        rules: core::iter::once(DramRule {
            base,
            end,
            dst,
            hole,
            socket_interleave: false,
            die_interleave: 0,
            channel_interleave: 3,
        })
        .collect(),
        hole_base: Some(0xb000_0000),
    };
    let local = map(0, 0x20_5000_0000, 0, true);
    let remote = map(0x20_5000_0000, 0x40_5000_0000, 0x20, false);
    let memory = memory_from_maps([local.clone(), remote, local], &numa_domains(Nps::Nps1, 2));
    assert_eq!(
        memory,
        [
            (0, 0xb000_0000, 0),
            (0x1_0000_0000, 0x20_5000_0000, 0),
            (0x20_5000_0000, 0x40_5000_0000, 1),
        ]
    );
    assert_eq!(
        memory.iter().map(|&(b, e, _)| e - b).sum::<u64>(),
        256 << 30
    );
}

#[test]
fn one_domain_emits_nothing() {
    let one = Numa {
        domains: &[Domain { socket: 0 }],
        memory: &[],
        phys_addr_bits: 48,
    };
    assert!(numa::tables(of(Generation::Rome), &one, &CPUS).is_empty());
}

#[test]
fn nps4_on_two_sockets() {
    let domains: Vec<Domain> = (0..8).map(|d| Domain { socket: d / 4 }).collect();
    let memory = [
        MemoryAffinity {
            base: 0,
            end: 0x8000_0000,
            domain: 0,
        },
        MemoryAffinity {
            base: 0x1_0000_0000,
            end: 0x2_0000_0000,
            domain: 5,
        },
    ];
    let nps4 = Numa {
        domains: &domains,
        memory: &memory,
        phys_addr_bits: 48,
    };
    let t = numa::tables(of(Generation::Milan), &nps4, &CPUS);
    let sigs: Vec<&[u8]> = t.iter().map(|t| &t[..4]).collect();
    assert_eq!(sigs, [b"SRAT", b"MSCT", b"SLIT"]);
    let slit = &t[2][44..];
    assert_eq!(slit.len(), 64);
    assert_eq!(&slit[..8], &[10, 12, 12, 12, 32, 32, 32, 32]);
    for t in &t {
        assert_eq!(t.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
    }
}

#[test]
fn naples_die_distances() {
    let (a, b, c) = (
        Domain { socket: 0 },
        Domain { socket: 0 },
        Domain { socket: 1 },
    );
    assert_eq!(numa::distance(of(Generation::Naples), a, b), 16);
    assert_eq!(numa::distance(of(Generation::Naples), a, c), 32);
    assert_eq!(numa::distance(of(Generation::Rome), a, b), 12);
}
