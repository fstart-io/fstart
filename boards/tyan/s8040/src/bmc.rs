// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AST2600 BMC on eSPI (the PSP brings up the link from the EFS): its
//! Super I/O at `0x2e` with the BMC's COM1, the console, and the KCS
//! channel SMM reports machine checks on.

use fstart_driver_amd::fch::FchHw;
use fstart_driver_amd::fch::acpimmio::{LpcGating, PM_LPC_GATING};
use fstart_driver_amd::fch::lpc::{self, IoPortDecode};
use fstart_driver_amd::window::Window;
use fstart_driver_superio::{ComPortConfig, SuperIoConfig, superio_dsdt_aml};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_amd::acpi::Board as AcpiBoard;
use tock_registers::interfaces::ReadWriteable;

/// The firmware's own KCS channel, KCS2 (`0xCA8`, status/command at
/// `0xCAC`): the BMC's device tree enables it with its kcsbridged, as on
/// the S8030; the OS has KCS3 at `0xCA2` (the vendor SPMI). The vendor's
/// firmware does the same: its IPMI transport hook moves PEI, DXE and SMM
/// to KCS2 (V2.00 PeiIpmiInitialize `0xffd95ece`, DxeIpmiInitialize
/// `0x4308`, SmmIpmiInitialize `0x1c94`), its SPMI gives the OS KCS3.
pub const FIRMWARE_KCS: (u16, u16) = (0xca8, 4);

/// The report of a machine check (`fstart_platform_amd::smm::ras`) to the
/// BMC's SEL, on the firmware's KCS channel, from SMM and at boot for the
/// last boot's errors: a Platform Event Message
/// (the BMC answers Add SEL Entry with C1h, invalid command).
pub fn sel(
    event: &fstart_platform_amd::smm::ras::Event<'_>,
    timer: fstart_driver_amd::ccx::tsc::Tsc,
) -> fstart_platform_amd::smm::ras::Delivery {
    use fstart_platform_amd::smm::ras::{self, Delivery};
    let (port, spacing) = FIRMWARE_KCS;
    // SAFETY: KCS2 is decoded and only firmware uses it.
    let mut kcs = unsafe { fstart_driver_ipmi::kcs::Kcs::new(port, spacing, timer) }
        .with_poll_budget(ras::SEL_POLL_BUDGET);
    match kcs.platform_event(&ras::sel(event)) {
        Ok(()) => Delivery::Sent,
        Err(_) => Delivery::Failed,
    }
}

/// COM1, the BMC's SOL, and its interrupt over SERIRQ.
pub const COM1_PORT: u16 = 0x3f8;
pub const COM1_IRQ: u8 = 4;
pub const BAUD: u32 = 115_200;

/// The firmware console: COM1 at 115200 8N1.
pub const CONSOLE: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio {
        base: COM1_PORT as u64,
    },
    clock_freq: 1_843_200,
    baud_rate: BAUD,
};

/// Before the console: the LPC bridge on, the Super I/O and COM1 decodes
/// (the PSP's eSPI setup and the other decodes stay), then the AST2600's
/// early Super I/O script (Ast2600PeiInit). The COM1 base and the UART
/// routing are the BMC's.
pub fn early_init(fch: &impl FchHw) {
    fch.acpi()
        .at(PM_LPC_GATING)
        .modify(LpcGating::LPC_ENABLE::SET);
    lpc::enable_super_io(fch, true, false);
    lpc::enable_io_decode(fch, IoPortDecode::SERIAL0::SET);
    // SAFETY: the configuration ports are decoded and the BSP is alone.
    unsafe {
        fstart_driver_superio::aspeed_ast2600::early_init_at(
            fstart_driver_superio::aspeed_ast2600::CONFIG_PORT,
        )
    };
}

fn sio_config() -> SuperIoConfig {
    SuperIoConfig {
        com1: Some(ComPortConfig {
            io_base: COM1_PORT,
            irq: COM1_IRQ,
            baud_rate: BAUD,
        }),
        ..SuperIoConfig::default()
    }
}

/// The board part of the ACPI tables: COM1 behind the LPC bridge and the
/// SPCR for it.
pub fn acpi_board() -> AcpiBoard {
    let mut spcr = alloc::vec::Vec::new();
    fstart_acpi::Aml::to_aml_bytes(
        &fstart_acpi::spcr::build_spcr_16550_io(COM1_PORT, COM1_IRQ),
        &mut spcr,
    );
    AcpiBoard {
        lpc_aml: superio_dsdt_aml(&sio_config()),
        tables: alloc::vec![spcr],
        ..AcpiBoard::default()
    }
}
