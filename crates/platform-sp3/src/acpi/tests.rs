//! Whole-set tests: every table assembles with valid checksums, the
//! per-generation FADT/MADT values, NUMA gating, x2APIC, and the DSDT
//! through `iasl` when it is installed.

use std::vec::Vec;

use fstart_acpi::devices::IpmiKcsAcpi;
use fstart_acpi::ivrs::IommuRegisters;
use fstart_driver_amd::sp3::nbio::iommu::IommuInfo;

use super::*;
use fstart_driver_amd::ccx::power::{CstateInfo, Pstate};

const TABLE_ADDR: u64 = 0x7F00_0000;

/// A 1P (or 2P) S8030-like system: 4 NBIOs per socket, bus 0 on NBIO3
/// (the FCH NBIO, IOAPIC 0xF4 → GSI 120 as on the donor).
struct System {
    cpus: Vec<Cpu>,
    ioapics: Vec<IoapicInfo>,
    iommus: Vec<IommuAcpi>,
    bridges: Vec<RootBridge>,
}

const IO: [Window<u16>; 1] = [Window {
    base: 0,
    limit: 0x0FFF,
}];
const IO_HI: [Window<u16>; 1] = [Window {
    base: 0x1000,
    limit: 0x1FFF,
}];
const MMIO: [Window<u64>; 2] = [
    Window {
        base: 0xC000_0000,
        limit: 0xC7FF_FFFF,
    },
    Window {
        base: 0x100_0000_0000,
        limit: 0x10F_FFFF_FFFF,
    },
];

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
    let ioapics = bridges
        .iter()
        .map(|b| IoapicInfo {
            socket: b.socket,
            index: b.index,
            id: fstart_driver_amd::sp3::nbio::ioapic::nbio_ioapic_id(
                generation,
                4 * b.socket + b.index,
            ),
            base: 0xA000_0000 + (u64::from(4 * b.socket + b.index) << 20),
            bus: b.bus_base,
        })
        .collect();
    let iommus = bridges
        .iter()
        .map(|b| IommuAcpi {
            info: IommuInfo {
                socket: b.socket,
                index: b.index,
                bus: b.bus_base,
                devfn: 2,
                cap_offset: 0x40,
                base: 0xB000_0000 + (u64::from(4 * b.socket + b.index) << 19),
                segment: 0,
                fch: b.primary,
            },
            registers: IommuRegisters {
                capability: 1 << 27,
                ..Default::default()
            },
            bus_limit: b.bus_limit,
        })
        .collect();
    let cpus = (0..threads)
        .map(|t| Cpu {
            apic_id: t,
            domain: 0,
        })
        .collect();
    System {
        cpus,
        ioapics,
        iommus,
        bridges,
    }
}

fn root_bridges(s: &System) -> Vec<RootBridgeAcpi<'static>> {
    s.bridges
        .iter()
        .map(|&bridge| RootBridgeAcpi {
            bridge,
            io: if bridge.primary { &IO } else { &IO_HI },
            mmio: &MMIO,
            domain: None,
            ioapic_features: 0x1430_0000 + (u32::from(bridge.index) << 20),
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
fn fch() -> &'static FchAcpi {
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
    std::boxed::Box::leak(std::boxed::Box::new(f))
}

fn inputs<'a>(
    generation: Generation,
    s: &'a System,
    bridges: &'a [RootBridgeAcpi<'a>],
    numa: Option<Numa<'a>>,
) -> Inputs<'a> {
    Inputs {
        generation,
        pci: crate::pci::segment(generation),
        cpus: &s.cpus,
        fch_ioapic_id: fstart_driver_amd::sp3::nbio::ioapic::fch_ioapic_id(generation),
        ioapics: &s.ioapics,
        hpet_block_id: 0x1022_8201,
        root_bridges: bridges,
        iommus: &s.iommus,
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
        einj: false,
        fch: fch(),
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
        cppc: std::boxed::Box::leak(
            vec![CppcAcpi {
                info: CppcInfo::from_answer(0x007F_FFFF),
                base: 0x7FF0_0000,
                smu_bar: 0xF110_0000,
                lowest_mhz: 1500,
                nominal_mhz: 2500,
                slots: std::boxed::Box::leak(
                    s.cpus
                        .iter()
                        .map(|c| c.apic_id)
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                ),
            }]
            .into_boxed_slice(),
        ),
        board: Board {
            ipmi: Some(IpmiKcsAcpi {
                port: 0xCA2,
                spacing: 1,
                uid: 0,
                spec_revision: 0x0200,
            }),
            i8042: true,
            lpc_aml: &[],
            sb_aml: &[],
        },
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
        let t = assemble(&build(&inputs(g, &s, &rb, None)).unwrap());
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
        let t = assemble(&build(&inputs(g, &s, &rb, None)).unwrap());
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
    let t = assemble(&build(&inputs(Generation::Rome, &s, &rb, None)).unwrap());
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
    let t = assemble(&build(&inputs(Generation::Milan, &s, &rb, None)).unwrap());
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
    let t = assemble(&build(&inputs(g, &s, &rb, Some(numa))).unwrap());
    assert!(get(&t, b"SRAT").is_some() && get(&t, b"MSCT").is_some());
    assert_eq!(&get(&t, b"SLIT").unwrap()[44..], &[10, 32, 32, 10]);

    let one = Numa {
        domains: &domains[..1],
        ..numa
    };
    let t = assemble(&build(&inputs(g, &s, &rb, Some(one))).unwrap());
    assert!(get(&t, b"SRAT").is_none() && get(&t, b"SLIT").is_none());
}

#[test]
fn dsdt_root_bridges_in_bus_order() {
    let s = system(Generation::Milan, 1, 8);
    let rb = root_bridges(&s);
    let tables = build(&inputs(Generation::Milan, &s, &rb, None)).unwrap();
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
        let t = assemble(&build(&inputs(g, &s, &rb, None)).unwrap());
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
    s.cpus[2].apic_id = 128;
    s.cpus[3].apic_id = 129;
    let rb = root_bridges(&s);
    let channels = [
        CppcAcpi {
            info: CppcInfo::from_answer(0x007f_ffff),
            base: 0x7000_0000,
            smu_bar: 0xf100_0000,
            lowest_mhz: 400,
            nominal_mhz: 2300,
            slots: &[0, 1],
        },
        CppcAcpi {
            info: CppcInfo::from_answer(0x007f_ffff),
            base: 0x7001_0000,
            smu_bar: 0xf200_0000,
            lowest_mhz: 400,
            nominal_mhz: 2300,
            slots: &[128, 129],
        },
    ];
    let mut i = inputs(Generation::Milan, &s, &rb, None);
    i.cppc = &channels;
    let built = build(&i).unwrap();
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
    let mut bad = channels;
    bad[1].slots = &[0, 129];
    i.cppc = &bad;
    assert!(
        build(&i).is_err(),
        "one APIC ID cannot belong to two PCC controllers"
    );
}

#[test]
fn cppc_builds_the_pcct() {
    let s = system(Generation::Rome, 1, 8);
    let rb = root_bridges(&s);
    let t = assemble(&build(&inputs(Generation::Rome, &s, &rb, None)).unwrap());
    assert_eq!(get(&t, b"WSMT").expect("WSMT")[36], 3);
    assert!(get(&t, b"TPM2").is_some());
    let pcct = get(&t, b"PCCT").expect("PCCT");
    assert_eq!(pcct.len(), 110);
    assert_eq!(&pcct[56..64], &0x7FF0_0000u64.to_le_bytes());
}
