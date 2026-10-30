// SPDX-License-Identifier: MIT OR Apache-2.0
//! The register writes of the Zeppelin flow, in the order the flow runs its
//! phases, against the host model of the dies. The board hooks write
//! markers, so their places in the sequence are pinned too. MP init is
//! stood in by the SMU's AP release.
extern crate std;
use super::{
    Config, DxioConfig, FchConfig,
    silicon::{self, Dma, Identity, SmuPages, SmuPolicy, State},
};
use crate::{
    acpi::{self, BoardAcpi, DieAcpi, Inputs},
    noncar::{acpi::HostBridge, layout::Region, pci::Root},
};
use fstart_arch::x86::cpu::msr::testing::MsrSpace;
use fstart_driver_amd::cpu::{Package, Revision};
use fstart_driver_amd::{
    apob::{self, Apob},
    df::{self, map},
    dxio::{self, Descriptor, SataChannel},
    fch::{Acpimmio, Bus, Fch, Space, taishan::irq},
    smu::{
        self,
        threads::{self, Policy},
        v9,
    },
    soc::{
        testing::{Hw, low},
        zeppelin::{Die, NONPCI_MMIO_SIZE},
    },
};
use fstart_pci::{PCI_BAR0, PCI_PRIMARY_BUS, PciAddress, PciEcam};
use std::vec::Vec;

const fn pcie(start: u8, end: u8, device: u8, function: u8) -> Descriptor {
    Descriptor {
        turn_off_unused_lanes: true,
        ..Descriptor::pcie(start, end, device, function)
    }
}

/// The MJ11-EC1 engines.
const EC1: &[Descriptor] = &[
    Descriptor {
        turn_off_unused_lanes: true,
        ..Descriptor::sata(0, 3, SataChannel::Other)
    },
    pcie(4, 7, 1, 2),
    pcie(8, 11, 1, 3),
    pcie(12, 13, 1, 4),
    pcie(14, 14, 1, 5),
    pcie(15, 15, 1, 1),
    pcie(31, 24, 3, 1),
    pcie(23, 16, 3, 3),
];

const ROUTES: &[irq::Route] = &[
    irq::Route {
        source: irq::A,
        pic: irq::NC,
        apic: 16,
    },
    irq::Route {
        source: irq::SCI,
        pic: 9,
        apic: 9,
    },
];

/// An APOB with the populated channels of every die and the PIE power flag.
fn apob(dies: u32) -> Vec<u8> {
    let entry = |kind: u32, instance: u32, size: usize, bytes: &[(usize, u8)]| {
        let mut entry = std::vec![0; 48 + size];
        for (i, word) in [apob::group::MEMORY, kind, instance, (48 + size) as u32]
            .into_iter()
            .enumerate()
        {
            entry[4 * i..4 * i + 4].copy_from_slice(&word.to_le_bytes());
        }
        for &(offset, byte) in bytes {
            entry[offset] = byte;
        }
        entry
    };
    let entries: Vec<u8> = (0..dies)
        .flat_map(|die| {
            // 1/2: the memory configuration.
            entry(2, die, 0x10, &[(0x38, 1), (0x39, 1)])
        })
        // 1/0xf: the PIE power settings.
        .chain(entry(0xf, 0, 0x110, &[(0x130, 1)]))
        .collect();
    let size = (16 + entries.len()) as u32;
    [0x424f_5041, 9, size, 16]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .chain(entries)
        .collect()
}

/// A board hook's marker in the log.
fn marker(hw: &Hw, code: u8) {
    hw.write(Space::Io, 0xe0, 1, code.into());
}

