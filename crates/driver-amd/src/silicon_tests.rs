// SPDX-License-Identifier: MIT OR Apache-2.0
//! Whole-sequence write logs of the Family 17h silicon init against the
//! host model of the dies ([`crate::soc::testing`]). The fingerprints pin
//! the sequences the hardware was tested with.
extern crate std;
use crate::soc::testing::*;
use fstart_pci::PciAddress;
use std::vec::Vec;


#[cfg(all(feature = "zeppelin", target_arch = "x86_64", target_os = "linux"))]
mod zeppelin {
    use super::*;
    use crate::{
        df::{self, map},
        dxio::{self, Descriptor, SataChannel},
        fch::{
            Acpimmio, Fch,
            taishan::{self, Devices},
        },
        nbio::{
            self, iommu,
            iohc::{self, IOAPIC_BAR},
        },
        psp,
        smu::{
            threads::{self, Policy},
            v9 as smu,
        },
        soc::zeppelin::{Die, NONPCI_IOAPIC, NONPCI_IOMMU, NONPCI_PSP, Package, Revision},
    };

    const fn pcie(start: u8, end: u8, device: u8, function: u8) -> Descriptor {
        Descriptor {
            turn_off_unused_lanes: true,
            ..Descriptor::pcie(start, end, device, function)
        }
    }
    const DIE0: &[Descriptor] = &[
        Descriptor::sata(0, 3, SataChannel::Other),
        pcie(4, 7, 1, 2),
        pcie(8, 11, 1, 3),
        pcie(12, 13, 1, 4),
        pcie(14, 14, 1, 5),
        pcie(15, 15, 1, 1),
        pcie(31, 24, 3, 1),
        pcie(23, 16, 3, 3),
    ];
    const DIE1: &[Descriptor] = &[pcie(0, 7, 1, 1), pcie(16, 31, 3, 1)];
    const SUBSYSTEM_ID: u32 = 0x1000_1458;
    const DEVICES: [Devices; 2] = [
        Devices {
            xhci: true,
            sata: true,
        },
        Devices {
            xhci: true,
            sata: true,
        },
    ];

    fn sp4() -> Hw {
        let hw = Hw::new(
            &[0, 0x40],
            smu::MAILBOX.arguments.address(),
            smu::DXIO_SERVICE,
            &[(smu::GET_VERSION, 7)],
        );
        let df = |node: u8, function| PciAddress::new(0, 0, 0x18 + node, function);
        hw.set_config(df(0, 1), 0x204, 2);
        hw.set_config(df(0, 0), 0x84, 0);
        hw.set_config(df(1, 0), 0x84, 0x40);
        hw.set_smn(0, crate::smu::THREAD_CONFIG, 0x013);
        hw.set_smn(1, crate::smu::THREAD_CONFIG, 0x103);
        hw.set_smn(0, crate::smu::THREAD_ENABLE, 1);
        hw.set_smn(1, crate::smu::THREAD_ENABLE, 0);
        hw.set_smn(0, 0x13f0_0040_u32, 0x1964_000f);
        // Die 1's internal bridge 8.1 exists.
        hw.set_config(PciAddress::new(0, 0x40, 8, 1), 0, 0x1452_1022);
        hw
    }

