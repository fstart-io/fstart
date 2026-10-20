// SPDX-License-Identifier: MIT OR Apache-2.0
//! The board's ACPI devices behind the LPC bridge: the AST2500 SUART1 as
//! COM1 and the BMC's IPMI KCS interface with its SPMI table. The LPC TPM
//! isn't described.
use alloc::vec;
use fstart_acpi::devices::IpmiKcsAcpi;
use fstart_driver_superio::{ComPortConfig, SuperIoConfig, superio_dsdt_aml};
use fstart_platform_amd::zeppelin::BoardAcpi;

/// The BMC KCS data port; status/command is the next one.
pub const KCS_PORT: u16 = 0xca2;

const KCS: IpmiKcsAcpi = IpmiKcsAcpi {
    port: KCS_PORT,
    spacing: 1,
    uid: 0,
    // The AST2500 firmware implements IPMI 2.0.
    spec_revision: 0x0200,
};

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
        tables: vec![KCS.spmi()],
    }
}
