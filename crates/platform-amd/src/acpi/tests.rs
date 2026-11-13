// SPDX-License-Identifier: MIT OR Apache-2.0
//! The tables of the die SoCs as their flow fills the inputs
//! ([`crate::noncar::acpi`]): routing, MADT, IVRS, NUMA, the DSDT through
//! `iasl` and `acpiexec` when they are installed, and the table set of an
//! MJ11 byte for byte. SP3's are `crate::sp3::acpi`'s tests.
extern crate std;

use core::ops::RangeInclusive;
use std::{vec, vec::Vec};

use fstart_acpi::ivrs::{DeviceEntry, Variety};
use fstart_acpi::platform::PlatformConfig;
use fstart_driver_amd::ccx::power;
use fstart_driver_amd::nbio::iommu::Iommu;

use super::*;
use crate::noncar::acpi::{FCH_IOAPIC_ID, cpus, root_bridge};
use crate::zeppelin::acpi::ACPI;

static IO: RangeInclusive<u16> = 0..=0xffff;

/// A root bridge with the I/O window `io` and the MMIO windows `mmio`.
pub(crate) fn pci_root(
    buses: RangeInclusive<u8>,
    io: &RangeInclusive<u16>,
    mmio: &[RangeInclusive<u64>],
) -> &'static fstart_pci::PciRoot {
    use fstart_pci::PciRole::{Forwarded, Own, Window};
    use fstart_pci::{PciRoot, PciRootInfo};
    let size = |range: &RangeInclusive<u64>| range.end() - range.start() + 1;
    let (ports, ecam) = (u64::from(*io.start())..=u64::from(*io.end()), 0xf000_0000);
    let primary = |size: u64| if *buses.start() == 0 { size } else { 0 };
    // PCI devices get the ports above the legacy ones.
    let pci = (*ports.start()).max(0x1000);
    let root = PciRoot::new(
        PciRootInfo {
            segment: 0,
            ecam_base: ecam,
            bus_start: *buses.start(),
            bus_end: *buses.end(),
        },
        // The die with bus 0 has the legacy I/O and VGA memory routed
        // to it and owns the ECAM window of its 128 buses.
        [Forwarded.io(0, primary(0x1000))]
            .into_iter()
            .chain([Window.io(pci, (ports.end() + 1).saturating_sub(pci))])
            .chain([Forwarded.mmio(0xa_0000, primary(0x2_0000))])
            .chain(
                mmio.iter()
                    .map(|range| Window.mmio(*range.start(), size(range))),
            )
            .chain([Own.mmio(ecam, primary(128 << 20))]),
    )
    .unwrap();
    std::boxed::Box::leak(std::boxed::Box::new(root))
}

const MMIO: [RangeInclusive<u64>; 2] =
    [0xe000_0000..=0xefff_ffff, 0x100_0000_0000..=0x1ff_ffff_ffff];

/// An IOMMU with an EFR.
const IOMMU: Iommu = Iommu {
    base: 0,
    capability: 1 << 27 | 0x0003_000f,
    range: 0,
    misc: 0,
    control: 0,
    efr: 0,
    efr2: 0,
    counter_config: 0,
};

const PCI: PciRootInfo = PciRootInfo {
    segment: 0,
    ecam_base: 0xf000_0000,
    bus_start: 0,
    bus_end: 127,
};

/// The two dies of an SP4.
fn dies(numa: bool) -> [RootBridgeAcpi<'static>; 2] {
    [0u8, 1].map(|die| {
        let iommu = Iommu {
            base: 0xfe60_0000 - u64::from(die) * 0x0800_0000,
            ..IOMMU
        };
        root_bridge(
            die,
            pci_root(die * 64..=die * 64 + 63, &IO, &MMIO),
            0xfeb0_0000 - u32::from(die) * 0x0800_0000,
            Some(iommu),
            numa,
        )
    })
}

/// EPYC 3151 P-states: 2.7, 1.9 and 1.2 GHz.
fn power() -> power::Power<'static> {
    static PSTATES: std::sync::LazyLock<[power::Pstate; 3]> = std::sync::LazyLock::new(|| {
        [(0, 0x6c, 8), (1, 0x98, 0x10), (2, 0x60, 0x10)].map(|(i, fid, did)| {
            power::decode_pstate(i, 1 << 63 | 0x50 << 22 | 0x40 << 14 | did << 8 | fid).unwrap()
        })
    });
    power::Power {
        pstates: &*PSTATES,
        cstates: Some(power::CstateInfo {
            io_base: 0x813,
            cc6: true,
        }),
        threads_per_core: 2,
        c2_latency_us: 400,
    }
}

