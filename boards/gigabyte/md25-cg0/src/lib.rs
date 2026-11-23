// SPDX-License-Identifier: MIT OR Apache-2.0
//! GIGABYTE MD25-CG0: 1P SP7 server board for AMD Venice (WH I/O die + Qilian
//! FCH) with an ASPEED AST2700 BMC over eSPI/LPC.
//!
//! Status: compiled, unit-tested, and donor-ROM flash-validated offline;
//! not yet booted on physical SP7 hardware.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
mod smm;
pub mod wiring;

use core::sync::atomic::{AtomicBool, Ordering};

use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::fch::FchHw;
use fstart_driver_amd::fch::acpimmio::{LpcGating, PM_LPC_GATING};
use fstart_driver_amd::fch::lpc::{self, IoPortDecode};
use fstart_driver_amd::window::Window;
use fstart_driver_superio::{ComPortConfig, SuperIoConfig, superio_dsdt_aml};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_amd::acpi::Board as AcpiBoard;
use fstart_platform_amd::board::SocId;
use fstart_platform_amd::sp7::{Config, Mpio};
use tock_registers::interfaces::ReadWriteable;

/// Records whether host-side LPC wide-I/O decode for KCS (`0xCA0..0xCAF`) succeeded.
static KCS_DECODED: AtomicBool = AtomicBool::new(false);

pub struct Board;

impl fstart_platform_amd::board::Board for Board {
    const NAME: &'static str = "GIGABYTE MD25-CG0";
    const SOCS: &'static [SocId] = &[SocId::Venice];
    /// 1P SP7 ceiling: up to 256 cores, SMT (512 logical CPUs).
    const MAX_CPUS: u16 = 512;
    /// Corrected errors counted in SMM and reported to the AST2700 BMC's SEL
    /// over KCS (`0xCA2`/`0xCA3`), shared with the OS IPMI driver.
    const RAS: fstart_platform_amd::smm::ras::Policy = fstart_platform_amd::smm::ras::Policy {
        shared_bmc: true,
        ..fstart_platform_amd::smm::ras::Policy::THRESHOLDS
    };
    const BMC_EVENT: Option<fstart_platform_amd::board::BmcEvent> = Some(sel);
}

/// IPMI KCS channel on the AST2700 BMC (`0xCA2` data, `0xCA3` status/cmd).
pub const KCS: (u16, u16) = (
    fstart_driver_ipmi::kcs::DEFAULT_PORT,
    fstart_driver_ipmi::kcs::DEFAULT_SPACING,
);

/// Report a machine-check event to the AST2700 BMC's SEL over KCS when idle
/// and when host-side KCS I/O decode was established during [`early_init`].
pub fn sel(
    event: &fstart_platform_amd::smm::ras::Event<'_>,
    timer: Tsc,
) -> fstart_platform_amd::smm::ras::Delivery {
    use fstart_platform_amd::smm::ras::{self, Delivery};
    if !KCS_DECODED.load(Ordering::Acquire) {
        return Delivery::Failed;
    }
    let (port, spacing) = KCS;
    // SAFETY: BMC KCS ports decoded by `early_init`; only started when idle.
    let mut kcs = unsafe { fstart_driver_ipmi::kcs::Kcs::new(port, spacing, timer) }
        .with_poll_budget(ras::SEL_POLL_BUDGET);
    if !kcs.idle() {
        return Delivery::Busy;
    }
    match kcs.platform_event(&ras::sel(event)) {
        Ok(()) => Delivery::Sent,
        Err(_) => Delivery::Failed,
    }
}

/// Host serial console: COM1 (`0x3F8`, IRQ 4) routed via the AST2700 BMC/SIO.
pub const COM1_PORT: u16 = 0x3f8;
pub const COM1_IRQ: u8 = 4;
pub const BAUD: u32 = 115_200;

pub const CONSOLE: Ns16550Config = Ns16550Config {
    regs: AccessMode::Pio {
        base: COM1_PORT as u64,
    },
    clock_freq: 1_843_200,
    baud_rate: BAUD,
};