    #[test]
    fn sp4_silicon_init_sequence() {
        let hw = sp4();
        let pci = &hw;
        let dies: Vec<Die<&Hw>> = unsafe { df::dies(pci) }.collect();
        let mut delay = hw.clock();

        // Before the PCIe training
        let plan = map::plan(0xe000_0000, 0x4_2000_0000, 128 << 20, 2).unwrap();
        map::program(&plan, &dies);
        let nonpci: Vec<u32> = dies
            .iter()
            .map(|die| df::zeppelin::nonpci_mmio_base(&dies[0].df(), die.index()).unwrap())
            .collect();
        assert_eq!(
            map::mmio_windows(&dies[0].df(), dies[1].ioms_fabric_id()).len(),
            2
        );
        for die in &dies {
            df::zeppelin::early_init(&die.df(), 2, Revision::B2);
        }
        let fch0 = Fch::new(pci, Acpimmio::DIE0);
        taishan::reset_init(&fch0, Revision::B2);
        taishan::sata::reset(&dies[0].smn(), &mut delay);
        taishan::multi::reset(
            pci,
            &dies,
            Package::Sp4,
            |d| nonpci[usize::from(d)],
            &mut delay,
        );
        for die in &dies {
            nbio::zeppelin::early_init(die, Revision::B2);
            let _ = iohc::map_psp(&die.smn(), nonpci[usize::from(die.index())] + NONPCI_PSP);
        }
        smu::print_name(&dies[0], &mut delay);
        for die in &dies {
            iohc::dram_map(die, 0xe000_0000, Some(0x4_2000_0000));
        }

        // PCIe training and after
        let ioapics: Vec<u32> = dies
            .iter()
            .map(|die| {
                let nonpci = nonpci[usize::from(die.index())];
                iommu::init_bar(die, nonpci + NONPCI_IOMMU);
                IOAPIC_BAR.map(&die.smn(), nonpci + NONPCI_IOAPIC);
                nonpci + NONPCI_IOAPIC
            })
            .collect();
        assert_eq!(ioapics[0], nonpci[0] + (7 << 20));
        assert_eq!(psp::bars(&dies), [nonpci[0], nonpci[1]]);
        let buffers = low(0x4000_0000, dxio::zeppelin::Buffers::new());
        let topology = unsafe {
            dxio::zeppelin::init(&dies, &[DIE0, DIE1], SUBSYSTEM_ID, buffers, &mut delay)
        }
        .unwrap();
        let trained = |die: usize| {
            topology[die]
                .ports
                .iter()
                .flatten()
                .filter(|p| p.trained)
                .count()
        };
        assert_eq!(topology[0].ports.iter().flatten().count(), 7);
        assert!(trained(0) > 0 && trained(0) < 7);
        assert_eq!(topology[1].ports.iter().flatten().count(), 2);
        taishan::usb::init(&dies[0].smn(), 0, Package::Sp4, &mut delay);
        taishan::multi::usb(&dies, &DEVICES, Package::Sp4, &mut delay);
        let buffers = smu::DmaBuffers {
            policy: 0x7000_0040,
            agm_log: [0x7000_1000, 0x7000_2000],
            perfmon: [0x7000_3000, 0x7000_4000],
        };
        unsafe { smu::init(&dies, &buffers, &mut delay) }.unwrap();
        smu::cac_weights(&dies[0], &mut delay).unwrap();
        assert_eq!(df::zeppelin::sockets(&dies[0].df()), 2);
        for die in &dies {
            map::route_vga(&die.df(), dies[0].ioms_fabric_id());
        }
        for die in &dies {
            nbio::zeppelin::pre_enumeration_init(die);
        }
        taishan::env_init(&fch0, &dies[0].smn(), SUBSYSTEM_ID);
        taishan::sata::init(&fch0, 0, &dies[0].smn(), SUBSYSTEM_ID, &mut delay);
        taishan::multi::xgbe_init(&dies[0].smn());
        taishan::multi::env(
            pci,
            &dies,
            &DEVICES,
            |d| nonpci[usize::from(d)],
            SUBSYSTEM_ID,
            &mut delay,
        );

        // MP
        let threads = threads::thread_count(&dies, Policy::DEFAULT).unwrap();
        assert_eq!(threads, 20);
        unsafe { threads::launch(&dies, Policy::DEFAULT, threads - 1, &mut delay) }.unwrap();
        smu::aps_ready(&dies, &mut delay).unwrap();
        smu::ccx_done(&dies, &mut delay).unwrap();

        // ReadyToBoot
        taishan::multi::late(&dies, |die| {
            (die == 1).then(|| PciAddress::new(0, 0x40, 8, 2))
        });
        taishan::late_init(&fch0);
        for die in &dies {
            df::zeppelin::disable_ext_cfg(&die.df());
        }
        assert_eq!(hw.fingerprint(), (16906, 0x9dd1_7783_e797_1b5e));
        assert_eq!(hw.state(), 0xbc50_f84e_0701_a314);
    }

    /// The DF routes disjoint buses and windows to the two root complexes.
    #[test]
    fn sp4_root_complexes() {
        let hw = sp4();
        let dies: Vec<Die<&Hw>> = unsafe { df::dies(&hw) }.collect();
        // The PSP's bus maps, the last one up to bus 0xff.
        for node in 0..2 {
            let df = PciAddress::new(0, 0, 0x18 + node, 0);
            hw.set_config(df, 0xa0, 0x3f00_0043);
            hw.set_config(df, 0xa4, 0xff40_0243);
        }
        let plan = map::plan(0xe000_0000, 0x4_2000_0000, 128 << 20, 2).unwrap();
        map::program(&plan, &dies);

        let [die0, die1] = [&dies[0], &dies[1]].map(|die| map::root_complex(die).unwrap());
        let hpet = (0xfed0_0000, 0x1_0000);
        let high = 0x100_0000_0000;
        assert_eq!(die0.buses, 0..=0x3f);
        assert_eq!(
            die0.mmio,
            [
                (0xe800_0000, 0x0800_0000),
                (high, 0x37e_8000_0000),
                (0xf800_0000, 0x06c0_0000),
                hpet
            ]
        );
        assert_eq!(die0.io, [(0, 0x8000)]);
        assert_eq!(die1.buses, 0x40..=0xff);
        assert_eq!(
            die1.mmio,
            [
                (0xe000_0000, 0x0800_0000),
                (high + 0x37e_8000_0000, (1 << 48) - high - 0x37e_8000_0000)
            ]
        );
        assert_eq!(die1.io, [(0x8000, 0x1ff_8000)]);
    }
}