/// The inputs as [`crate::noncar::acpi::install`] fills them.
pub(crate) fn inputs<'a>(
    soc: &'a SocAcpi,
    cpus: &'a [Cpu],
    root_bridges: &'a [RootBridgeAcpi<'a>],
    board: &'a Board,
) -> Inputs<'a> {
    Inputs {
        soc,
        pci: PCI,
        cpus,
        fch_ioapic_id: FCH_IOAPIC_ID,
        hpet_block_id: 0x1022_8201,
        root_bridges,
        numa: None,
        power: Some(power()),
        acpi_smi: Some(AcpiSmiConfig {
            smi_cmd: fstart_driver_amd::fch::smi::APM_CNT.into(),
            acpi_enable: fstart_driver_amd::fch::smi::ACPI_ENABLE,
            acpi_disable: fstart_driver_amd::fch::smi::ACPI_DISABLE,
        }),
        bmc_sx_notify: None,
        tpm: None,
        devices: &[],
        sb_aml: &[],
        tables: &[],
        board,
    }
}

/// The tables of an SP4 with `board`'s part.
fn tables(board: &Board) -> Tables {
    let (dies, cpus) = (dies(false), cpus(&[0, 1, 8, 9, 2, 3], false));
    build(&inputs(&ACPI, &cpus, &dies, board)).unwrap()
}

/// The `_PRT` packages after `name`: (address, pin, GSI).
fn prt_of(aml: &[u8], name: &[u8; 4]) -> Vec<(u64, u64, u64)> {
    let start = aml.windows(4).position(|w| w == name).unwrap();
    let prt = start + aml[start..].windows(4).position(|w| w == b"_PRT").unwrap();
    let integer = |at: &mut usize| {
        let a = *at;
        let (value, len) = match aml[a] {
            0x00 => (0, 1),
            0x01 => (1, 1),
            0x0a => (u64::from(aml[a + 1]), 2),
            0x0b => (u64::from(u16::from_le_bytes([aml[a + 1], aml[a + 2]])), 3),
            0x0c => (
                u64::from(u32::from_le_bytes(aml[a + 1..a + 5].try_into().unwrap())),
                5,
            ),
            op => panic!("unexpected AML integer {op:#x}"),
        };
        *at += len;
        value
    };
    // PackageOp, PkgLength (lead byte bits 7:6 count the extra bytes),
    // NumElements; returns that count.
    let package = |at: &mut usize| {
        assert_eq!(aml[*at], 0x12);
        *at += 2 + usize::from(aml[*at + 1] >> 6);
        let count = aml[*at];
        *at += 1;
        count
    };
    let mut at = prt + 4;
    (0..package(&mut at))
        .map(|_| {
            package(&mut at);
            let [address, pin, _, gsi] = [(); 4].map(|_| integer(&mut at));
            (address, pin, gsi)
        })
        .collect()
}

/// The (pin, GSI) of a bridge `_PRT`.
fn bridge_prt(aml: &[u8], name: &[u8; 4]) -> Vec<(u64, u64)> {
    prt_of(aml, name)
        .into_iter()
        .map(|(_, pin, gsi)| (pin, gsi))
        .collect()
}

/// Where the device `PCIn` starts: its last occurrence (the first ones
/// are `\_PIC`'s and the wake GPE's paths).
fn device(aml: &[u8], name: &[u8; 4]) -> usize {
    aml.windows(4).rposition(|w| w == name).unwrap()
}

