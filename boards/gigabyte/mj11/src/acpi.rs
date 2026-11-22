// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's ACPI devices behind the LPC bridge: the AST2500 SUART1 as
//! COM1 (also the SPCR console, the BMC's SOL) and the BMC's IPMI KCS
//! interface with its SPMI table and sleep state reports. The LPC TPM isn't
//! described.
use alloc::vec;
use fstart_acpi::devices::IpmiKcsAcpi;
use fstart_driver_ipmi::kcs;
use fstart_driver_superio::{ComPortConfig, SuperIoConfig, superio_dsdt_aml};
use fstart_platform_amd::acpi::Board;

// The AST2500 firmware implements IPMI 2.0.
const KCS: IpmiKcsAcpi = kcs::DEFAULT_ACPI;

pub fn tables() -> Board {
    let com1 = SuperIoConfig {
        com1: Some(ComPortConfig {
            io_base: super::COM1,
            irq: super::COM1_IRQ,
            baud_rate: 115_200,
        }),
        ..SuperIoConfig::default()
    };
    let mut spcr = alloc::vec::Vec::new();
    fstart_acpi::Aml::to_aml_bytes(
        &fstart_acpi::spcr::build_spcr_16550_io(super::COM1, super::COM1_IRQ),
        &mut spcr,
    );
    Board {
        ipmi: Some(KCS),
        lpc_aml: superio_dsdt_aml(&com1),
        // The console: SOL over COM1.
        tables: vec![spcr],
        ..Board::default()
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use fstart_core::acpi::AcpiSmiConfig;
    use fstart_driver_amd::ccx::power;
    use fstart_driver_amd::fch::smi;
    use fstart_driver_amd::nbio::iommu::Iommu;
    use fstart_pci::PciRole::{Forwarded, Own, Window};
    use fstart_pci::{PciRoot, PciRootInfo};
    use fstart_platform_amd::acpi::{Inputs, build};
    use fstart_platform_amd::board::Board as _;
    use fstart_platform_amd::noncar::acpi::{FCH_IOAPIC_ID, cpus, root_bridge};

    /// The table set of an MJ11-EC1 (EPYC 3151, one die) as the stage
    /// builds it, byte for byte: the windows the enumeration finds, the
    /// board's COM1, IPMI KCS, SPCR and BMC sleep-state SMIs.
    #[test]
    fn ec1_tables_are_pinned() {
        let pci = PciRootInfo {
            segment: 0,
            ecam_base: 0xf000_0000,
            bus_start: 0,
            bus_end: 127,
        };
        // The die has the legacy I/O and VGA memory routed to it and owns
        // the ECAM window; PCI devices get the ports above the legacy ones.
        let root = PciRoot::new(
            pci,
            [
                Forwarded.io(0, 0x1000),
                Window.io(0x1000, 0xf000),
                Forwarded.mmio(0xa_0000, 0x2_0000),
                Window.mmio(0xe000_0000, 0x0800_0000),
                Window.mmio(0xf800_0000, 0x0680_0000),
                Own.mmio(0xf000_0000, 128 << 20),
            ],
        )
        .unwrap();
        let die = [root_bridge(
            0,
            &root,
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
        // 2.7, 1.9 and 1.2 GHz.
        let pstates = [(0, 0x6c, 8), (1, 0x98, 0x10), (2, 0x60, 0x10)].map(|(i, fid, did)| {
            power::decode_pstate(i, 1 << 63 | 0x50 << 22 | 0x40 << 14 | did << 8 | fid).unwrap()
        });
        let cpus = cpus(&[0, 1, 2, 3, 8, 9, 10, 11], false);
        let board = super::tables();
        let tables = build(&Inputs {
            soc: &fstart_platform_amd::zeppelin::acpi::ACPI,
            pci,
            cpus: &cpus,
            fch_ioapic_id: FCH_IOAPIC_ID,
            hpet_block_id: 0x1022_8201,
            root_bridges: &die,
            numa: None,
            power: Some(power::Power {
                pstates: &pstates,
                cstates: Some(power::CstateInfo {
                    io_base: 0x813,
                    cc6: true,
                }),
                threads_per_core: 2,
                c2_latency_us: 400,
            }),
            acpi_smi: Some(AcpiSmiConfig {
                smi_cmd: smi::APM_CNT.into(),
                acpi_enable: smi::ACPI_ENABLE,
                acpi_disable: smi::ACPI_DISABLE,
            }),
            bmc_sx_notify: crate::Board::BMC_SX_NOTIFY,
            tpm: None,
            devices: &[],
            ivrs_devices: &[],
            sb_aml: &[],
            tables: &[],
            apei: None,
            board: &board,
        })
        .unwrap();
        let mut storage = std::vec![0; 0x2_0000];
        let len = tables.assemble_into(&mut storage, 0x7601_8000).unwrap();
        let hash = storage[..len]
            .iter()
            .fold(0xcbf2_9ce4_8422_2325u64, |h, &b| {
                (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
            });
        assert_eq!((len, hash), (5936, 0x2b2d_45f2_36af_dfbd));
    }
}