#[cfg(all(feature = "picasso", target_arch = "x86_64", target_os = "linux"))]
mod picasso {
    use super::*;
    use crate::{
        df::{self, picasso as fabric},
        dxio::{
            self, Aspm, ClockRequest, Descriptor,
            picasso::{Options, PowerPolicy},
        },
        nbio::{
            iohc::{PSP_BAR, SMU_BAR},
            picasso as nbio,
        },
        soc::picasso::Die,
        smu::{
            THREAD_CONFIG, THREAD_ENABLE,
            threads::{self, Downcore, Policy},
            v10 as smu,
        },
    };

    const fn pcie(first: u8, last: u8, function: u8, clock: ClockRequest) -> Descriptor {
        let mut descriptor = Descriptor::pcie(first, last, 1, function);
        descriptor.aspm = Aspm::L1;
        descriptor.clock_request = clock;
        descriptor.turn_off_unused_lanes = true;
        descriptor
    }
    const DALBOZ: &[Descriptor] = &[
        pcie(4, 5, 7, ClockRequest::Pin2),
        pcie(0, 0, 2, ClockRequest::Pin0),
        pcie(1, 1, 3, ClockRequest::Pin1),
    ];

    #[test]
    fn picasso_silicon_init_sequence() {
        let hw = Hw::new(&[0], smu::ARGUMENTS.address(), smu::DXIO_SERVICE, &[]);
        hw.set_smn(0, THREAD_CONFIG, 0x13);
        hw.set_smn(0, THREAD_ENABLE, 1);
        hw.set_smn(0, SMU_BAR.low, 0);
        hw.set_smn(0, SMU_BAR.high, 0);
        hw.set_smn(0, PSP_BAR.low, 0);
        hw.set_smn(0, crate::nbio::pcie::link_status(0), 0);
        // TOM2[40:32] bit 8 clear: below 1 TiB.
        hw.set_smn(0, 0x13b1_0068_u32, 0x1234_5600);
        let die: Die<&Hw> = unsafe { df::dies(&hw) }.next().unwrap();
        let (smn, df) = (die.smn(), die.df());
        let mut delay = hw.clock();

        fabric::early_init(&df);
        fabric::late_init(&df, true);
        nbio::early_init(&smn, true, false, &mut delay).unwrap();
        nbio::psp_bar(&smn).unwrap();
        assert!(nbio::psp_bar_installed(&smn));
        nbio::dram_routes(&die, 0xd000_0000, Some(0x4_3000_0000)).unwrap();
        nbio::ioapic_decode(&smn);

        let policy = Policy {
            downcore: Downcore::Two,
            disable_smt: true,
        };
        assert_eq!(
            threads::apply(&die, policy, &mut delay).unwrap(),
            threads::Effect::ResetRequired
        );
        unsafe { threads::launch(&[die], policy, 3, &mut delay) }.unwrap();

        let options = Options {
            power: PowerPolicy::Performance,
            am4: false,
            silicon_id: 0x8000_0000,
            graphics: true,
            hda_enabled: false,
            acp_enabled: true,
        };
        let buffers = low(0x4800_0000, dxio::picasso::Buffers::new());
        let ports =
            unsafe { dxio::picasso::train(&die, buffers, DALBOZ, options, &mut delay) }.unwrap();
        assert_eq!(
            ports
                .ports
                .iter()
                .filter(|p| p.descriptor.is_some())
                .count(),
            3
        );
        assert_eq!(ports.ports.iter().filter(|p| p.trained).count(), 2);
        // The final register state is what the hardware was tested with,
        // the log follows the shared Family 17h sequences.
        assert_eq!(hw.state(), 0xc446_ef2a_df4d_6ca4);
        assert_eq!(hw.fingerprint(), (3489, 0xe499_1a53_e5fd_a388));
    }
}