#[test]
fn video_rom_targets_the_discovered_function_without_duplicating_root_ports() {
    use fstart_pci::PciAddress as P;
    let rom = [0x55, 0xaa, 1, 0xcb];
    assert_eq!(video_rom_aml(0, &[], &rom), Err(AmlError::InvalidPath));
    let direct = video_rom_aml(0, &[P::new(0, 0, 3, 2)], &rom).unwrap();
    assert!(direct.windows(9).any(|w| w == b"_ADR\x0c\x02\x00\x03\x00"));
    let path = [P::new(0, 0, 8, 1), P::new(0, 1, 3, 2), P::new(0, 2, 0, 1)];
    let aml = video_rom_aml(0, &path, &rom).unwrap();
    assert!(aml.windows(4).any(|w| w == b"RP81"));
    assert!(aml.windows(4).any(|w| w == b"D01A"));
    assert!(aml.windows(4).any(|w| w == b"D001"));
    // Only the intermediate bridge and display function are new devices.
    assert_eq!(aml.windows(2).filter(|w| *w == b"\x5b\x82").count(), 2);
    let (dies, cpus) = (dies(false), cpus(&[0], false));
    let board = Board::default();
    let tables = build(&Inputs {
        sb_aml: &aml,
        ..inputs(&ACPI, &cpus, &dies, &board)
    })
    .unwrap();
    assert!(tables.dsdt.windows(4).any(|w| w == b"_ROM"));
}

#[test]
fn table_storage_has_room_for_a_full_128k_video_rom() {
    use fstart_pci::PciAddress as P;
    let path = [P::new(0, 0, 8, 1), P::new(0, 4, 0, 0)];
    let rom = vec![0x5a; 0x20000];
    let aml = video_rom_aml(0, &path, &rom).unwrap();
    let vfct = fstart_acpi::vfct::build_vfct(
        [0; 16],
        &[fstart_acpi::vfct::VbiosImage {
            segment: 0,
            bus: 4,
            device: 0,
            function: 0,
            vendor_id: 0x1002,
            device_id: 0x15d8,
            subsystem_vendor_id: 0,
            subsystem_id: 0,
            revision: 0,
            rom: &rom,
        }],
    )
    .unwrap();
    let (dies, cpus) = (dies(false), cpus(&[0], false));
    let board = Board::default();
    let tables = build(&Inputs {
        sb_aml: &aml,
        tables: &[vfct],
        ..inputs(&ACPI, &cpus, &dies, &board)
    })
    .unwrap();
    // The die SoCs' flow reserves 512 KiB for both transports plus tables.
    let mut storage = vec![0; 0x80000];
    let used = tables.assemble_into(&mut storage, 0x7000_0000).unwrap();
    assert!(used > 0x40000 && used <= storage.len());
    let vfct = signature(&storage[..used], b"VFCT");
    assert_eq!(&vfct[104..], rom.as_slice());
    let mut too_small = vec![0xa5; 0x40000];
    assert_eq!(
        tables.assemble_into(&mut too_small, 0x7000_0000),
        Err(AmlError::Capacity)
    );
    assert!(too_small.iter().all(|&byte| byte == 0xa5));
}

#[test]
fn root_prt_has_the_fch_interrupts_on_die_0_and_the_bridges_own() {
    let aml = tables(&Board::default()).dsdt;
    let (pci0, pci1) = (device(&aml, b"PCI0"), device(&aml, b"PCI1"));
    let root0 = prt_of(&aml[pci0..], b"PCI0");
    // FCH INTA-D for the IOMMU and SMBus/LPC, then the bridges on the
    // NBIO IOAPIC, from GSI 24: the PCIe ports, 7.1 and 8.1 on the pins
    // 0x14 and 0x08.
    assert_eq!(
        root0[..4],
        [
            (0xffff, 0, 16),
            (0xffff, 1, 17),
            (0xffff, 2, 18),
            (0xffff, 3, 19)
        ]
    );
    assert_eq!(root0[4], (0x14_ffff, 0, 16));
    assert_eq!(root0[8], (0x1_ffff, 0, 40));
    assert_eq!(root0.len(), 20);
    assert_eq!(root0[18..], [(0x7_ffff, 0, 44), (0x8_ffff, 0, 32)]);
    // Die 1 has no FCH below it: its NBIO IOAPIC's pins from GSI 56.
    let root1 = prt_of(&aml[pci1..], b"PCI1");
    assert_eq!(root1.len(), 12);
    assert_eq!(root1[0], (0x1_ffff, 0, 72));
    assert_eq!(root1[10..], [(0x7_ffff, 0, 76), (0x8_ffff, 0, 64)]);
}

