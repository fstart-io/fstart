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
use fstart_driver_amd::fch::qilian::hwacpi::SyncFlood;
use fstart_driver_superio::{ComPortConfig, SuperIoConfig, superio_dsdt_aml};
use fstart_driver_uart::ns16550::{AccessMode, Ns16550Config};
use fstart_platform_amd::acpi::Board as AcpiBoard;
use fstart_platform_amd::board::SocId;
use fstart_platform_amd::sp7::{Config, EarlyBmc, Mpio};

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

    /// The host ACPI baseline's Venice: the ECAM above 4 GiB, firmware-first.
    fn sp7_config() -> Option<Config> {
        Some(config())
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
    // The AST2700's display on the link the ABL trains (lane 128, IOD 1's
    // bonus core, 5.1; [`wiring::BMC_VGA_LANE`]): expected, not required.
    // A link that does not come up is reported, the boot goes on.
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
        nbio: nbio(),
        smu: smu(),
        // GIGABYTE's sync flood (`PcdResetCpuOnSyncFlood` ex `0xFC007` 0,
        // `PcdDelayResetCpuOnSyncFlood` `0xFC03B` 0): no CPU reset.
        sync_flood: SyncFlood {
            reset_cpu: false,
            delay: 0,
        },
        mpio: Mpio {
            // GIGABYTE's build values: dword 0 `0x8C1` (no exact match),
            // `CfgDxioClockGating` 1, dwords 3 and 5 as Dell's.
            global: fstart_platform_amd::sp7::MpioGlobal::GIGABYTE_BUILD,
            // `PcdMPIOAncDataSupport` (ex `0x41600`) 1, as Dell's.
            ancillary: true,
            // `PcdAfterResetDelay` (ex `0x4F221`) 0, as Dell's.
            after_reset_delay_ms: 0,
            // `PcdCfgHotplugUMBSupport` is 1 in both donors, but the board
            // has no UBM backplane and the F01 tables no hotplug slot:
            // nothing for `UPDATE_LINK` and the UBM PERST to act on.
            ubm_hotplug: false,
            // GIGABYTE's AmdCxlOnAllPorts (PCD 0x500AC) = 1: the CXL
            // straps on every port. `PcdPtmEn` (ex `0x4F069`) 0: strap 0x78
            // off (Dell 1).
            ports: fstart_platform_amd::sp7::PortPolicy {
                cxl_on_all_ports: true,
                ptm: false,
                ..Default::default()
            },
            cores: Default::default(),
            core_policy: Default::default(),
            hotplug: None,
            // `PcdEarlyBmcLinkTraining` (ex `0x4F004`) 1, socket 0
            // (`0x4F005`), lane `0x80` (`PcdEarlyBmcLinkLaneNum`, ex
            // `0x4F03E` by code position: SilVenicePei `0x7092b9c1`, Dell's
            // `0xf0f963`). The F01 APCB's early PCIe entry agrees: lane 128
            // present (Gen4, 5.1), and GIGABYTE's SMBIOS type 41 places the
            // AST2700 there ([`wiring::BMC_VGA_LANE`]). `$A26`'s x1 port
            // at 129 (5.2, not present in the APCB entry) is the same
            // bonus core: the early-link scan (every bonus port's LTSSM)
            // finds the ABL's link, and a core with one takes the early
            // register path; 129 stays an MPIO port ([`wiring::ports`]).
            early_bmc: Some(EarlyBmc {
                socket: 0,
                lane: wiring::EARLY_BMC_LANE,
            }),
            required: Default::default(),
            expected,
            perst: &wiring::PERST,
        },
        usb: [None, None],
        // The F01 APCB's `DfPciMmioBaseLo/Hi` and `DfPciMmioSize`
        // (`0x2FFC_8000_0000`, one segment; 2P_COMPLETE §2.2), used if the
        // ABL left `MMIO_CFG_BASE` off.
        ecam: fstart_driver_amd::ccx::msr::EcamWindow::new(0x2ffc_8000_0000, 1),
        // The ABL's bus counts.
        bus_request: None,
        // `PcdAmdFabricCcxAsNumaDomain` default: off.
        ccx_as_numa: false,
        display: Some(wiring::BMC_VGA_LANE),
        legacy_devices: true,
        // Populated by `stage` from `early_init()` before entering console/SMM.
        kcs_decoded: false,
        // All 16 SP7 DDR5 channels of its one socket.
        memory_channels: [0xffff, 0],
        // Up to 256 GiB RDIMM per slot: fstart's value (SP5 Onyx's), not
        // GIGABYTE's SMBIOS type 16, which the research did not extract.
        max_dimm_mib: 256 * 1024,
        einj: false,
        // As GIGABYTE ships it (CXL_COMPLETE.md §3): CXL on every port
        // (AmdCxlOnAllPorts 1; its DSDT makes every root bridge ACPI0016),
        // the donors' input block and DXE PCDs, soft-reserved memory
        // (PcdAmdCxlSpm 1). Its CXL `_OSC` rejects the CXL UUID; fstart's
        // grants CXL memory error reporting and, firmware-first, every PCI
        // control but AER and DPC (`acpi::CxlHost`).
        //
        // Departure: not GIGABYTE's CxlV2Manager loop that sizes the
        // devices, rewrites the booted APCB's (empty) region array in the
        // SPI flash and warm-resets. That flash write has not run on this
        // board; until it has been seen working, the APCB's regions stay
        // the image's (none), so the ABL maps no CXL window and devices
        // stay CXL.io only.
        cxl: Some(fstart_platform_amd::sp7::cxl::Config {
            ports: &[],
            on_all_ports: true,
            regions: &[],
            memory: fstart_platform_amd::sp7::cxl::Memory::SoftReserved,
            policy: fstart_platform_amd::sp7::cxl::POLICY,
            lock_host_decoders: true,
            dxe: fstart_platform_amd::sp7::cxl::DXE_POLICY,
            apcb_update: false,
            sync_flood: true,
        }),
        // HSMP, as the F01 SSDT `SMU-HSMP`; no FCH I2C controller in use.
        // GIGABYTE's ISA step (33 MHz normal SPI read) and its WDAT, whose
        // 1000 ms per count is the watchdog's 1 s clock.
        devices: fstart_platform_amd::sp7::Devices {
            isa: Some(fstart_driver_amd::fch::qilian::isa::Policy::GIGABYTE),
            wdat: Some(fstart_driver_amd::fch::qilian::pm::WatchdogClock::S1),
            ..Default::default()
        },
    }
}