/// Board configuration for Venice on the MD25-CG0.
pub fn config() -> Config {
    use fstart_platform_amd::sp7::links::{Behind, Soldered};
    let required = [Soldered {
        lane: wiring::BMC_VGA_LANE,
        behind: Behind::BridgedDisplay,
    }]
    .into_iter()
    .collect();
    Config {
        gpio: &[],
        nbio: Default::default(),
        ccx: Default::default(),
        mpio: Mpio {
            required,
            perst: &wiring::PERST,
            psp_trained: &wiring::PSP_TRAINED,
        },
        usb: wiring::USB,
        display: Some(wiring::BMC_VGA_LANE),
        legacy_devices: true,
        // All 16 SP7 DDR5 channels.
        memory_channels: 0xffff,
        // Up to 256 GiB RDIMM per slot (matches SP5 Onyx).
        max_dimm_mib: 256 * 1024,
        einj: true,
    }
}

/// Before the console: enable host-side LPC/eSPI bridge decodes for the AST2700
/// Super I/O (`0x2E`/`0x4E`), COM1 (`0x3F8`), and IPMI KCS (`0xCA0..0xCAF`).
///
/// Enables host-side decodes and unlocks/activates AST2700 Super I/O SUART1/SUART2.
/// PSP/EFS eSPI peripheral channel bring-up is performed prior to x86 entry.
/// Returns `true` if the wide-I/O range for KCS (`0xCA0..0xCAF`) was allocated.
pub fn early_init(fch: &impl FchHw) -> bool {
    fch.acpi()
        .at(PM_LPC_GATING)
        .modify(LpcGating::LPC_ENABLE::SET);
    lpc::enable_super_io(fch, true, true);
    lpc::enable_io_decode(fch, IoPortDecode::SERIAL0::SET);
    let kcs_ok = lpc::set_wideio_range(fch, 0xca0, 16).is_ok();
    KCS_DECODED.store(kcs_ok, Ordering::Release);
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    // SAFETY: 0x2E/0x2F decoded above; BSP owns the AST2700 Super I/O port pair.
    unsafe {
        fstart_driver_superio::aspeed_ast2700::early_init_at(
            fstart_driver_superio::aspeed_ast2700::CONFIG_PORT,
        );
    }
    kcs_ok
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

/// Board ACPI additions: COM1 behind LPC/eSPI, SPCR for COM1, and IPMI KCS
/// (published only when `kcs_decoded` is true).
pub fn acpi_board(kcs_decoded: bool) -> AcpiBoard {
    let mut spcr = alloc::vec::Vec::new();
    fstart_acpi::Aml::to_aml_bytes(
        &fstart_acpi::spcr::build_spcr_16550_io(COM1_PORT, COM1_IRQ),
        &mut spcr,
    );
    AcpiBoard {
        lpc_aml: superio_dsdt_aml(&sio_config()),
        tables: alloc::vec![spcr],
        ipmi: kcs_decoded.then_some(fstart_driver_ipmi::kcs::DEFAULT_ACPI),
        ..AcpiBoard::default()
    }
}

/// SMBIOS system identity.
pub fn smbios_identity() -> fstart_acpi::smbios::SmbiosIdentity<'static> {
    fstart_acpi::smbios::SmbiosIdentity::new("GIGABYTE", "MD25-CG0")
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_platform_amd::board::Board as _;

    #[test]
    fn board_targets_venice_with_com1_and_conditional_kcs() {
        assert_eq!(Board::SOCS, &[SocId::Venice]);
        assert_eq!(Board::MAX_CPUS, 512);
        let cfg = config();
        assert!(cfg.legacy_devices);
        assert_eq!(cfg.memory_channels, 0xffff);
        assert_eq!(cfg.max_dimm_mib, 256 * 1024);
        assert_eq!(cfg.mpio.psp_trained, &[(129, 129)]);

        let acpi_with_kcs = acpi_board(true);
        assert_eq!(acpi_with_kcs.tables.len(), 1);
        assert_eq!(&acpi_with_kcs.tables[0][0..4], b"SPCR");
        assert!(acpi_with_kcs.ipmi.is_some());
        assert!(!acpi_with_kcs.lpc_aml.is_empty());

        let acpi_no_kcs = acpi_board(false);
        assert!(acpi_no_kcs.ipmi.is_none());
    }
}
