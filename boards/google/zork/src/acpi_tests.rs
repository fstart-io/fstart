// SPDX-License-Identifier: MIT OR Apache-2.0
//! Vilboz's DSDT as the stage builds it: the ChromeEC under the LPC bridge,
//! the console UART and I2C on their own FCH IRQs, and the EC's I/O not in
//! SYSR. It disassembles with `iasl`, and the EC's methods run in the real
//! namespace in `acpiexec`, when they are installed.
extern crate std;

use fstart_core::acpi::AcpiSmiConfig;
use fstart_driver_amd::fch::smi;
use fstart_pci::PciRole::{Forwarded, Own, Window};
use fstart_pci::{PciRoot, PciRootInfo};
use fstart_platform_amd::acpi::{Inputs, build};
use fstart_platform_amd::noncar::acpi::{FCH_IOAPIC_ID, cpus, root_bridge};
use fstart_platform_amd::picasso;

#[test]
fn vilboz_dsdt_loads() {
    let pci = PciRootInfo {
        segment: 0,
        ecam_base: 0xf000_0000,
        bus_start: 0,
        bus_end: 255,
    };
    let root = PciRoot::new(
        pci,
        [
            Forwarded.io(0, 0x1000),
            Window.io(0x1000, 0xf000),
            Forwarded.mmio(0xa_0000, 0x2_0000),
            Window.mmio(0xe000_0000, 0x1000_0000),
            Own.mmio(0xf000_0000, 256 << 20),
        ],
    )
    .unwrap();
    let root = [root_bridge(0, &root, 0xfec0_1000, None, false)];
    let cpus = cpus(&[0, 1], false);
    let board = crate::acpi();
    let config = &crate::CONFIG;
    let routes = config.routes().unwrap();
    let soc_aml =
        picasso::acpi::devices(config.fch.console_uart, config.i2c, &routes, false).unwrap();
    let tables = build(&Inputs {
        soc: &picasso::acpi::ACPI,
        pci,
        cpus: &cpus,
        fch_ioapic_id: FCH_IOAPIC_ID,
        hpet_block_id: 0x1022_8201,
        root_bridges: &root,
        numa: None,
        power: None,
        acpi_smi: Some(AcpiSmiConfig {
            smi_cmd: smi::APM_CNT.into(),
            acpi_enable: smi::ACPI_ENABLE,
            acpi_disable: smi::ACPI_DISABLE,
        }),
        bmc_sx_notify: None,
        tpm: None,
        devices: &[],
        ivrs_devices: &[],
        sb_aml: &soc_aml,
        tables: &[],
        apei: None,
        board: &board,
    })
    .unwrap();
    let dir = std::env::temp_dir().join(std::format!("fstart-vilboz-dsdt-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("dsdt.aml"),
        fstart_acpi::platform::build_dsdt(&tables.dsdt),
    )
    .unwrap();
    let Some(output) = run(&dir, "iasl", &["-d", "dsdt.aml"]) else {
        return;
    };
    let log = std::string::String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && !log.contains("Error"),
        "iasl: {log}"
    );
    let dsl = std::fs::read_to_string(dir.join("dsdt.dsl")).unwrap();
    for object in [
        "Device (EC0)",
        "Device (CREC)",
        "Device (PS2K)",
        "Device (FUR0)",
        "Name (_HID, \"AMDI0020\")",
    ] {
        assert!(dsl.contains(object), "{object} missing:\n{dsl}");
    }
    let batch = [
        "BAT0._BST",
        "LID0._LID",
        "AC._PSR",
        "CREC.TBMC._STA",
        "_Q1D",
    ]
    .map(|method| std::format!(r"execute \_SB.PCI0.LPCB.EC0.{method}"))
    .join(";");
    let Some(output) = run(
        &dir,
        "acpiexec",
        &["-dt", "-fv", "0", "-b", &batch, "dsdt.aml"],
    ) else {
        return;
    };
    let log = std::string::String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && !log.contains("AE_") && log.matches("Evaluating ").count() == 5,
        "{log}"
    );
}

/// Run `tool` in `dir`; `None` if it isn't installed.
fn run(dir: &std::path::Path, tool: &str, args: &[&str]) -> Option<std::process::Output> {
    match std::process::Command::new(tool)
        .current_dir(dir)
        .args(args)
        .output()
    {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::eprintln!("{tool} not found, skipping");
            None
        }
        output => Some(output.unwrap()),
    }
}