/// The NBIO policy: the donors' values, with GIGABYTE's where they differ:
/// `PcdCfgSriovEnDev0F1` (ex `0x51010`) 0, no `NBIF_OPT_2A`;
/// `PcdAmdCxlOnAllPorts` (`0x500AC`) 1, IOMMU property `0x200`
/// (`cfg+0x80`), GIGABYTE's egress poison masks. AER firmware-first as GIGABYTE
/// (the SoC default, `nbio::wh::Policy::aer_mechanism`).
///
/// Unresolved: GIGABYTE's build has no `PcdCtrlUnusedTileClkGating`
/// (VALUES "Open"); the board takes the SoC default, Dell's 1, for
/// `EnableSPGUnusedPcie` and `MPIO_ENABLE_CG`'s third argument.
fn nbio() -> fstart_driver_amd::nbio::wh::Policy {
    let mut policy = fstart_driver_amd::nbio::wh::Policy::default();
    policy.nbif.features = false;
    policy.iommu_200 = true;
    // `PcdAmdNbioEgressPoisonMaskHi` `0xFFFC_FFFE`, no severity
    // (`PcdEnableEgressPoisonSeverity` 0).
    policy.ras.egress_poison = fstart_driver_amd::nbio::wh::ras::EgressPoison::GIGABYTE;
    policy
}

/// The board's platform power limits (`PcdCfgPlatform{TDP,PPT,TDC,TdcSoc,
/// TdcMemVddioS3,EDC}`): the F01 PCD database (ex `0x4F110`, `0x4F111`,
/// `0x4F112`, `0x4F114`, `0x4F115`, `0x4F113`; no CBS override at first
/// boot), 600 W, 600 W, 330 A, 310 A, 210 A, 430 A. (Dell PR9XJ 1.0.8's,
/// for comparison: 630 W, 630 W, 350 A, 310 A, 210 A, 455 A.)
const POWER_LIMITS: fstart_driver_amd::smu::v14::pptable::PlatformLimits =
    fstart_driver_amd::smu::v14::pptable::PlatformLimits {
        tdp_w: 600,
        ppt_w: 600,
        tdc_cpu_a: 330,
        tdc_soc_a: 310,
        tdc_vddio_a: 210,
        edc_a: 430,
    };