/// The register writes from the FCH pre-init to the ReadyToBoot of the
/// silicon, in the flow's order. `dma` is a free 2 MiB-aligned address
/// below 4 GiB.
fn flow(
    hw: &Hw,
    config: &Config,
    identity: Identity,
    top: (u64, Option<u64>),
    dma: usize,
) -> State {
    let dies: Vec<Die<&Hw>> = unsafe { df::dies(hw) }.collect();
    let fch = Fch::new(hw, Acpimmio::DIE0);
    let msr = MsrSpace::new([]);
    let mut clock = hw.clock();
    let apob = apob(dies.len() as u32);
    let apob = Apob::new(&apob, fstart_driver_amd::psp::Variant::Zp).unwrap();

    // Entry
    silicon::post(&fch, 0x30);
    silicon::fch_pre_init(&fch).unwrap();
    marker(hw, 1); // early_init
    silicon::post(&fch, 0x31);
    silicon::fch_early_init(&fch, config);
    silicon::post(&fch, 0x32);

    // TP1
    let dma = Dma {
        smu_pages: low(dma, SmuPages([[0; 4096]; 4])),
        smu_policy: low(dma + 0x1_0000, SmuPolicy([0; 0x80])),
        dxio: low(dma + 0x2_0000, dxio::zeppelin::Buffers::new()),
    };
    let state = silicon::pre_mp(
        &dies,
        &fch,
        &msr,
        &mut { *config },
        identity,
        top,
        Some(&apob),
        dma,
        &mut clock,
        |_| marker(hw, 2),
        || marker(hw, 3),
    )
    .unwrap();
    assert_eq!(state.ioapics[0], state.nonpci[0] + (7 << 20));
    let threads = threads::thread_count(&dies, Policy::DEFAULT).unwrap();
    unsafe { threads::launch(&dies, Policy::DEFAULT, threads - 1, &mut clock) }.unwrap();
    silicon::cpus_up(&dies, &mut clock);
    silicon::post(&fch, 0x42);
    silicon::route_interrupts(&fch, config).unwrap();

    // TP2
    silicon::after_resources(&fch);
    marker(hw, 4); // mainboard_final
    fch.enable_hpet_ioapic();

    // TP3
    let sata = PciAddress::new(0, 1, 0, 0);
    silicon::ready_to_boot(&dies, &fch, Some(sata), || marker(hw, 5));
    silicon::post(&fch, 0x43);
    silicon::post(&fch, 0x44);
    state
}

/// The model's SMU, DF and topology registers of `dies` dies.
fn model(dies: u8) -> Hw {
    let buses: Vec<u8> = (0..dies).map(|die| die * 0x40).collect();
    let hw = Hw::new(
        &buses,
        v9::MAILBOX.arguments.address(),
        v9::msg::DXIO,
        &[(v9::msg::GET_VERSION, 7)],
    );
    let df = |node: u8, function| PciAddress::new(0, 0, 0x18 + node, function);
    hw.set_config(df(0, 1), 0x204, dies.into());
    for (node, &bus) in buses.iter().enumerate() {
        hw.set_config(df(node as u8, 0), 0x84, bus.into());
    }
    hw.set_smn(0, smu::THREAD_CONFIG, 0x013);
    hw.set_smn(0, smu::THREAD_ENABLE, 1);
    if dies > 1 {
        hw.set_smn(1, smu::THREAD_CONFIG, 0x103);
        hw.set_smn(1, smu::THREAD_ENABLE, 0);
    }
    hw.set_smn(0, 0x13f0_0040_u32, 0x1964_000f);
    // The FCH LPC bridge.
    hw.set_fch(Space::Lpc, 0, 4, 0x790e_1022);
    hw
}

/// MJ11-EC1: an EPYC 3151 (SP4r2, one die) with the board's engines,
/// interrupt routes and SMI. `dma` as [`flow`]'s, one per test.
fn mj11_ec1_flow_on(hw: &Hw, dma: usize) -> State {
    let config = Config {
        subsystem_id: 0x1000_1458,
        ccx: super::CcxConfig::DEFAULT,
        fch: FchConfig {
            irq_routes: ROUTES,
            smi_gevents: 1 << 11,
            ..FchConfig::DEFAULT
        },
        dxio: DxioConfig {
            descriptors: [EC1, &[]],
        },
    };
    let identity = Identity {
        revision: Revision::ZpB2,
        package: Package::Sp4r2,
    };
    flow(
        hw,
        &config,
        identity,
        (0x8000_0000, Some(0x4_8000_0000)),
        dma,
    )
}

