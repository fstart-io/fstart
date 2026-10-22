// SPDX-License-Identifier: MIT OR Apache-2.0
//! The register writes of the Zeppelin flow, in the order the flow runs its
//! phases, against the host model of the dies. The board hooks write
//! markers, so their places in the sequence are pinned too. MP init is
//! stood in by the SMU's AP release.
extern crate std;
use super::{
    Config, DxioConfig, FchConfig,
    silicon::{self, Dma, Identity, SmuPages, SmuPolicy},
};
use fstart_arch::x86::cpu::msr::testing::MsrSpace;
use fstart_driver_amd::{
    apob::{self, Apob},
    df,
    dxio::{self, Descriptor, SataChannel},
    fch::{Acpimmio, Bus, Fch, Space, taishan::irq},
    smu::{
        self,
        threads::{self, Policy},
        v9,
    },
    soc::{
        testing::{Hw, low},
        zeppelin::{Die, Package, Revision},
    },
};
use fstart_pci::PciAddress;
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
        for (i, word) in [apob::GROUP_MEMORY, kind, instance, (48 + size) as u32]
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
            entry(
                apob::TYPE_MEM_GENERAL_CONFIG,
                die,
                0x10,
                &[(0x38, 1), (0x39, 1)],
            )
        })
        .chain(entry(apob::TYPE_MEM_PIE_PWR, 0, 0x110, &[(0x130, 1)]))
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
fn flow(hw: &Hw, config: &Config, identity: Identity, top: (u64, Option<u64>), dma: usize) {
    let dies: Vec<Die<&Hw>> = unsafe { df::dies(hw) }.collect();
    let fch = Fch::new(hw, Acpimmio::DIE0);
    let msr = MsrSpace::new([]);
    let mut clock = hw.clock();
    let apob = apob(dies.len() as u32);
    let apob = Apob::parse(&apob).unwrap();

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
        config,
        identity,
        top,
        Some(&apob),
        dma,
        &mut clock,
        || marker(hw, 2),
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
}

/// The model's SMU, DF and topology registers of `dies` dies.
fn model(dies: u8) -> Hw {
    let buses: Vec<u8> = (0..dies).map(|die| die * 0x40).collect();
    let hw = Hw::new(
        &buses,
        v9::MAILBOX.arguments.address(),
        v9::DXIO_SERVICE,
        &[(v9::GET_VERSION, 7)],
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
/// interrupt routes and SMI.
#[test]
fn mj11_ec1_flow() {
    let hw = model(1);
    let config = Config {
        subsystem_id: 0x1000_1458,
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
        revision: Revision::B2,
        package: Package::Sp4r2,
    };
    flow(
        &hw,
        &config,
        identity,
        (0x8000_0000, Some(0x4_8000_0000)),
        0x5000_0000,
    );
    assert_eq!(hw.fingerprint(), (9686, 0x2f54_6e5c_18ed_9aad));
    assert_eq!(hw.state(), 0x5ba3_2753_5cdf_f76d);
}

const DIE1: &[Descriptor] = &[pcie(0, 7, 1, 1), pcie(16, 31, 3, 1)];

/// SP4: two dies, the second one with engines and its own FCH.
#[test]
fn sp4_flow() {
    let hw = model(2);
    hw.set_config(PciAddress::new(0, 0x40, 8, 1), 0, 0x1452_1022);
    let config = Config {
        subsystem_id: 0x1000_1458,
        fch: FchConfig {
            irq_routes: ROUTES,
            ..FchConfig::DEFAULT
        },
        dxio: DxioConfig {
            descriptors: [EC1, DIE1],
        },
    };
    let identity = Identity {
        revision: Revision::B2,
        package: Package::Sp4,
    };
    flow(
        &hw,
        &config,
        identity,
        (0xe000_0000, Some(0x4_2000_0000)),
        0x5800_0000,
    );
    assert_eq!(hw.fingerprint(), (17243, 0x54bf_78c5_95a4_3d43));
    assert_eq!(hw.state(), 0x3699_29c4_2d3f_3403);
}
