// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's ACPI devices behind the LPC bridge: the AST2500 SUART1 as
//! COM1 (also the SPCR console, the BMC's SOL) and the BMC's IPMI KCS
//! interface with its SPMI table and sleep state reports. The LPC TPM isn't
//! described.
use alloc::vec;
use fstart_acpi::devices::IpmiKcsAcpi;
use fstart_driver_ipmi::kcs;
use fstart_driver_superio::{ComPortConfig, SuperIoConfig, superio_dsdt_aml};
use fstart_platform_amd::acpi::BoardAcpi;

// The AST2500 firmware implements IPMI 2.0.
const KCS: IpmiKcsAcpi = kcs::DEFAULT_ACPI;

pub fn tables() -> BoardAcpi {
    let com1 = SuperIoConfig {
        com1: Some(ComPortConfig {
            io_base: super::COM1,
            irq: super::COM1_IRQ,
            baud_rate: 115_200,
        }),
        ..SuperIoConfig::default()
    };
    let mut lpc_aml = superio_dsdt_aml(&com1);
    lpc_aml.extend(KCS.dsdt_aml());
    BoardAcpi {
        lpc_aml,
        // The console: SOL over COM1.
        tables: vec![KCS.spmi(), {
            let mut spcr = alloc::vec::Vec::new();
            fstart_acpi::Aml::to_aml_bytes(
                &fstart_acpi::spcr::build_spcr_16550_io(super::COM1, super::COM1_IRQ),
                &mut spcr,
            );
            spcr
        }],
        devices: vec![],
        // `_PTS`/`_WAK` tell the BMC the sleep state through the SMM handler.
        sb_aml: super::BMC_SLEEP_NOTIFY.aml(fstart_driver_amd::fch::smi::APM_CNT),
    }
}