/// What `SmuInitialization` sends: both donors' values with the board's
/// platform power limits ([`POWER_LIMITS`]) and without `UpdatePccLimit`,
/// which its PI 0.0.8.1 does not send.
fn smu() -> fstart_driver_amd::smu::v14::init::Policy {
    use fstart_driver_amd::smu::v14::{init, pptable};
    init::Policy {
        pptable: pptable::PpTable::DEFAULT.with_platform_limits(POWER_LIMITS),
        update_pcc_limit: false,
        ..init::Policy::DEFAULT
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
/// COM1, `D14F3x48` 2Eh/4Eh/Port 80) are set here as its SEC does, and
/// cleared again by GIGABYTE's ISA step at `init_fch` (`qilian::isa`,
/// `PcdLpcEnable` 0, as F01): eSPI carries COM1, KCS and Port 80, with
/// its own decodes. LPC wide-I/O windows and the LPC gate are not set:
/// they do not decode eSPI cycles. The AST2700 Super
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

    /// The board data that differs between the donors is GIGABYTE's F01
    /// PCD database (venice `reports/research/VALUES/decisions.json`,
    /// mirrored here; `lookup.py` per token).
    #[test]
    fn donor_differences_are_gigabytes() {
        use fstart_driver_amd::dxio::wh::{Layout, translate_socket_lane};
        let cfg = config();
        // MPIO: ancillary data 1, AfterResetDelay 0, early BMC 1 / 0 / 0x80.
        assert!(cfg.mpio.ancillary);
        assert_eq!(cfg.mpio.after_reset_delay_ms, 0);
        let bmc = cfg.mpio.early_bmc.unwrap();
        assert_eq!((bmc.socket, bmc.lane), (0, 0x80));
        // Lane 128 is a valid one-lane link on IOD 1's bonus core, the
        // core of `$A26`'s port at 129; the display is the ABL's link.
        let early = translate_socket_lane(Layout::Sp7B0, bmc.lane, bmc.lane).unwrap();
        let x1 = translate_socket_lane(Layout::Sp7B0, 129, 129).unwrap();
        assert_eq!((early.iod, early.core), (x1.iod, x1.core));
        assert_eq!((early.iod, early.core), (1, 6));
        assert_eq!(cfg.display, Some(bmc.lane));
        // GIGABYTE's MPIO global config, egress poison, ISA and WDAT.
        assert_eq!(
            cfg.mpio.global.dwords(false),
            [0x8c1, 1, 0, 0xec40_0000, 0, 3]
        );
        assert_eq!(
            cfg.nbio.ras.egress_poison,
            fstart_driver_amd::nbio::wh::ras::EgressPoison::GIGABYTE
        );
        assert_eq!(
            cfg.devices.isa,
            Some(fstart_driver_amd::fch::qilian::isa::Policy::GIGABYTE)
        );
        assert_eq!(cfg.devices.wdat.map(|c| c.period_ms()), Some(1000));
        // PcdPtmEn 0, PcdCfgSriovEnDev0F1 0, PcdAmdCxlOnAllPorts 1.
        assert!(!cfg.mpio.ports.ptm);
        assert!(!cfg.nbio.nbif.features && cfg.nbio.iommu_200);
        // PcdResetCpuOnSyncFlood 0, PcdDelayResetCpuOnSyncFlood 0.
        assert_eq!(
            cfg.sync_flood,
            SyncFlood {
                reset_cpu: false,
                delay: 0
            }
        );
        // PcdCfgPlatform{TDP,PPT,TDC,TdcSoc,TdcMemVddioS3,EDC}.
        let t = &cfg.smu.pptable;
        assert_eq!(
            [
                t.tdp_platform_limit.get(),
                t.ppt_platform_limit.get(),
                t.tdc_cpu_platform_limit.get(),
                t.tdc_soc_platform_limit.get(),
                t.tdc_vddio_mem_s3_platform_limit.get(),
                t.edc_platform_limit.get()
            ],
            [0x258, 0x258, 0x14a, 0x136, 0xd2, 0x1ae]
        );
        assert!(!cfg.smu.update_pcc_limit && cfg.smu.hsmp);
    }

    /// The ABL tokens this manifest sets (`[package.metadata.fstart.amd]
    /// apcb-tokens`, which the image build writes into the APCBs): `(id,
    /// value)`, names resolved as image-build resolves them.
    fn apcb_tokens() -> alloc::vec::Vec<(u32, u32)> {
        let manifest: toml::Table = include_str!("../Cargo.toml").parse().unwrap();
        let tokens = manifest["package"]["metadata"]["fstart"]["amd"]["apcb-tokens"]
            .as_array()
            .unwrap();
        tokens
            .iter()
            .map(|t| {
                let id = match (t.get("id"), t.get("name")) {
                    (Some(id), _) => id.as_integer().unwrap() as u32,
                    (None, Some(name)) => {
                        fstart_amdfw_format::apcb::names::id(name.as_str().unwrap()).unwrap()
                    }
                    (None, None) => panic!("token without id or name"),
                };
                let value = &t["value"];
                let value = value
                    .as_bool()
                    .map(u32::from)
                    .unwrap_or_else(|| value.as_integer().unwrap() as u32);
                (id, value)
            })
            .collect()
    }

    /// The APCB and the configuration agree by the SoC's rules: the ECAM
    /// the ABL places is the one fstart falls back to, the IOMMU setting
    /// is fstart's. The values are GIGABYTE's F01 ones
    /// (VALUES/apcb-tokens.json `gbt_values`).
    #[test]
    fn apcb_tokens_agree_with_the_config() {
        use fstart_platform_amd::sp7::apcb::{disagreement, token};
        let tokens = apcb_tokens();
        let get = |id: u32| tokens.iter().find(|t| t.0 == id).map(|t| t.1);
        let cfg = config();
        assert_eq!(disagreement(&cfg, get), None);
        let names = fstart_amdfw_format::apcb::names::id;
        for (name, gigabyte) in [
            ("DfPciMmioBaseLo", 0x8000_0000),
            ("DfPciMmioBaseHi", 0x2ffc),
            ("DfPciMmioSize", 0x1000_0000),
            ("NbioIommu", 1),
            ("FchRom3BaseHigh", 0x2ffd),
            ("BmcInitBeforeDram", 1),
            // amd-apcb's spelling (sic) of xHCI 0's token, an alias of the
            // manifest's `CbsCmnFchUsbXHCI0Enable`.
            ("FcbUsb0Enable", 1),
            ("FchUsb1Enable", 1),
            ("FchUsb2Enable", 1),
            ("FchUsb3Enable", 1),
        ] {
            assert_eq!(get(names(name).unwrap()), Some(gigabyte), "{name}");
        }
        // The one departure from F01 (1): never fuse PSB.
        assert_eq!(get(names("PspPsbAutoFuse").unwrap()), Some(0));
        assert_eq!(tokens.len(), 11);
        // ROM3 is inside the identity map, the BMC link the ABL trains is
        // the one the board keeps.
        let rom3 = u64::from(get(names("FchRom3BaseHigh").unwrap()).unwrap()) << 32;
        let layout = <fstart_platform_amd::venice::Wh as fstart_platform_amd::sp7::Variant>::LAYOUT;
        assert!(rom3 + (64 << 20) <= (layout.pdpts * 512) << 30);
        assert!(cfg.mpio.early_bmc.is_some());

        // Another window, or the IOMMU off, would be caught.
        let mut moved = config();
        moved.ecam = fstart_driver_amd::ccx::msr::EcamWindow::new(0x1000_0000_0000, 2);
        assert_eq!(disagreement(&moved, get), Some(token::DF_PCI_MMIO_BASE_LO));
        let mut off = config();
        off.nbio.iommu = false;
        assert_eq!(disagreement(&off, get), Some(token::NBIO_IOMMU));
    }

    #[test]
    fn board_targets_venice_with_com1_conditional_kcs_and_opt_in_usb_einj() {
        assert_eq!(Board::SOCS, &[SocId::Venice]);
        assert_eq!(Board::TPM, Some(fstart_platform_amd::board::TpmBus::Spi));
        assert_eq!(Board::MAX_CPUS, 512);
        let cfg = config();
        assert!(cfg.legacy_devices);
        assert!(!cfg.kcs_decoded);
        assert!(cfg.usb.iter().all(Option::is_none));
        let ecam = cfg.ecam.unwrap();
        assert_eq!((ecam.base, ecam.segments), (0x2ffc_8000_0000, 1));
        assert!(!cfg.einj);
        assert_eq!(cfg.memory_channels, [0xffff, 0]);
        assert_eq!(cfg.max_dimm_mib, 256 * 1024);
        assert!(!cfg.mpio.ubm_hotplug);
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