#[test]
fn bridge_prts_route_through_the_nbio_ioapic_of_their_die() {
    let aml = tables(&Board::default()).dsdt;
    let (pci0, pci1) = (device(&aml, b"PCI0"), device(&aml, b"PCI1"));
    // 3.1: group 7, CDAB on die 0: GSI 24 + 28 + 2, 3, 0, 1
    assert_eq!(
        bridge_prt(&aml[pci0..], b"RP31"),
        [(0, 54), (1, 55), (2, 52), (3, 53)]
    );
    // 7.1 (xHCI): group 3, DABC
    assert_eq!(
        bridge_prt(&aml[pci0..], b"RP71"),
        [(0, 39), (1, 36), (2, 37), (3, 38)]
    );
    // 8.1 on die 1: group 4, BCDA from GSI 56
    assert_eq!(
        bridge_prt(&aml[pci1..], b"RP81"),
        [(0, 73), (1, 74), (2, 75), (3, 72)]
    );
}

/// The I/O an eSPI EC owns is its own devices', not the FCH's: SYSR
/// leaves out the ranges it forwards there (Vilboz: the ACPI EC ports and
/// the memory map at 0x900), and keeps the rest.
#[test]
fn the_fch_reserves_no_io_a_board_peripheral_owns() {
    let root = [root_bridge(
        0,
        pci_root(0..=255, &IO, &MMIO),
        0xfec0_1000,
        None,
        false,
    )];
    let cpus = cpus(&[0, 1], false);
    let io = |board: &Board| {
        let aml = build(&inputs(&crate::picasso::acpi::ACPI, &cpus, &root, board))
            .unwrap()
            .dsdt;
        let sysr = device(&aml, b"SYSR");
        // IO descriptors: 0x47, decode, min, max, alignment, length.
        aml[sysr..]
            .windows(8)
            .filter(|w| w[0] == 0x47 && w[1] == 1 && w[2..4] == w[4..6])
            .map(|w| u16::from_le_bytes([w[2], w[3]]))
            .collect::<Vec<_>>()
    };
    let all = io(&Board::default());
    let vilboz = io(&Board {
        peripheral_io: vec![(0x62, 5), (0x800, 0x100), (0x900, 0xff), (0x200, 8)],
        ..Default::default()
    });
    for port in [0x63, 0x65, 0x900, 0x910] {
        assert!(all.contains(&port) && !vilboz.contains(&port), "{port:#x}");
    }
    assert_eq!(all.len() - vilboz.len(), 4);
    assert!(vilboz.contains(&0x67) && vilboz.contains(&0x80));
}

/// Picasso has one root bridge with the bridges of its routing table.
#[test]
fn picasso_routes_its_internal_bridges() {
    let root = [root_bridge(
        0,
        pci_root(0..=255, &IO, &MMIO),
        0xfec0_1000,
        None,
        false,
    )];
    let (cpus, board) = (cpus(&[0, 1], false), Board::default());
    let aml = build(&inputs(&crate::picasso::acpi::ACPI, &cpus, &root, &board))
        .unwrap()
        .dsdt;
    let pci0 = device(&aml, b"PCI0");
    let prt = prt_of(&aml[pci0..], b"PCI0");
    assert_eq!(prt[prt.len() - 2..], [(0x8_ffff, 0, 43), (0x8_ffff, 1, 36)]);
    assert_eq!(
        bridge_prt(&aml[pci0..], b"RP82"),
        [(0, 54), (1, 55), (2, 52), (3, 53)]
    );
    // Neither a wake GPE nor the server's second PCIe core.
    assert!(!aml.windows(4).any(|w| w == b"_PRW" || w == b"RP31"));
}

#[test]
fn madt_lists_first_threads_first_and_the_nbio_ioapics() {
    let tables = tables(&Board::default());
    let PlatformConfig::X86(config) = &tables.platform else {
        unreachable!()
    };
    assert_eq!(config.apic_ids, [0, 2, 8, 1, 3, 9]);
    let ioapics: Vec<_> = config
        .ioapics
        .iter()
        .map(|ioapic| (ioapic.id, ioapic.base, ioapic.gsi_base))
        .collect();
    assert_eq!(
        ioapics,
        [
            (0, 0xfec0_0000, 0),
            (1, 0xfeb0_0000, 24),
            (2, 0xf6b0_0000, 56)
        ]
    );
}

