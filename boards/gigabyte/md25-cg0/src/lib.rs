// SPDX-License-Identifier: MIT OR Apache-2.0
//! GIGABYTE MD25-CG0: 1P AMD SP7 (Venice / WH dual-IOD, Qilian FCH) server
//! board with 16 DDR5 channels, six PCIe 6.0 x16 slots, and an ASPEED AST2700
//! BMC on eSPI/LPC.
//!
//! Offline-compiled and unit-tested only; not yet booted on physical SP7
//! hardware.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

/// The SMM handler, built into the separate SMM image.
#[cfg(feature = "smm")]
mod smm;
pub mod wiring;

use fstart_driver_amd::ccx::tsc::Tsc;
use fstart_driver_amd::fch::FchHw;
use fstart_driver_amd::fch::lpc::{self, IoPortDecode};
use fstart_driver_amd::fch::qilian::espi::{self, Espi, IoRange};
use fstart_driver_superio::{ComPortConfig, SuperIoConfig, superio_dsdt_aml};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_amd::acpi::Board as AcpiBoard;
use fstart_platform_amd::board::SocId;
use fstart_platform_amd::sp7::{Config, Mpio};

pub struct Board;

impl fstart_platform_amd::board::Board for Board {
    const NAME: &'static str = "GIGABYTE MD25-CG0";
    const SOCS: &'static [SocId] = &[SocId::Venice];
    /// 256 cores, SMT (512 logical CPUs).
    const MAX_CPUS: u16 = 512;
    /// Corrected errors counted in SMM and reported to the AST2700 BMC's SEL
    /// ([`sel`]) on the shared OS KCS channel.
    const RAS: fstart_platform_amd::smm::ras::Policy = fstart_platform_amd::smm::ras::Policy {
        shared_bmc: true,
        ..fstart_platform_amd::smm::ras::Policy::THRESHOLDS
    };
    const BMC_EVENT: Option<fstart_platform_amd::board::BmcEvent> = Some(sel);
    /// The TPM header (GIGABYTE's TPM module, optional) is on the SPI TPM
    /// chip select, as GIGABYTE's `ConfigureTpmDecoder` routes it; without
    /// a module the probe finds nothing.
    const TPM: Option<fstart_platform_amd::board::TpmBus> =
        Some(fstart_platform_amd::board::TpmBus::Spi);

    /// Baseline board ACPI contributions (with KCS enabled; the stage
    /// suppresses KCS if no eSPI range decodes it, [`early_init`]).
    fn acpi_board() -> AcpiBoard {
        acpi_board(true)
    }
}

/// IPMI KCS channel on the AST2700 BMC (`0xCA2` data, `0xCA3` status/cmd).
pub const KCS: (u16, u16) = (
    fstart_driver_ipmi::kcs::DEFAULT_PORT,
    fstart_driver_ipmi::kcs::DEFAULT_SPACING,
);