#[test]
fn mj11_ec1_flow() {
    let hw = model(1);
    mj11_ec1_flow_on(&hw, 0x5000_0000);
    assert_eq!(hw.fingerprint(), (9686, 0x2e5b_9a12_d620_85bd));
    assert_eq!(hw.state(), 0x5ba3_2753_5cdf_f76d);
}

/// The single die's PCI root is the whole ECAM with the DF's windows below
/// 4 GiB, without the ECAM and the non-PCI pool: what the hardware was
/// tested with.
#[test]
fn mj11_ec1_pci_root() {
    let hw = model(1);
    // The PSP's bus map: every bus to die 0.
    hw.set_config(PciAddress::new(0, 0, 0x18, 0), 0xa0, 0xff00_0043);
    let state = mj11_ec1_flow_on(&hw, 0x6800_0000);
    let die: Die<&Hw> = unsafe { df::dies(&hw) }.next().unwrap();
    let ecam = Region::new(0xf000_0000, 128 << 20);
    let reserved = Region::new(state.nonpci[0].into(), NONPCI_MMIO_SIZE.into());
    let root = Root::new(&map::root_complex(&die).unwrap(), ecam, reserved).unwrap();
    assert_eq!(root.buses(), 0..=127);
    assert!(root.io().eq(core::iter::once(0x1000..=0xffff)));
    assert_eq!(
        root.mmio().collect::<Vec<_>>(),
        [0xf800_0000..=0xfe3f_ffff, 0x8000_0000..=0xefff_ffff]
    );
}

const DIE1: &[Descriptor] = &[pcie(0, 7, 1, 1), pcie(16, 31, 3, 1)];

/// SP4: two dies, the second one with engines and its own FCH. `dma` as
/// [`flow`]'s, one per test.
fn sp4_flow_on(hw: &Hw, dma: usize) -> State {
    hw.set_config(PciAddress::new(0, 0x40, 8, 1), 0, 0x1452_1022);
    let config = Config {
        subsystem_id: 0x1000_1458,
        ccx: super::CcxConfig::DEFAULT,
        fch: FchConfig {
            irq_routes: ROUTES,
            ..FchConfig::DEFAULT
        },
        dxio: DxioConfig {
            descriptors: [EC1, DIE1],
        },
    };
    let identity = Identity {
        revision: Revision::ZpB2,
        package: Package::Sp4,
    };
    flow(
        hw,
        &config,
        identity,
        (0xe000_0000, Some(0x4_2000_0000)),
        dma,
    )
}

#[test]
fn sp4_flow() {
    let hw = model(2);
    sp4_flow_on(&hw, 0x5800_0000);
    assert_eq!(hw.fingerprint(), (17243, 0xed55_2202_b2b9_8567));
    assert_eq!(hw.state(), 0x3699_29c4_2d3f_3403);
}

/// The address space descriptors (type, min, max) in `aml`: buses (2), I/O
/// (1) and memory (0).
fn address_spaces(aml: &[u8]) -> Vec<(u8, u64, u64)> {
    let field = |bytes: &[u8]| {
        let mut word = [0; 8];
        word[..bytes.len()].copy_from_slice(bytes);
        u64::from_le_bytes(word)
    };
    (0..aml.len().saturating_sub(5))
        .filter_map(|at| {
            let width = match aml[at..at + 3] {
                [0x88, 0x0d, 0] => 2,
                [0x87, 0x17, 0] => 4,
                _ => return None,
            };
            let min = at + 6 + width;
            let max = min + width;
            (aml[at + 4] == 0x0c).then(|| {
                let (min, max) = (field(&aml[min..max]), field(&aml[max..max + width]));
                (aml[at + 3], min, max)
            })
        })
        .collect()
}