#[test]
fn ivrs_has_both_dies_with_their_buses_and_the_madt_ioapic_ids() {
    let (dies, cpus) = (dies(false), cpus(&[0], false));
    let board = Board::default();
    let inputs = inputs(&ACPI, &cpus, &dies, &board);
    let windows = fixed::gsi_windows(FCH_IOAPIC_ID, &dies);
    let special = |variety, handle, source, dte| DeviceEntry::Special {
        variety,
        handle,
        source,
        dte,
    };
    let blocks = |table: &[u8]| {
        let mut at = 48;
        let mut blocks = Vec::new();
        while at < table.len() {
            let (kind, length) = (table[at], [table[at + 2], table[at + 3]]);
            let device = u16::from_le_bytes([table[at + 4], table[at + 5]]);
            blocks.push((kind, device));
            at += usize::from(u16::from_le_bytes(length));
        }
        blocks
    };

    // IVHD 10h and 11h of the IOMMUs at 0:00.2 and 40:00.2, each with its
    // buses from device 1, then its NBIO IOAPIC, and die 0's with the FCH
    // IOAPIC and the HPET behind 0:14.0.
    let table = ivrs::ivrs(&inputs, &windows).unwrap();
    assert_eq!(
        blocks(&table),
        [
            (0x10, 0x0002),
            (0x11, 0x0002),
            (0x10, 0x4002),
            (0x11, 0x4002)
        ]
    );
    let has = |entry: DeviceEntry| {
        let DeviceEntry::Special {
            variety,
            handle,
            source,
            dte,
        } = entry
        else {
            unreachable!()
        };
        let kind = if variety == Variety::IoApic { 1 } else { 2 };
        let [low, high] = source.to_le_bytes();
        let bytes = [0x48, 0, 0, dte, handle, low, high, kind];
        table.windows(8).filter(|w| *w == bytes).count()
    };
    // Once per IVHD type.
    assert_eq!(has(special(Variety::IoApic, 1, 0x0001, 0)), 2);
    assert_eq!(has(special(Variety::IoApic, 2, 0x4001, 0)), 2);
    assert_eq!(
        has(special(Variety::IoApic, FCH_IOAPIC_ID, 0x00a0, 0xc7)),
        2
    );
    assert_eq!(has(special(Variety::Hpet, 0, 0x00a0, 0)), 2);
    // Start of range 0:01.0 and 40:01.0.
    for start in [[3, 0x08, 0x00, 0], [3, 0x08, 0x40, 0]] {
        assert_eq!(table.windows(4).filter(|w| *w == start).count(), 2);
    }
    assert!(
        tables(&board)
            .extra
            .iter()
            .any(|table| table[..4] == *b"IVRS")
    );

    // Without die 0's IOMMU, die 1's is still described.
    let mut without = dies;
    without[0].iommu = None;
    let table = ivrs::ivrs(
        &Inputs {
            root_bridges: &without,
            ..inputs
        },
        &windows,
    )
    .unwrap();
    assert_eq!(blocks(&table), [(0x10, 0x4002), (0x11, 0x4002)]);
}

/// The ACPI-only eMMC of a Picasso board gets an IVHD 40h entry.
#[test]
fn an_acpi_only_emmc_has_its_ivhd_entry() {
    let root = [root_bridge(
        0,
        pci_root(0..=255, &IO, &MMIO),
        0xfec0_1000,
        Some(IOMMU),
        false,
    )];
    let (cpus, board) = (cpus(&[0, 1], false), Board::default());
    let routes = fstart_driver_amd::fch::sandstone::legacy::Routes::new([0], []).unwrap();
    let emmc = [crate::picasso::acpi::emmc(&routes).unwrap()];
    let soc = &crate::picasso::acpi::ACPI;
    let with = Inputs {
        devices: &emmc,
        ..inputs(soc, &cpus, &root, &board)
    };
    let kinds = |inputs: &Inputs| {
        let windows = fixed::gsi_windows(FCH_IOAPIC_ID, inputs.root_bridges);
        let table = ivrs::ivrs(inputs, &windows).unwrap();
        let mut at = 48;
        let mut kinds = Vec::new();
        while at < table.len() {
            kinds.push(table[at]);
            at += usize::from(u16::from_le_bytes([table[at + 2], table[at + 3]]));
        }
        (kinds, table.windows(8).any(|w| w == b"AMDI0040"))
    };
    assert_eq!(kinds(&with), (vec![0x10, 0x11, 0x40], true));
    assert_eq!(
        kinds(&inputs(soc, &cpus, &root, &board)),
        (vec![0x10, 0x11], false)
    );
    // The device itself is in the DSDT.
    assert!(build(&with).unwrap().dsdt.windows(4).any(|w| w == b"MMC0"));
}