/// Report a machine-check event to the AST2700 BMC's SEL over KCS when idle.
///
/// In stage, `sp7::tables::acpi` gates boot-time SEL calls on
/// `Config::kcs_decoded`. In SMM, [`sel_if`] gates runtime calls on
/// `sp7::smm::Config::kcs_decoded` (passed across the ELF boundary from stage).
pub fn sel(
    event: &fstart_platform_amd::smm::ras::Event<'_>,
    timer: Tsc,
) -> fstart_platform_amd::smm::ras::Delivery {
    use fstart_platform_amd::smm::ras::{self, Delivery};
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

/// Conditional SEL delivery for SMM: returns `Delivery::Failed` immediately
/// when `kcs_decoded` is false (from `sp7::smm::Config::kcs_decoded`).
pub fn sel_if(
    kcs_decoded: bool,
    event: &fstart_platform_amd::smm::ras::Event<'_>,
    timer: Tsc,
) -> fstart_platform_amd::smm::ras::Delivery {
    if !kcs_decoded {
        return fstart_platform_amd::smm::ras::Delivery::Failed;
    }
    sel(event, timer)
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
///
/// Unverified Qilian USB SMU cold-boot and PSP EINJ remain opt-in (`None` /
/// `false`) until validated on physical SP7 silicon.
pub fn config() -> Config {
    use fstart_platform_amd::sp7::links::{Behind, Soldered};
    // The AST2700's display (lane 129): expected, not required. That lane
    // 129 is IOD1 core 6 is decompiler-inferred (MPIO.md G1b), and nothing
    // says the ABL trains it early: the F01 APCB's GNB EarlyPcieConfig
    // (1705h:1003h, read offline from `image.bin`) has lanes 128 (present,
    // Gen4, 5.1), 129 (not present, 5.2) and 130-131 (not present, 5.3),
    // decoded with the published Turin layout (amd-apcb
    // `EarlyPcieConfigBody`), which is not confirmed for Venice. A link
    // that does not come up is reported, the boot goes on.
    let expected = [Soldered {
        socket: 0,
        lane: wiring::BMC_VGA_LANE,
        behind: Behind::BridgedDisplay,
    }]
    .into_iter()
    .collect();
    Config {
        gpio: &[],
        ccx: Default::default(),
        nbio: Default::default(),
        mpio: Mpio {
            global: Default::default(),
            ancillary: false,
            // fstart's endpoint reset recovery (the F01 `AfterResetDelay`
            // value is not known).
            after_reset_delay_ms: 200,
            // No UBM backplane, no hotplug slot in the F01 tables.
            ubm_hotplug: false,
            ports: Default::default(),
            cores: Default::default(),
            hotplug: None,
            // The F01 APCB's early PCIe entry does not mark lane 129
            // present (see `expected`): MPIO trains it like any port.
            early_bmc: None,
            required: Default::default(),
            expected,
            perst: &wiring::PERST,
        },
        usb: None,
        display: Some(wiring::BMC_VGA_LANE),
        legacy_devices: true,
        // Populated by `stage` from `early_init()` before entering console/SMM.
        kcs_decoded: false,
        // All 16 SP7 DDR5 channels of its one socket.
        memory_channels: [0xffff, 0],
        // Up to 256 GiB RDIMM per slot (matches SP5 Onyx).
        max_dimm_mib: 256 * 1024,
        einj: false,
        // GIGABYTE's DSDT declares every root bridge ACPI0016 and its DXE
        // carries the CXL modules, but which x16 slots train CXL (WH CXL
        // needs PCIe cores 0/1) and the F01 PCDs are not known, and its
        // APCB's CXL region array is empty: no CXL until verified.
        cxl: None,
    }
}

/// COM1 on the AST2700 (SUART1, which the ABL's `EspiSioInit` puts at
/// `0x3F8`/IRQ 4).
const COM1_RANGE: IoRange = IoRange {
    base: COM1_PORT,
    size: 8,
};

/// What the ABL's eSPI decodes (APCB `EspiInit`, GIGABYTE: `0xCA2`+4,
/// `0x2F8`+8, `0x3F8`+8, Port 80, 60h/64h) send to the AST2700: the console
/// and KCS decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BmcDecodes {
    /// The KCS data and status ports `0xCA2`/`0xCA3`.
    pub kcs: bool,
    /// COM1, `0x3F8..0x3FF`.
    pub com1: bool,
    /// The Super I/O configuration ports `0x2E`/`0x2F`.
    pub sio: bool,
}

/// The eSPI decodes of both controllers as the ABL left them.
pub fn bmc_decodes(fch: &impl FchHw) -> BmcDecodes {
    let (port, spacing) = KCS;
    BmcDecodes {
        kcs: espi::covers(fch.mmio(), port, spacing + 1),
        com1: espi::covers(fch.mmio(), COM1_RANGE.base, COM1_RANGE.size),
        sio: espi::covers(
            fch.mmio(),
            fstart_driver_superio::aspeed_ast2700::CONFIG_PORT,
            2,
        ),
    }
}

/// Before the console. The eSPI link to the AST2700 and its decodes are the
/// ABL's (APCB): they are read, not reset. COM1 is added to eSPI0 if the
/// APCB left it out (the board's console); the KCS decision is what the
/// ABL decoded. The LPC bridge bits GIGABYTE's SEC sets (`D14F3x44[6]`
/// COM1, `D14F3x48` 2Eh/4Eh/Port 80) are kept; LPC wide-I/O windows and
/// the LPC gate are not: they do not decode eSPI cycles. The AST2700 Super
/// I/O is unlocked only when eSPI decodes its configuration ports (the ABL
/// already set SUART1 up). Returns whether KCS is decoded.
pub fn early_init(fch: &impl FchHw) -> bool {
    lpc::enable_super_io(fch, true, true);
    lpc::enable_io_decode(fch, IoPortDecode::SERIAL0::SET);
    lpc::enable_port80(fch);
    if let Some(espi0) = Espi::new(fch.mmio(), 0) {
        // Already decoded on the MD25-CG0's APCB: no write.
        let _ = espi0.add_io(COM1_RANGE);
    }
    let decodes = bmc_decodes(fch);
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if decodes.sio {
        // SAFETY: 0x2E/0x2F decoded to the AST2700 on eSPI; BSP owns the
        // Super I/O port pair.
        unsafe {
            fstart_driver_superio::aspeed_ast2700::early_init_at(
                fstart_driver_superio::aspeed_ast2700::CONFIG_PORT,
            );
        }
    }
    decodes.kcs
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
    fn kcs_follows_the_espi_ranges_not_lpc() {
        use fstart_driver_amd::fch::qilian::espi::CONTROLLERS;
        use fstart_driver_amd::fch::testing::{Space, World};
        let e0 = CONTROLLERS[0];
        // The GIGABYTE APCB's ranges 0-2, Port 80, 60h/64h.
        let world = World::default();
        world.set(Space::Mmio, e0 + 0x40, 4, 0b111 << 8 | 0b110);
        for (i, (base, size)) in [(0xca2u32, 4u32), (0x2f8, 8), (0x3f8, 8)]
            .iter()
            .enumerate()
        {
            world.set(Space::Mmio, e0 + 0x44 + 2 * i as u32, 2, *base);
            world.set(Space::Mmio, e0 + 0x4c + i as u32, 1, size - 1);
        }
        let fch = world.fch();
        assert_eq!(
            bmc_decodes(&fch),
            BmcDecodes {
                kcs: true,
                com1: true,
                sio: false
            }
        );
        assert!(early_init(&fch));
        // COM1 was decoded: eSPI untouched. No LPC wide I/O window opened.
        let espi_writes = world
            .writes()
            .into_iter()
            .filter(|w| w.0 == Space::Mmio && (e0..e0 + 0x100).contains(&w.1))
            .count();
        assert_eq!(espi_writes, 0);
        assert_eq!(world.get(Space::Lpc, 0x64, 4), 0);
        assert_eq!(world.get(Space::Lpc, 0x48, 4) & (1 << 2), 0);
        assert_eq!(world.get(Space::Lpc, 0x48, 4) & 0x20_0003, 0x20_0003);

        // Without the KCS range, KCS is not decoded; COM1 gets a range.
        let world = World::default();
        world.set(Space::Mmio, e0 + 0x40, 4, 0b110);
        let fch = world.fch();
        assert!(!early_init(&fch));
        assert_eq!(world.get(Space::Mmio, e0 + 0x44, 2), 0x3f8);
        assert!(bmc_decodes(&fch).com1);
    }

    #[test]
    fn board_targets_venice_with_com1_conditional_kcs_and_opt_in_usb_einj() {
        assert_eq!(Board::SOCS, &[SocId::Venice]);
        assert_eq!(Board::TPM, Some(fstart_platform_amd::board::TpmBus::Spi));
        assert_eq!(Board::MAX_CPUS, 512);
        let cfg = config();
        assert!(cfg.legacy_devices);
        assert!(!cfg.kcs_decoded);
        assert!(cfg.usb.is_none());
        assert!(!cfg.einj);
        assert_eq!(cfg.memory_channels, [0xffff, 0]);
        assert_eq!(cfg.max_dimm_mib, 256 * 1024);
        assert!(cfg.mpio.early_bmc.is_none() && !cfg.mpio.ubm_hotplug);
        // The BMC's display is expected, not required: no halt without it.
        assert!(cfg.mpio.required.is_empty());
        assert_eq!(cfg.mpio.expected.len(), 1);
        assert_eq!(cfg.mpio.expected[0].lane, wiring::BMC_VGA_LANE);

        let baseline = Board::acpi_board();
        assert_eq!(baseline.tables.len(), 1);
        assert_eq!(&baseline.tables[0][0..4], b"SPCR");
        assert!(baseline.ipmi.is_some());
        assert!(!baseline.lpc_aml.is_empty());

        let acpi_no_kcs = acpi_board(false);
        assert!(acpi_no_kcs.ipmi.is_none());
    }
}