/// SP4 enumerates and allocates below each die from what the DF routes to
/// it, and has an ACPI host bridge per die with those buses and windows.
#[test]
fn sp4_pci_roots_and_host_bridges() {
    let hw = model(2);
    // The PSP's bus maps, the last one up to bus 0xff.
    for node in 0..2 {
        let df = PciAddress::new(0, 0, 0x18 + node, 0);
        hw.set_config(df, 0xa0, 0x3f00_0043);
        hw.set_config(df, 0xa4, 0xff40_0243);
    }
    let state = sp4_flow_on(&hw, 0x6000_0000);
    let dies: Vec<Die<&Hw>> = unsafe { df::dies(&hw) }.collect();

    // Test RAM for the 128-bus ECAM: the GPP bridge 1.1 on each root bus
    // with an NVMe drive behind; every BAR sizes as 16 bytes of 32-bit memory.
    let mut ecam = std::vec![0u32; 128 << 18];
    let config = |bus: usize, devfn: usize, offset: usize| (bus << 18) + (devfn << 10) + offset / 4;
    for bus in [0, 1, 0x40, 0x41] {
        (0..32).for_each(|device| ecam[config(bus, device << 3, 0)] = u32::MAX);
    }
    for root in [0, 0x40] {
        // (bus, devfn, header type, class)
        for (bus, devfn, header, class) in [
            (root, 0x08, 0x80, 0x0600),
            (root, 0x09, 1, 0x0604),
            (root + 1, 0, 0, 0x0108),
        ] {
            ecam[config(bus, devfn, 0)] = 0x1453_1022;
            ecam[config(bus, devfn, 8)] = class << 16;
            ecam[config(bus, devfn, 0xc)] = header << 16;
        }
    }
    // The test RAM is above 4 GiB; the real ECAM is between the dies' windows.
    let ecam_region = Region::new(ecam.as_mut_ptr() as u64, 128 << 20);
    let roots: Vec<Root> = dies
        .iter()
        .map(|die| {
            let pool = state.nonpci[usize::from(die.index())];
            let reserved = Region::new(pool.into(), NONPCI_MMIO_SIZE.into());
            Root::new(&map::root_complex(die).unwrap(), ecam_region, reserved).unwrap()
        })
        .collect();
    let mut pci = PciEcam::from_providers(&roots).unwrap();
    pci.enumerate_and_allocate().unwrap();
    let read = |address, offset| pci.device(address).unwrap().read32(offset);

    let buses = |bus| read(PciAddress::new(0, bus, 1, 1), PCI_PRIMARY_BUS) & 0xff_ffff;
    assert_eq!((buses(0), buses(0x40)), (0x01_0100, 0x41_4140));
    for (root, endpoint) in roots.iter().zip([1, 0x41]) {
        let bar = u64::from(read(PciAddress::new(0, endpoint, 0, 0), PCI_BAR0) & !0xf);
        assert!(root.mmio().any(|window| window.contains(&bar)), "{bar:#x}");
    }

    let hosts: Vec<HostBridge> = roots
        .iter()
        .map(|root| HostBridge::new(root, &pci))
        .collect();
    let dies: Vec<DieAcpi> = hosts
        .iter()
        .enumerate()
        .map(|(die, host)| host.acpi(die as u8, state.ioapics[die], None))
        .collect();
    let board = BoardAcpi::default();
    let tables = acpi::build(&Inputs {
        soc: &super::acpi::ACPI,
        apic_ids: &[0, 1, 8, 9],
        dies: &dies,
        ecam_base: 0xf000_0000,
        ecam_buses: 128,
        hpet_id: 0,
        power: None,
        numa: None,
        board: &board,
    })
    .unwrap();
    let aml = &tables.dsdt_aml;
    let pci1 = aml.windows(4).position(|name| name == b"PCI1").unwrap();
    let vga = (0, 0xa_0000, 0xb_ffff);
    assert_eq!(
        address_spaces(&aml[..pci1]),
        [
            (2, 0, 0x3f),
            (1, 0x1000, 0x7fff),
            (0, 0xe800_0000, 0xef7f_ffff),
            (0, 0xf800_0000, 0xfebf_ffff),
            vga
        ]
    );
    assert_eq!(
        address_spaces(&aml[pci1..]),
        [
            (2, 0x40, 0x7f),
            (1, 0x8000, 0xffff),
            (0, 0xe000_0000, 0xe77f_ffff)
        ]
    );
}