#[test]
fn root_bridge_resources_skip_the_configuration_ports() {
    // Die 0 consumes the configuration ports and gets the VGA window.
    const CFG: [u8; 8] = [0x47, 1, 0xf8, 0x0c, 0xf8, 0x0c, 1, 8];
    let vga = 0xa_0000u32.to_le_bytes();
    let [die0, die1] = dies(false);
    let die0 = fstart_acpi::pci_root::crs(die0.root);
    let die1 = fstart_acpi::pci_root::crs(die1.root);
    assert!(die0.windows(8).any(|w| w == CFG) && die0.windows(4).any(|w| w == vga));
    assert!(!die1.windows(8).any(|w| w == CFG) && !die1.windows(4).any(|w| w == vga));
}

#[test]
fn fixed_device_irq_polarity_survives_platform_assembly() {
    use fstart_core::acpi::{AcpiGenericDevice, AcpiInterruptFlags};
    let devices = [AcpiGenericDevice {
        name: fstart_core::hstr("MMC0"),
        hid: fstart_core::hstr("AMDI0040"),
        resources: Default::default(),
        gsiv: Some(5),
        interrupt_flags: AcpiInterruptFlags {
            active_low: true,
            ..Default::default()
        },
    }];
    let (dies, cpus) = (dies(false), cpus(&[0], false));
    let board = Board::default();
    let tables = build(&Inputs {
        devices: &devices,
        ..inputs(&ACPI, &cpus, &dies, &board)
    })
    .unwrap();
    assert!(
        tables
            .dsdt
            .windows(9)
            .any(|w| w == [0x89, 6, 0, 5, 1, 5, 0, 0, 0])
    );
}

/// The FCH GPIO controller and I2C controller 2 with a touchpad and
/// a HID-over-I2C alternative, as a Picasso board describes them.
fn fch_devices() -> Vec<u8> {
    use crate::picasso::acpi::fch_i2c_name;
    use fstart_acpi::devices::i2c::{
        DesignWareSpeed, GpioInterrupt, I2cDevice, designware_timing_aml,
    };
    let interrupt = Some(GpioInterrupt {
        controller: fstart_driver_amd::fch::aml::GPIO_PATH,
        pin: 9,
        edge: false,
        active_low: true,
    });
    let device = |name, ids, address, hid_descriptor| {
        I2cDevice {
            name,
            ids,
            uid: 0,
            bus: "\\_SB.I2C2",
            address,
            speed_hz: 400_000,
            interrupt,
            hid_descriptor,
        }
        .dsdt_aml()
        .unwrap()
    };
    let mut body = designware_timing_aml(DesignWareSpeed::Fast, 117, 245, 45).unwrap();
    body.extend(device("D150", &["ELAN0000"], 0x15, None));
    body.extend(device("D2C1", &["SYNA0000", "PNP0C50"], 0x2c, Some(0x20)));
    use fstart_driver_amd::fch::aml;
    let mut aml = aml::gpio(7, 0x400).unwrap();
    aml.extend(aml::i2c(&fch_i2c_name(2), 2, 0xfedc_4000, 14, &body).unwrap());
    aml
}

/// Assemble the table set and disassemble the DSDT with `iasl`.
#[test]
fn dsdt_disassembles() {
    let board = Board {
        ipmi: Some(fstart_acpi::devices::IpmiKcsAcpi {
            port: 0xca2,
            spacing: 1,
            uid: 0,
            spec_revision: 0x0200,
        }),
        sb_aml: fstart_acpi::devices::IpmiKcsAcpi::sleep_notify_aml(0xb2, 0xc0),
        ..Default::default()
    };
    let soc_aml = [
        fch_devices(),
        video_rom_aml(
            0,
            &[
                fstart_pci::PciAddress::new(0, 0, 8, 1),
                fstart_pci::PciAddress::new(0, 4, 0, 0),
            ],
            &[0x55, 0xaa, 1, 0xcb],
        )
        .unwrap(),
    ]
    .concat();
    let (dies, cpus) = (dies(false), cpus(&[0, 1, 8, 9, 2, 3], false));
    let tables = build(&Inputs {
        sb_aml: &soc_aml,
        ..inputs(&ACPI, &cpus, &dies, &board)
    })
    .unwrap();
    let mut storage = vec![0; 64 * 1024];
    let len = tables.assemble_into(&mut storage, 0x7f00_0000).unwrap();
    let fadt = signature(&storage[..len], b"FACP");
    assert_eq!(&fadt[46..48], &9u16.to_le_bytes()); // SCI_INT
    assert_eq!(&fadt[48..54], &[0xb2, 0, 0, 0, 0xe1, 0x1e]); // SMI_CMD, ACPI_EN/DIS
    assert_eq!(&fadt[56..60], &0x800u32.to_le_bytes()); // PM1a_EVT
    assert_eq!(&fadt[80..84], &0x820u32.to_le_bytes()); // GPE0
    assert_eq!(&fadt[109..111], &1u16.to_le_bytes()); // IAPC_BOOT_ARCH
    assert_eq!(fadt[128], 6); // RESET_VALUE
    assert!(signature(&storage[..len], b"MCFG").len() == 60);
    assert_eq!(
        signature(&storage[..len], b"WSMT")[36..40],
        3u32.to_le_bytes()
    );
    assert_eq!(
        signature(&storage[..len], b"HPET")[53..55],
        0x37eeu16.to_le_bytes()
    );

    let Ok(dsl) = iasl_disassemble(&fstart_acpi::platform::build_dsdt(&tables.dsdt)) else {
        std::eprintln!("iasl not found, skipping the DSDT disassembly");
        return;
    };
    for object in [
        "Name (_S1,",
        "Name (_S4,",
        "Name (_S5,",
        "Scope (\\_SB.PCI0.RP81)",
        "Device (D000)",
        "Method (_ROM, 2, Serialized)",
        "Device (PCI1)",
        "Device (RP81)",
        "Device (LPCB)",
        "Device (SYSR)",
        "Device (SPMI)",
        "PNP0C02",
        "Device (GPIO)",
        "Device (I2C2)",
        "Device (D150)",
        "Device (D2C1)",
        "Interrupt (ResourceConsumer, Level, ActiveLow, Shared, ,, )",
        "Interrupt (ResourceConsumer, Edge, ActiveHigh, Exclusive, ,, )",
        "I2cSerialBusV2 (0x0015, ControllerInitiated, 0x00061A80,",
        "\\_SB.I2C2",
        "GpioInt (Level, ActiveLow, Exclusive, PullDefault, 0x0000,",
        "\\_SB.GPIO",
        "Name (FMCN, Package",
        "Method (_DSM, 4, Serialized)",
        "ToUUID (\"3cdff6f7-4267-4555-ad05-b30a3d8938de\")",
        "Device (C005)",
        "Name (_HID, \"ACPI0007\"",
        "Name (_PSS, Package (0x03)",
        "0x0A8C,",
        "SystemIO,",
        "0x0000000000000814",
        "Name (_PSD, Package (0x01)",
        "Method (_PIC, 1, NotSerialized)",
        "\\_SB.PCI1.NBIO.NAPE ()",
        "NAPX = 0x14300000",
        "NAPD &= 0xFFFFFFEF",
        "Name (_PRW, Package (0x02)",
        "Method (_L0B, 0, NotSerialized)",
        "Notify (\\_SB.PCI1.RP71, 0x02)",
        "Method (_PTS, 1, NotSerialized)",
        "BSMC = (Arg0 + 0xC0)",
        "Method (_INI, 0, NotSerialized)",
    ] {
        assert!(dsl.contains(object), "{object} missing:\n{dsl}");
    }
    // Resolve and execute the real AMD namespace, not just an isolated
    // generic display fixture. RP81 must remain the existing root port.
    let dir =
        std::env::temp_dir().join(std::format!("fstart-zeppelin-dsdt-{}", std::process::id()));
    let output = std::process::Command::new("acpiexec")
        .current_dir(dir)
        .args([
            "-dt",
            "-b",
            r"execute \_SB.PCI0.RP81.D000._ADR;execute \_SB.PCI0.RP81.D000._ROM 0 8",
            "dsdt.aml",
        ])
        .output();
    let output = match output {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::eprintln!("acpiexec not found, skipping the DSDT execution");
            return;
        }
        output => output.unwrap(),
    };
    let log = std::string::String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && !log.contains("AE_") && log.contains("55 AA 01 CB 00 00 00 00"),
        "{log}"
    );
}

/// Two dies as NUMA nodes: SRAT, MSCT, SLIT and the root bridges'
/// proximity.
#[test]
fn two_dies_are_two_domains() {
    let (dies, cpus) = (dies(true), cpus(&[0, 1, 0x10, 0x11], true));
    assert_eq!(
        cpus.iter().map(|c| c.domain).collect::<Vec<_>>(),
        [0, 1, 0, 1]
    );
    let memory = [
        MemoryAffinity {
            base: 0,
            end: 0x8000_0000,
            domain: 0,
        },
        MemoryAffinity {
            base: 0x1_0000_0000,
            end: 0x2_0000_0000,
            domain: 1,
        },
    ];
    let board = Board::default();
    let tables = build(&Inputs {
        numa: Some(Numa {
            domains: &[Domain { socket: 0 }; 2],
            memory: &memory,
            phys_addr_bits: 48,
        }),
        ..inputs(&ACPI, &cpus, &dies, &board)
    })
    .unwrap();
    let signatures: Vec<_> = tables.extra.iter().map(|t| &t[..4]).collect();
    assert_eq!(&signatures[1..4], [b"SRAT", b"MSCT", b"SLIT"]);
    let slit = &tables.extra[3];
    assert_eq!(&slit[44..48], &[10, 16, 16, 10]);
    assert_eq!(tables.dsdt.windows(4).filter(|w| w == b"_PXM").count(), 2);
}

/// The table set of an MJ11-EC1 (EPYC 3151, one die) as the stage builds
/// it, byte for byte: the windows the enumeration finds, the board's
/// COM1, IPMI KCS and SPCR.
#[test]
fn mj11_ec1_tables_are_pinned() {
    let io = 0x1000..=0xffff;
    let mmio = [0xe000_0000..=0xe7ff_ffff, 0xf800_0000..=0xfe7f_ffff];
    let die = [root_bridge(
        0,
        pci_root(0..=127, &io, &mmio),
        0xfeb0_0000,
        Some(Iommu {
            base: 0xfe60_0000,
            capability: 0x1909_0040,
            range: 0x0000_ff00,
            misc: 0x0000_1a0b,
            control: 0,
            efr: 0x0000_00a2_9ec4_a7a5,
            efr2: 0,
            counter_config: 0x0000_0000_0002_0402,
        }),
        false,
    )];
    // The board's: the KCS interface, SPCR on COM1, the BMC sleep-state
    // SMIs.
    let mut spcr = vec![];
    fstart_acpi::Aml::to_aml_bytes(&fstart_acpi::spcr::build_spcr_16550_io(0x3f8, 4), &mut spcr);
    let board = Board {
        ipmi: Some(fstart_acpi::devices::IpmiKcsAcpi {
            port: 0xca2,
            spacing: 1,
            uid: 0,
            spec_revision: 0x0200,
        }),
        tables: vec![spcr],
        sb_aml: fstart_acpi::devices::IpmiKcsAcpi::sleep_notify_aml(0xb2, 0xc0),
        ..Default::default()
    };
    let cpus = cpus(&[0, 1, 2, 3, 8, 9, 10, 11], false);
    let tables = build(&inputs(&ACPI, &cpus, &die, &board)).unwrap();
    let mut storage = vec![0; 0x2_0000];
    let len = tables.assemble_into(&mut storage, 0x7601_8000).unwrap();
    let hash = storage[..len]
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
            (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    assert_eq!((len, hash), (5936, 0xf1b6_f2f2_e936_8d23));
}

/// The table with `signature`: the first match whose length and
/// checksum hold (the DSDT names devices like tables).
fn signature<'a>(tables: &'a [u8], signature: &[u8; 4]) -> &'a [u8] {
    (0..tables.len().saturating_sub(8))
        .filter(|&at| &tables[at..at + 4] == signature)
        .find_map(|at| {
            let len = u32::from_le_bytes(tables[at + 4..at + 8].try_into().unwrap()) as usize;
            let table = tables.get(at..at.checked_add(len)?)?;
            (len >= 36 && table.iter().fold(0u8, |sum, &b| sum.wrapping_add(b)) == 0)
                .then_some(table)
        })
        .unwrap()
}

fn iasl_disassemble(dsdt: &[u8]) -> std::io::Result<std::string::String> {
    let dir =
        std::env::temp_dir().join(std::format!("fstart-zeppelin-dsdt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("dsdt.aml"), dsdt)?;
    let output = std::process::Command::new("iasl")
        .current_dir(&dir)
        .args(["-d", "dsdt.aml"])
        .output()?;
    let log = std::string::String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && !log.contains("Error"),
        "iasl: {log}"
    );
    std::fs::read_to_string(dir.join("dsdt.dsl"))
}
