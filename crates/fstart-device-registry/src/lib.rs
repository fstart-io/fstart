//! Device registry crate.
//!
//! This is a **host-only** `std` crate used during code generation (`fstart-codegen`)
//! to parse board configurations and produce the `DriverInstance` enum.
//!
//! It aggregates all driver configuration types from the various driver crates
//! into a single enum. The same enum is replicated into the firmware image via
//! codegen, but the firmware uses a feature-minimized version.
//!
//! On the host (codegen), enable the `all-drivers` feature to support parsing
//! any board configuration.

#![cfg_attr(docsrs, feature(doc_cfg))]
#![allow(unused_imports)] // Conditional imports below

use heapless::String as HString;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Re-export driver config types (conditionally based on features)
// ---------------------------------------------------------------------------

#[cfg(feature = "ns16550")]
pub mod ns16550 {
    pub use fstart_driver_ns16550::{AccessMode, Ns16550Config};
}

#[cfg(feature = "pl011")]
pub mod pl011 {
    pub use fstart_driver_pl011::Pl011Config;
}

#[cfg(feature = "designware-i2c")]
pub mod designware_i2c {
    pub use fstart_driver_designware_i2c::DesignwareI2cConfig;
}

#[cfg(feature = "sunxi-a20-ccu")]
pub mod sunxi_a20_ccu {
    pub use fstart_driver_sunxi_ccu::SunxiA20CcuConfig;
}

#[cfg(feature = "sunxi-h3-ccu")]
pub mod sunxi_h3_ccu {
    pub use fstart_driver_sunxi_h3_ccu::SunxiH3CcuConfig;
}

#[cfg(feature = "sunxi-a20-dramc")]
pub mod sunxi_a20_dramc {
    pub use fstart_driver_sunxi_a20_dramc::SunxiA20DramcConfig;
}

#[cfg(feature = "sunxi-h3-dramc")]
pub mod sunxi_h3_dramc {
    pub use fstart_driver_sunxi_h3_dramc::SunxiH3DramcConfig;
}

#[cfg(feature = "sunxi-mmc")]
pub mod sunxi_mmc {
    pub use fstart_driver_sunxi_mmc::SunxiMmcConfig;
}

#[cfg(feature = "sunxi-spi")]
pub mod sunxi_spi {
    pub use fstart_driver_sunxi_spi::SunxiSpiConfig;
}

#[cfg(feature = "sunxi-d1-ccu")]
pub mod sunxi_d1_ccu {
    pub use fstart_driver_sunxi_d1_ccu::SunxiD1CcuConfig;
}

#[cfg(feature = "sunxi-d1-dramc")]
pub mod sunxi_d1_dramc {
    pub use fstart_driver_sunxi_d1_dramc::SunxiD1DramcConfig;
}

#[cfg(feature = "sifive-uart")]
pub mod sifive_uart {
    pub use fstart_driver_sifive_uart::SifiveUartConfig;
}

#[cfg(feature = "fu740-prci")]
pub mod fu740_prci {
    pub use fstart_driver_fu740_prci::Fu740PrciConfig;
}

#[cfg(feature = "fu740-ddr")]
pub mod fu740_ddr {
    pub use fstart_driver_fu740_ddr::Fu740DdrConfig;
}

#[cfg(feature = "pci-ecam")]
pub mod pci_ecam {
    pub use fstart_driver_pci_ecam::PciEcamConfig;
}

#[cfg(feature = "bochs-display")]
pub mod bochs_display {
    pub use fstart_driver_bochs_display::BochsDisplayConfig;
}

#[cfg(feature = "qemu-fw-cfg")]
pub mod qemu_fw_cfg {
    pub use fstart_driver_qemu_fw_cfg::QemuFwCfgConfig;
}

#[cfg(feature = "q35-hostbridge")]
pub mod q35_hostbridge {
    pub use fstart_driver_q35_hostbridge::Q35HostBridgeConfig;
}

#[cfg(feature = "ite8721f")]
pub mod ite8721f {
    pub use fstart_driver_ite8721f::Ite8721fConfig;
}

#[cfg(feature = "nsc-pc87382")]
pub mod nsc_pc87382 {
    pub use fstart_driver_nsc_pc87382::Pc87382Config;
}

#[cfg(feature = "nsc-pc87392")]
pub mod nsc_pc87392 {
    pub use fstart_driver_nsc_pc87392::Pc87392Config;
}

#[cfg(feature = "intel-pineview")]
pub mod intel_pineview {
    pub use fstart_driver_intel_pineview::IntelPineviewConfig;
}

#[cfg(feature = "intel-ich7")]
pub mod intel_ich7 {
    pub use fstart_driver_intel_ich7::IntelIch7Config;
}

#[cfg(feature = "intel-gm965")]
pub mod intel_gm965 {
    pub use fstart_driver_intel_gm965::IntelGm965Config;
}

#[cfg(feature = "intel-ich8")]
pub mod intel_ich8 {
    pub use fstart_driver_intel_ich8::IntelIch8Config;
}

#[cfg(feature = "lenovo-x61-mainboard")]
pub mod lenovo_x61_mainboard {
    pub use fstart_mainboard_lenovo_x61::LenovoX61MainboardConfig;
}

#[cfg(feature = "i2c-ck505")]
pub mod i2c_ck505 {
    pub use fstart_driver_i2c_ck505::I2cCk505Config;
}

// ---------------------------------------------------------------------------
// DriverMeta — static metadata about a driver
// ---------------------------------------------------------------------------

/// Service traits a driver instance can provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Service {
    Console,
    BlockDevice,
    ClockController,
    MemoryController,
    PciRootBus,
    PciHost,
    SmmOps,
    Framebuffer,
    AcpiTableProvider,
    X86AcpiPlatformProvider,
    MemoryDetector,
    SuperIoHost,
    Southbridge,
    Mainboard,
    PreConsoleInit,
    EarlyInit,
    StageLocalInit,
    PostDramInit,
    FinalizeInit,
    FlashLayoutVerifier,
    FirmwareImageProvider,
    I2cBus,
    SpiBus,
    GpioController,
    SystemManagementBus,
}

impl Service {
    /// All known service variants in stable display order.
    pub const ALL: &'static [Self] = &[
        Self::Console,
        Self::BlockDevice,
        Self::ClockController,
        Self::MemoryController,
        Self::PciRootBus,
        Self::PciHost,
        Self::SmmOps,
        Self::Framebuffer,
        Self::AcpiTableProvider,
        Self::X86AcpiPlatformProvider,
        Self::MemoryDetector,
        Self::SuperIoHost,
        Self::Southbridge,
        Self::Mainboard,
        Self::PreConsoleInit,
        Self::EarlyInit,
        Self::StageLocalInit,
        Self::PostDramInit,
        Self::FinalizeInit,
        Self::FlashLayoutVerifier,
        Self::FirmwareImageProvider,
        Self::I2cBus,
        Self::SpiBus,
        Self::GpioController,
        Self::SystemManagementBus,
    ];

    /// Stable service name used in diagnostics and generated imports.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Console => "Console",
            Self::BlockDevice => "BlockDevice",
            Self::ClockController => "ClockController",
            Self::MemoryController => "MemoryController",
            Self::PciRootBus => "PciRootBus",
            Self::PciHost => "PciHost",
            Self::SmmOps => "SmmOps",
            Self::Framebuffer => "Framebuffer",
            Self::AcpiTableProvider => "AcpiTableProvider",
            Self::X86AcpiPlatformProvider => "X86AcpiPlatformProvider",
            Self::MemoryDetector => "MemoryDetector",
            Self::SuperIoHost => "SuperIoHost",
            Self::Southbridge => "Southbridge",
            Self::Mainboard => "Mainboard",
            Self::PreConsoleInit => "PreConsoleInit",
            Self::EarlyInit => "EarlyInit",
            Self::StageLocalInit => "StageLocalInit",
            Self::PostDramInit => "PostDramInit",
            Self::FinalizeInit => "FinalizeInit",
            Self::FlashLayoutVerifier => "FlashLayoutVerifier",
            Self::FirmwareImageProvider => "FirmwareImageProvider",
            Self::I2cBus => "I2cBus",
            Self::SpiBus => "SpiBus",
            Self::GpioController => "GpioController",
            Self::SystemManagementBus => "SmBus",
        }
    }

    const fn bit(self) -> u128 {
        1u128 << (self as u8)
    }
}

/// Compact set of driver-provided services used by codegen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ServiceSet(u128);

impl ServiceSet {
    /// Construct an empty service set.
    pub const fn empty() -> Self {
        Self(0)
    }

    /// Construct a service set from static driver metadata.
    pub const fn from_static(services: &'static [Service]) -> Self {
        let mut idx = 0;
        let mut bits = 0;
        while idx < services.len() {
            bits |= services[idx].bit();
            idx += 1;
        }
        Self(bits)
    }

    /// Insert a service into the set.
    pub fn insert(&mut self, service: Service) {
        self.0 |= service.bit();
    }

    /// Return a copy of this set with `service` inserted.
    pub const fn with(self, service: Service) -> Self {
        Self(self.0 | service.bit())
    }

    /// Remove a service from the set.
    pub fn remove(&mut self, service: Service) {
        self.0 &= !service.bit();
    }

    /// Return true if the set contains `service`.
    pub const fn contains(self, service: Service) -> bool {
        self.0 & service.bit() != 0
    }

    /// Iterate over services present in this set.
    pub fn iter(self) -> impl Iterator<Item = Service> {
        Service::ALL
            .iter()
            .copied()
            .filter(move |service| self.contains(*service))
    }

    /// Return true if the set contains no services.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

/// Static metadata about a driver.
///
/// Returned by [`DriverInstance::meta()`] to give codegen everything it
/// needs to emit imports, construct devices, and generate accessors
/// without per-driver match arms in the stage generator.
#[derive(Debug, Clone, Copy)]
pub struct DriverMeta {
    /// RON / feature-flag name (e.g., `"ns16550"`).
    pub name: &'static str,
    /// Rust type name of the driver struct (e.g., `"Ns16550"`).
    pub type_name: &'static str,
    /// Full module path to import from (e.g., `"fstart_driver_ns16550"`).
    pub module_path: &'static str,
    /// Rust type name of the config struct (e.g., `"Ns16550Config"`).
    pub config_type: &'static str,
    /// Unconditional service traits this driver implements.
    pub static_services: &'static [Service],
    /// Compatible strings for FDT generation.
    pub compatible: &'static [&'static str],
    /// Whether this driver implements `AcpiDevice` (behind `acpi` feature).
    pub has_acpi: bool,
    /// Whether this driver implements
    /// [`fstart_services::device::BusDevice`] (`true`) vs only
    /// [`fstart_services::device::Device`] (`false`).
    ///
    /// Drives construction codegen: a bus-device child is built with
    /// `BusDevice::new_on_bus(&cfg, &parent)`, a plain-device child (or
    /// a root) with `Device::new(&cfg)`. Plain-device children still
    /// benefit from the parent link for init ordering (see
    /// `ensure_device_ready`) but don't take the parent as an argument.
    pub is_bus_device: bool,
}

/// Codegen construction/lifecycle category for a driver registry entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstructionKind {
    /// Runtime device with a generated driver field.
    Device,
    /// Topology-only structural node.
    Structural,
}

// ---------------------------------------------------------------------------
// DriverInstance — typed enum of all known driver configs
// ---------------------------------------------------------------------------

/// Typed topology role for structural (driverless) device tree nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StructuralKind {
    /// PCI bridge/port grouping children below a PCI root or host.
    PciBridge,
    /// LPC bus branch below a southbridge.
    LpcBus,
    /// SMBus branch below a southbridge.
    SmBus,
    /// Generic topology-only bus branch.
    GenericBus,
}

/// Configuration for structural (driverless) device tree nodes.
///
/// Used by `DriverInstance::Structural`. The node remains topology only and
/// provides no Rust service traits; `kind` records the board-owned topology
/// role so bus hierarchy validation does not need pseudo-services.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuralConfig {
    /// Board-owned topology role for this structural node.
    pub kind: StructuralKind,
}

impl Default for StructuralConfig {
    fn default() -> Self {
        Self {
            kind: StructuralKind::GenericBus,
        }
    }
}

/// Build-time inputs for hardware firmware-image providers.
///
/// Runtime providers read chipset registers.  Host tooling cannot, so it
/// supplies the corresponding build artifacts here (for example an Intel Flash
/// Descriptor blob) plus board-declared flash layout policy.
pub struct BuildFirmwareImageContext<'a> {
    /// Optional board-declared flash layout policy.
    pub flash_layout: Option<&'a fstart_types::memory::FlashLayout>,
    /// Optional Intel Flash Descriptor bytes for descriptor-based SPI flash.
    pub intel_ifd: Option<&'a [u8]>,
}

/// Host-side counterpart to `fstart_services::FirmwareImageProvider`.
///
/// Implemented by the registry enum so codegen/xtask can ask the same hardware
/// driver selection that runtime code uses, while sourcing facts from build
/// artifacts instead of MMIO registers.
pub trait BuildFirmwareImageProvider {
    /// Return this driver's build-time firmware-image mapping, if it provides
    /// one for the current board.
    fn build_firmware_image(
        &self,
        ctx: &BuildFirmwareImageContext<'_>,
    ) -> Result<Option<fstart_services::FirmwareImage>, String>;
}

/// One Rust-owned block-device candidate for firmware-image boot media.
///
/// Used for platforms where hardware boot-source registers select among block
/// devices (for example sunxi eGON). Board RON names only
/// `BootMedia(FirmwareImage(...))`; device candidates and offsets live here as
/// platform/provider metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlatformBootMediaCandidate {
    /// Board device name that provides `BlockDevice`.
    pub device: &'static str,
    /// Byte offset on the device where the FFS image starts.
    pub offset: u64,
    /// Size of the FFS image region in bytes.
    pub size: u64,
}

/// Firmware-image mapping supplied by Rust platform knowledge.
///
/// These are fixed firmware-image windows defined by platform specifications or
/// emulator machine models. They intentionally live in Rust rather than in the
/// `BootMedia` RON capability so board files do not carry raw boot-media MMIO
/// base/size tuples.
pub fn platform_firmware_image(
    board_name: &str,
    platform: fstart_types::Platform,
) -> Option<fstart_services::FirmwareImage> {
    use fstart_types::Platform;

    let image = match (board_name, platform) {
        ("qemu-riscv64" | "qemu-riscv64-multi", Platform::Riscv64) => {
            fstart_services::FirmwareImage::single_window(0x2000_0000, 0x0200_0000)
        }
        ("qemu-aarch64" | "qemu-aarch64-multi" | "qemu-aarch64-uefi", Platform::Aarch64)
        | ("qemu-armv7", Platform::Armv7) => {
            fstart_services::FirmwareImage::single_window(0x0000_0000, 0x0800_0000)
        }
        ("qemu-q35" | "qemu-q35-uefi", Platform::X86_64) => {
            fstart_services::FirmwareImage::single_window(0xff90_0000, 0x006f_f000)
        }
        ("sifive-unmatched", Platform::Riscv64) => {
            fstart_services::FirmwareImage::single_window(0x8000_0000, 0x1000_0000)
        }
        ("sifive-unmatched-hw", Platform::Riscv64) => {
            fstart_services::FirmwareImage::single_window(0x2000_0000, 0x0200_0000)
        }
        _ => return None,
    };
    Some(image)
}

/// Rust-owned boot-source-selected block firmware-image candidates.
pub fn platform_boot_media_candidates(
    board_name: &str,
    platform: fstart_types::Platform,
) -> &'static [PlatformBootMediaCandidate] {
    use fstart_types::Platform;

    match (board_name, platform) {
        ("bananapi-m1", Platform::Armv7) => &[PlatformBootMediaCandidate {
            device: "mmc0",
            offset: 0x2000,
            size: 0x0080_0000,
        }],
        ("orangepi-pc2", Platform::Aarch64) | ("licheerv-dock", Platform::Riscv64) => {
            &[PlatformBootMediaCandidate {
                device: "mmc0",
                offset: 0x2000,
                size: 0x0100_0000,
            }]
        }
        ("orangepi-r1", Platform::Armv7) => &[
            PlatformBootMediaCandidate {
                device: "mmc0",
                offset: 0x2000,
                size: 0x0080_0000,
            },
            PlatformBootMediaCandidate {
                device: "spi0",
                offset: 0,
                size: 0x0100_0000,
            },
        ],
        _ => &[],
    }
}

/// Parsed subset of an Intel Flash Descriptor needed by build tooling.
#[derive(Debug, Clone, Copy)]
pub struct ParsedIntelIfd {
    /// Total SPI flash component size in bytes.
    pub flash_size: u32,
    /// Region table entries indexed like FLREGn.
    pub regions: [Option<(u32, u32)>; 16],
}

/// Parse Intel Flash Descriptor bytes.
pub fn parse_intel_ifd(data: &[u8]) -> Result<ParsedIntelIfd, String> {
    let sig_offset = data
        .windows(4)
        .enumerate()
        .step_by(4)
        .find_map(|(offset, bytes)| {
            let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            (value == 0x0ff0_a55a).then_some(offset)
        })
        .ok_or_else(|| "Intel flash descriptor signature 0x0ff0a55a not found".to_string())?;

    if sig_offset + 8 > data.len() {
        return Err("Intel flash descriptor too small for FLMAP0".to_string());
    }
    let flmap0 = u32::from_le_bytes([
        data[sig_offset + 4],
        data[sig_offset + 5],
        data[sig_offset + 6],
        data[sig_offset + 7],
    ]);

    let fcba = ((flmap0 & 0xff) << 4) as usize;
    let component_count = ((flmap0 >> 8) & 0x3) + 1;
    if fcba + 4 > data.len() {
        return Err(format!(
            "Intel flash descriptor FCBA {fcba:#x} outside descriptor file"
        ));
    }
    let flcomp = u32::from_le_bytes([data[fcba], data[fcba + 1], data[fcba + 2], data[fcba + 3]]);
    let mut flash_size = 1u32 << (19 + (flcomp & 0x7));
    if component_count > 1 {
        flash_size = flash_size.saturating_add(1u32 << (19 + ((flcomp >> 3) & 0x7)));
    }

    let frba = (((flmap0 >> 16) & 0xff) << 4) as usize;
    if frba + 4 > data.len() {
        return Err(format!(
            "Intel flash descriptor FRBA {frba:#x} outside descriptor file"
        ));
    }

    let mut regions = [None; 16];
    for (idx, slot) in regions.iter_mut().enumerate() {
        let off = frba + idx * 4;
        if off + 4 > data.len() {
            break;
        }
        let flreg = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
        let base = (flreg & 0x7fff) << 12;
        let limit = ((flreg >> 16) & 0x7fff) << 12 | 0xfff;
        if limit >= base {
            *slot = Some((base, limit - base + 1));
        }
    }

    Ok(ParsedIntelIfd {
        flash_size,
        regions,
    })
}

/// A driver instance with its typed configuration.
///
/// Each variant carries the driver's own `Config` struct — the same type
/// that `Device::new()` takes.
///
/// Sunxi (Allwinner) drivers that share a unified crate (MMC) use an inner
/// enum config that selects the SoC-specific variant. Drivers with
/// fundamentally different codepaths (CCU, DRAM) stay as separate flat
/// variants.
///
/// Variants are feature-gated to match the driver modules.  On the host
/// (codegen), enable `all-drivers` to parse any board config.  On the
/// target, only the drivers the board actually uses are compiled in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DriverInstance {
    /// Structural (driverless) node — a bus bridge managed by its parent.
    ///
    /// Used for internal chipset sub-functions (PCIe root ports, LPC
    /// bus, SMBus) that exist only to give downstream devices a parent
    /// in the tree. Skipped by the driver init loop.
    Structural(StructuralConfig),

    /// NS16550(A) UART
    #[cfg(feature = "ns16550")]
    Ns16550(ns16550::Ns16550Config),

    /// ARM PL011 UART
    #[cfg(feature = "pl011")]
    Pl011(pl011::Pl011Config),

    /// Synopsys DesignWare APB I2C controller.
    #[cfg(feature = "designware-i2c")]
    DesignwareI2c(designware_i2c::DesignwareI2cConfig),

    /// Allwinner A20 (sun7i) Clock Control Unit.
    #[cfg(feature = "sunxi-a20-ccu")]
    SunxiA20Ccu(sunxi_a20_ccu::SunxiA20CcuConfig),

    /// Allwinner H3/H2+ (sun8i) Clock Control Unit.
    #[cfg(feature = "sunxi-h3-ccu")]
    SunxiH3Ccu(sunxi_h3_ccu::SunxiH3CcuConfig),

    /// Allwinner A20 (sun7i) DRAM controller.
    #[cfg(feature = "sunxi-a20-dramc")]
    SunxiA20Dramc(sunxi_a20_dramc::SunxiA20DramcConfig),

    /// Allwinner H3/H2+ (sun8i) DRAM controller.
    #[cfg(feature = "sunxi-h3-dramc")]
    SunxiH3Dramc(sunxi_h3_dramc::SunxiH3DramcConfig),

    /// Allwinner sunxi SD/MMC controller (unified A20/H3).
    ///
    /// The inner [`SunxiMmcConfig`] enum selects the SoC generation
    /// (Sun7iA20 vs Sun8iH3), which determines clock gating and
    /// FIFO offset differences.
    #[cfg(feature = "sunxi-mmc")]
    SunxiMmc(sunxi_mmc::SunxiMmcConfig),

    /// Allwinner sunxi SPI controller (unified A20/H3).
    ///
    /// The inner [`SunxiSpiConfig`] enum selects the SoC generation
    /// (Sun7iA20 vs Sun8iH3), which determines register layout,
    /// clock gating, and GPIO pin mux differences.
    #[cfg(feature = "sunxi-spi")]
    SunxiSpi(sunxi_spi::SunxiSpiConfig),

    /// Allwinner D1/T113 (sun20i) Clock Control Unit.
    #[cfg(feature = "sunxi-d1-ccu")]
    SunxiD1Ccu(sunxi_d1_ccu::SunxiD1CcuConfig),

    /// Allwinner D1/T113 (sun20i) DRAM controller.
    #[cfg(feature = "sunxi-d1-dramc")]
    SunxiD1Dramc(sunxi_d1_dramc::SunxiD1DramcConfig),

    /// SiFive UART (FU540/FU740).
    #[cfg(feature = "sifive-uart")]
    SifiveUart(sifive_uart::SifiveUartConfig),

    /// SiFive FU740 PRCI clock controller.
    #[cfg(feature = "fu740-prci")]
    Fu740Prci(fu740_prci::Fu740PrciConfig),

    /// SiFive FU740 DDR4 memory controller.
    #[cfg(feature = "fu740-ddr")]
    Fu740Ddr(fu740_ddr::Fu740DdrConfig),

    /// PCI ECAM host bridge with bus enumeration and resource allocation.
    #[cfg(feature = "pci-ecam")]
    PciEcam(pci_ecam::PciEcamConfig),

    /// Bochs VBE display (QEMU bochs-display, PCI MMIO mode).
    #[cfg(feature = "bochs-display")]
    BochsDisplay(bochs_display::BochsDisplayConfig),

    /// QEMU fw_cfg device — provides ACPI tables and e820 memory map.
    #[cfg(feature = "qemu-fw-cfg")]
    QemuFwCfg(qemu_fw_cfg::QemuFwCfgConfig),

    /// Q35 PCI host bridge — ECAM with CF8/CFC bootstrap and runtime
    /// MMIO window computation from e820.
    #[cfg(feature = "q35-hostbridge")]
    Q35HostBridge(q35_hostbridge::Q35HostBridgeConfig),

    /// ITE IT8721F SuperIO — LPC-attached multi-function peripheral.
    #[cfg(feature = "ite8721f")]
    Ite8721f(ite8721f::Ite8721fConfig),

    /// NSC PC87382 SuperIO / DLPC block.
    #[cfg(feature = "nsc-pc87382")]
    NscPc87382(nsc_pc87382::Pc87382Config),

    /// NSC PC87392 SuperIO — dock-side X61 peripheral.
    #[cfg(feature = "nsc-pc87392")]
    NscPc87392(nsc_pc87392::Pc87392Config),

    /// Intel Atom D4xx/D5xx (Pineview) northbridge / MCH.
    #[cfg(feature = "intel-pineview")]
    IntelPineview(intel_pineview::IntelPineviewConfig),

    /// Intel ICH7 / NM10 southbridge.
    #[cfg(feature = "intel-ich7")]
    IntelIch7(intel_ich7::IntelIch7Config),

    /// Intel GM965 (Crestline) northbridge / MCH.
    #[cfg(feature = "intel-gm965")]
    IntelGm965(intel_gm965::IntelGm965Config),

    /// Intel ICH8 / ICH8-M southbridge.
    #[cfg(feature = "intel-ich8")]
    IntelIch8(intel_ich8::IntelIch8Config),

    /// Lenovo ThinkPad X61 mainboard glue.
    #[cfg(feature = "lenovo-x61-mainboard")]
    LenovoX61Mainboard(lenovo_x61_mainboard::LenovoX61MainboardConfig),

    /// IDT CK505 clock generator (SMBus-attached).
    #[cfg(feature = "i2c-ck505")]
    I2cCk505(i2c_ck505::I2cCk505Config),
}

/// A Rust-board runtime driver bound to a named [`fstart_types::DeviceConfig`].
///
/// Migrated Rust board crates use named bindings so their device topology can
/// include driverless structural bus nodes without padding the driver list with
/// fake positional `Structural` entries. Codegen still lowers this into its
/// legacy parallel table internally.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverBinding {
    /// Device name in the board's flat topology table.
    pub device: HString<32>,
    /// Typed runtime driver configuration for that device.
    pub instance: DriverInstance,
}

impl DriverBinding {
    /// Bind a typed driver instance to a board device name.
    #[must_use]
    pub fn new(device: &str, instance: DriverInstance) -> Self {
        let mut name = HString::new();
        name.push_str(device)
            .expect("driver binding device name exceeds capacity");
        Self {
            device: name,
            instance,
        }
    }

    /// Cargo feature for this binding's driver, if it has one.
    #[must_use]
    pub fn driver_feature(&self) -> Option<&'static str> {
        self.instance.driver_feature()
    }
}

impl DriverInstance {
    /// Bind this instance to a named board device.
    #[must_use]
    pub fn bind(self, device: &str) -> DriverBinding {
        DriverBinding::new(device, self)
    }
}

impl BuildFirmwareImageProvider for DriverInstance {
    #[allow(unused_variables)]
    fn build_firmware_image(
        &self,
        ctx: &BuildFirmwareImageContext<'_>,
    ) -> Result<Option<fstart_services::FirmwareImage>, String> {
        match self {
            #[cfg(feature = "intel-ich7")]
            Self::IntelIch7(_) => Ok(Some(fstart_services::FirmwareImage::x86_top_of_4g(
                16 * 1024 * 1024,
            ))),
            #[cfg(feature = "intel-ich8")]
            Self::IntelIch8(_) => build_intel_ifd_firmware_image(ctx),
            _ => Ok(None),
        }
    }
}

fn build_intel_ifd_firmware_image(
    ctx: &BuildFirmwareImageContext<'_>,
) -> Result<Option<fstart_services::FirmwareImage>, String> {
    if let Some(data) = ctx.intel_ifd {
        let parsed = parse_intel_ifd(data)?;
        let bios_idx = fstart_types::memory::IntelIfdRegion::Bios
            .flreg_index()
            .ok_or_else(|| "Intel IFD BIOS region has no FLREG index".to_string())?;
        let Some((_offset, size)) = parsed.regions.get(bios_idx).copied().flatten() else {
            return Err("Intel flash descriptor has no BIOS region".to_string());
        };
        if size == 0 {
            return Err("Intel flash descriptor BIOS region is empty".to_string());
        }
        return Ok(Some(fstart_services::FirmwareImage::single_window(
            0x1_0000_0000u64 - u64::from(size),
            u64::from(size),
        )));
    }

    let Some(fstart_types::memory::FlashLayout::IntelIfd(layout)) = ctx.flash_layout else {
        return Ok(None);
    };
    let bios = layout
        .bios_region()
        .ok_or_else(|| "Intel IFD flash_layout requires a BIOS region".to_string())?;
    Ok(Some(fstart_services::FirmwareImage::single_window(
        layout.base + u64::from(bios.offset),
        u64::from(bios.size),
    )))
}

impl DriverInstance {
    /// Static metadata for this driver variant.
    pub fn meta(&self) -> &'static DriverMeta {
        match self {
            Self::Structural(_) => &DriverMeta {
                name: "structural",
                type_name: "_Structural",
                module_path: "fstart_device_registry",
                config_type: "StructuralConfig",
                static_services: &[],
                compatible: &[],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "ns16550")]
            Self::Ns16550(_) => &DriverMeta {
                name: "ns16550",
                type_name: "Ns16550",
                module_path: "fstart_driver_ns16550",
                config_type: "Ns16550Config",
                static_services: &[Service::Console],
                compatible: &[
                    "ns16550a",
                    "ns16550",
                    "snps,dw-apb-uart",
                    "allwinner,sun7i-a20-uart",
                ],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "pl011")]
            Self::Pl011(_) => &DriverMeta {
                name: "pl011",
                type_name: "Pl011",
                module_path: "fstart_driver_pl011",
                config_type: "Pl011Config",
                static_services: &[Service::Console],
                compatible: &["arm,pl011", "pl011"],
                has_acpi: true,
                is_bus_device: false,
            },
            #[cfg(feature = "designware-i2c")]
            Self::DesignwareI2c(_) => &DriverMeta {
                name: "designware-i2c",
                type_name: "DesignwareI2c",
                module_path: "fstart_driver_designware_i2c",
                config_type: "DesignwareI2cConfig",
                static_services: &[Service::I2cBus],
                compatible: &["snps,designware-i2c", "dw-apb-i2c"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "sunxi-a20-ccu")]
            Self::SunxiA20Ccu(_) => &DriverMeta {
                name: "sunxi-a20-ccu",
                type_name: "SunxiA20Ccu",
                module_path: "fstart_driver_sunxi_ccu",
                config_type: "SunxiA20CcuConfig",
                static_services: &[Service::ClockController],
                compatible: &["allwinner,sun7i-a20-ccu"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "sunxi-h3-ccu")]
            Self::SunxiH3Ccu(_) => &DriverMeta {
                name: "sunxi-h3-ccu",
                type_name: "SunxiH3Ccu",
                module_path: "fstart_driver_sunxi_h3_ccu",
                config_type: "SunxiH3CcuConfig",
                static_services: &[Service::ClockController],
                compatible: &["allwinner,sun8i-h3-ccu"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "sunxi-a20-dramc")]
            Self::SunxiA20Dramc(_) => &DriverMeta {
                name: "sunxi-a20-dramc",
                type_name: "SunxiA20Dramc",
                module_path: "fstart_driver_sunxi_a20_dramc",
                config_type: "SunxiA20DramcConfig",
                static_services: &[Service::MemoryController],
                compatible: &["allwinner,sun7i-a20-dramc"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "sunxi-h3-dramc")]
            Self::SunxiH3Dramc(_) => &DriverMeta {
                name: "sunxi-h3-dramc",
                type_name: "SunxiH3Dramc",
                module_path: "fstart_driver_sunxi_h3_dramc",
                config_type: "SunxiH3DramcConfig",
                static_services: &[Service::MemoryController],
                compatible: &["allwinner,sun8i-h3-dramc", "allwinner,sun50i-h5-dramc"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "sunxi-mmc")]
            Self::SunxiMmc(_) => &DriverMeta {
                name: "sunxi-mmc",
                type_name: "SunxiMmc",
                module_path: "fstart_driver_sunxi_mmc",
                config_type: "SunxiMmcConfig",
                static_services: &[Service::BlockDevice],
                compatible: &[
                    "allwinner,sun7i-a20-mmc",
                    "allwinner,sun8i-h3-mmc",
                    "allwinner,sun50i-h5-mmc",
                ],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "sunxi-spi")]
            Self::SunxiSpi(_) => &DriverMeta {
                name: "sunxi-spi",
                type_name: "SunxiSpi",
                module_path: "fstart_driver_sunxi_spi",
                config_type: "SunxiSpiConfig",
                static_services: &[Service::BlockDevice],
                compatible: &["allwinner,sun4i-a10-spi", "allwinner,sun8i-h3-spi"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "sunxi-d1-ccu")]
            Self::SunxiD1Ccu(_) => &DriverMeta {
                name: "sunxi-d1-ccu",
                type_name: "SunxiD1Ccu",
                module_path: "fstart_driver_sunxi_d1_ccu",
                config_type: "SunxiD1CcuConfig",
                static_services: &[Service::ClockController],
                compatible: &["allwinner,sun20i-d1-ccu"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "sunxi-d1-dramc")]
            Self::SunxiD1Dramc(_) => &DriverMeta {
                name: "sunxi-d1-dramc",
                type_name: "SunxiD1Dramc",
                module_path: "fstart_driver_sunxi_d1_dramc",
                config_type: "SunxiD1DramcConfig",
                static_services: &[Service::MemoryController],
                compatible: &["allwinner,sun20i-d1-mbus"],
                has_acpi: false,
                is_bus_device: false,
            },

            #[cfg(feature = "sifive-uart")]
            Self::SifiveUart(_) => &DriverMeta {
                name: "sifive-uart",
                type_name: "SifiveUart",
                module_path: "fstart_driver_sifive_uart",
                config_type: "SifiveUartConfig",
                static_services: &[Service::Console],
                compatible: &["sifive,fu740-c000-uart", "sifive,uart0"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "fu740-prci")]
            Self::Fu740Prci(_) => &DriverMeta {
                name: "fu740-prci",
                type_name: "Fu740Prci",
                module_path: "fstart_driver_fu740_prci",
                config_type: "Fu740PrciConfig",
                static_services: &[Service::ClockController],
                compatible: &["sifive,fu740-c000-prci"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "fu740-ddr")]
            Self::Fu740Ddr(_) => &DriverMeta {
                name: "fu740-ddr",
                type_name: "Fu740Ddr",
                module_path: "fstart_driver_fu740_ddr",
                config_type: "Fu740DdrConfig",
                static_services: &[Service::MemoryController],
                compatible: &["sifive,fu740-c000-ddr"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "pci-ecam")]
            Self::PciEcam(_) => &DriverMeta {
                name: "pci-ecam",
                type_name: "PciEcam",
                module_path: "fstart_driver_pci_ecam",
                config_type: "PciEcamConfig",
                static_services: &[Service::PciRootBus],
                compatible: &["pci-host-ecam-generic"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "bochs-display")]
            Self::BochsDisplay(_) => &DriverMeta {
                name: "bochs-display",
                type_name: "BochsDisplay",
                module_path: "fstart_driver_bochs_display",
                config_type: "BochsDisplayConfig",
                static_services: &[Service::Framebuffer],
                compatible: &["bochs-display", "qemu-stdvga"],
                has_acpi: false,
                is_bus_device: true,
            },
            #[cfg(feature = "qemu-fw-cfg")]
            Self::QemuFwCfg(_) => &DriverMeta {
                name: "qemu-fw-cfg",
                type_name: "QemuFwCfg",
                module_path: "fstart_driver_qemu_fw_cfg",
                config_type: "QemuFwCfgConfig",
                static_services: &[Service::AcpiTableProvider, Service::MemoryDetector],
                compatible: &["qemu,fw-cfg"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "q35-hostbridge")]
            Self::Q35HostBridge(_) => &DriverMeta {
                name: "q35-hostbridge",
                type_name: "Q35HostBridge",
                module_path: "fstart_driver_q35_hostbridge",
                config_type: "Q35HostBridgeConfig",
                static_services: &[Service::PciRootBus, Service::SmmOps],
                compatible: &["q35-hostbridge"],
                has_acpi: false,
                is_bus_device: false,
            },
            #[cfg(feature = "ite8721f")]
            Self::Ite8721f(_) => &DriverMeta {
                name: "ite8721f",
                type_name: "Ite8721f",
                module_path: "fstart_driver_ite8721f",
                config_type: "Ite8721fConfig",
                // SuperIOs always expose `SuperIoHost` for init-ordering of
                // children. `Console` is config-dependent: provided_services()
                // adds it only when `console_port` is set.
                static_services: &[Service::SuperIoHost],
                compatible: &["ite,it8721f", "ite,8721f"],
                has_acpi: true,
                is_bus_device: true,
            },
            #[cfg(feature = "nsc-pc87382")]
            Self::NscPc87382(_) => &DriverMeta {
                name: "nsc-pc87382",
                type_name: "Pc87382",
                module_path: "fstart_driver_nsc_pc87382",
                config_type: "Pc87382Config",
                static_services: &[Service::SuperIoHost],
                compatible: &["nsc,pc87382"],
                has_acpi: true,
                is_bus_device: true,
            },
            #[cfg(feature = "nsc-pc87392")]
            Self::NscPc87392(_) => &DriverMeta {
                name: "nsc-pc87392",
                type_name: "Pc87392",
                module_path: "fstart_driver_nsc_pc87392",
                config_type: "Pc87392Config",
                static_services: &[Service::SuperIoHost],
                compatible: &["nsc,pc87392"],
                has_acpi: true,
                is_bus_device: true,
            },
            #[cfg(feature = "intel-pineview")]
            Self::IntelPineview(_) => &DriverMeta {
                name: "intel-pineview",
                type_name: "IntelPineview",
                module_path: "fstart_driver_intel_pineview",
                config_type: "IntelPineviewConfig",
                static_services: &[
                    Service::MemoryController,
                    Service::MemoryDetector,
                    Service::PciHost,
                    Service::PciRootBus,
                    Service::SmmOps,
                    Service::PreConsoleInit,
                    Service::EarlyInit,
                    Service::StageLocalInit,
                ],
                compatible: &["intel,pineview-mch", "intel,atom-d4xx-mch"],
                has_acpi: true,
                is_bus_device: false,
            },
            #[cfg(feature = "intel-ich7")]
            Self::IntelIch7(_) => &DriverMeta {
                name: "intel-ich7",
                type_name: "IntelIch7",
                module_path: "fstart_driver_intel_ich7",
                config_type: "IntelIch7Config",
                static_services: &[
                    Service::Southbridge,
                    Service::PreConsoleInit,
                    Service::EarlyInit,
                    Service::PostDramInit,
                    Service::FinalizeInit,
                    Service::FirmwareImageProvider,
                    Service::X86AcpiPlatformProvider,
                    Service::SystemManagementBus,
                ],
                compatible: &["intel,ich7", "intel,nm10"],
                has_acpi: true,
                is_bus_device: false,
            },
            #[cfg(feature = "intel-gm965")]
            Self::IntelGm965(_) => &DriverMeta {
                name: "intel-gm965",
                type_name: "IntelGm965",
                module_path: "fstart_driver_intel_gm965",
                config_type: "IntelGm965Config",
                static_services: &[
                    Service::MemoryController,
                    Service::MemoryDetector,
                    Service::PciHost,
                    Service::PciRootBus,
                    Service::SmmOps,
                    Service::PreConsoleInit,
                    Service::EarlyInit,
                    Service::StageLocalInit,
                    Service::PostDramInit,
                ],
                compatible: &["intel,gm965", "intel,crestline"],
                has_acpi: true,
                is_bus_device: false,
            },
            #[cfg(feature = "intel-ich8")]
            Self::IntelIch8(_) => &DriverMeta {
                name: "intel-ich8",
                type_name: "IntelIch8",
                module_path: "fstart_driver_intel_ich8",
                config_type: "IntelIch8Config",
                static_services: &[
                    Service::Southbridge,
                    Service::PreConsoleInit,
                    Service::EarlyInit,
                    Service::PostDramInit,
                    Service::FinalizeInit,
                    Service::FlashLayoutVerifier,
                    Service::FirmwareImageProvider,
                    Service::X86AcpiPlatformProvider,
                    Service::SystemManagementBus,
                ],
                compatible: &["intel,ich8", "intel,ich8m", "intel,82801hx"],
                has_acpi: true,
                is_bus_device: false,
            },
            #[cfg(feature = "lenovo-x61-mainboard")]
            Self::LenovoX61Mainboard(_) => &DriverMeta {
                name: "lenovo-x61-mainboard",
                type_name: "LenovoX61Mainboard",
                module_path: "fstart_mainboard_lenovo_x61",
                config_type: "LenovoX61MainboardConfig",
                static_services: &[
                    Service::Mainboard,
                    Service::PreConsoleInit,
                    Service::PostDramInit,
                    Service::FinalizeInit,
                ],
                compatible: &["lenovo,thinkpad-x61"],
                has_acpi: true,
                is_bus_device: false,
            },
            #[cfg(feature = "i2c-ck505")]
            Self::I2cCk505(_) => &DriverMeta {
                name: "i2c-ck505",
                type_name: "I2cCk505",
                module_path: "fstart_driver_i2c_ck505",
                config_type: "I2cCk505Config",
                static_services: &[],
                compatible: &["idt,ck505"],
                has_acpi: false,
                is_bus_device: true,
            },
        }
    }

    /// Services provided by this concrete driver instance.
    ///
    /// This method is the source of truth for service availability. It may
    /// inspect typed config for config-dependent services.
    pub fn provided_services(&self) -> ServiceSet {
        let services = ServiceSet::from_static(self.meta().static_services);
        match self {
            #[cfg(feature = "ite8721f")]
            Self::Ite8721f(cfg) if cfg.console_port.is_some() => services.with(Service::Console),
            #[cfg(feature = "nsc-pc87382")]
            Self::NscPc87382(cfg) if cfg.console_port.is_some() => services.with(Service::Console),
            #[cfg(feature = "nsc-pc87392")]
            Self::NscPc87392(cfg) if cfg.console_port.is_some() => services.with(Service::Console),
            _ => services,
        }
    }

    /// Returns `true` when this concrete instance provides `service`.
    pub fn provides(&self, service: Service) -> bool {
        self.provided_services().contains(service)
    }

    /// The cargo feature / RON driver name for this runtime driver variant.
    ///
    /// Structural instances do not correspond to target-side driver features.
    pub fn driver_feature(&self) -> Option<&'static str> {
        if !self.has_runtime_driver() {
            None
        } else {
            Some(self.meta().name)
        }
    }

    /// The RON/registry name for this variant.
    pub fn driver_name(&self) -> &'static str {
        self.meta().name
    }

    /// Return the ACPI namespace name if this instance has one configured.
    ///
    /// Checks the driver's config for an `acpi_name` field with a `Some`
    /// value.  Only drivers whose configs have optional ACPI fields
    /// (e.g., PL011 with `acpi_name: Option<HString<8>>`) will return
    /// `Some`.  All others return `None`.
    pub fn acpi_name(&self) -> Option<&str> {
        match self {
            #[cfg(feature = "pl011")]
            Self::Pl011(cfg) => cfg.acpi_name.as_deref(),
            #[cfg(feature = "ite8721f")]
            Self::Ite8721f(cfg) => cfg.acpi_name.as_deref(),
            #[cfg(feature = "intel-pineview")]
            Self::IntelPineview(cfg) => cfg.acpi_name.as_deref(),
            #[cfg(feature = "intel-ich7")]
            Self::IntelIch7(cfg) => cfg.acpi_name.as_deref(),
            #[cfg(feature = "intel-gm965")]
            Self::IntelGm965(cfg) => cfg.acpi_name.as_deref(),
            #[cfg(feature = "intel-ich8")]
            Self::IntelIch8(cfg) => cfg.acpi_name.as_deref(),
            #[cfg(feature = "lenovo-x61-mainboard")]
            Self::LenovoX61Mainboard(cfg) => cfg.acpi_name.as_deref(),
            _ => None,
        }
    }

    /// Codegen construction/lifecycle category for this registry entry.
    pub fn construction_kind(&self) -> ConstructionKind {
        #[allow(unreachable_patterns)]
        match self {
            Self::Structural(_) => ConstructionKind::Structural,
            _ => ConstructionKind::Device,
        }
    }

    /// Returns `true` if this instance has a runtime driver field in generated
    /// stage code.
    pub fn has_runtime_driver(&self) -> bool {
        self.construction_kind() == ConstructionKind::Device
    }

    /// Returns `true` if this driver provides the runtime PCI root-bus service.
    pub fn provides_pci_root(&self) -> bool {
        self.provides(Service::PciRootBus)
    }

    /// Return the SoC boot-source register values that select this device.
    ///
    /// Used by stage codegen to emit match arms for runtime boot-device
    /// auto-detection (e.g. sunxi eGON `boot_media` byte).
    /// Returns an empty `Vec` for drivers that have no boot-source
    /// mapping (non-sunxi platforms, or devices that aren't boot media).
    ///
    /// The constants here mirror `fstart_soc_sunxi::BOOT_MEDIA_*` so
    /// that the codegen (host-side, `std`) can use them without depending
    /// on the `no_std` SoC crate.
    pub fn boot_media_values(&self) -> Vec<u8> {
        match self {
            #[cfg(feature = "sunxi-mmc")]
            Self::SunxiMmc(cfg) => match cfg.mmc_index() {
                0 => vec![0x00, 0x10], // MMC0, MMC0_HIGH
                2 => vec![0x02, 0x12], // MMC2, MMC2_HIGH
                _ => Vec::new(),
            },
            #[cfg(feature = "sunxi-spi")]
            Self::SunxiSpi(_) => vec![0x03], // SPI
            _ => Vec::new(),
        }
    }

    /// Return the build-time firmware-image mapping for this driver.
    pub fn build_firmware_image(
        &self,
        ctx: &BuildFirmwareImageContext<'_>,
    ) -> Result<Option<fstart_services::FirmwareImage>, String> {
        <Self as BuildFirmwareImageProvider>::build_firmware_image(self, ctx)
    }

    /// Serialize just the inner config struct via the given serializer.
    ///
    /// This enables generic config-to-tokens conversion in `fstart-codegen`
    /// without per-driver match arms there.
    pub fn serialize_config<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Structural(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "ns16550")]
            Self::Ns16550(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "pl011")]
            Self::Pl011(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "designware-i2c")]
            Self::DesignwareI2c(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-a20-ccu")]
            Self::SunxiA20Ccu(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-h3-ccu")]
            Self::SunxiH3Ccu(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-a20-dramc")]
            Self::SunxiA20Dramc(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-h3-dramc")]
            Self::SunxiH3Dramc(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-mmc")]
            Self::SunxiMmc(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-spi")]
            Self::SunxiSpi(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-d1-ccu")]
            Self::SunxiD1Ccu(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-d1-dramc")]
            Self::SunxiD1Dramc(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sifive-uart")]
            Self::SifiveUart(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "fu740-prci")]
            Self::Fu740Prci(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "fu740-ddr")]
            Self::Fu740Ddr(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "pci-ecam")]
            Self::PciEcam(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "bochs-display")]
            Self::BochsDisplay(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "qemu-fw-cfg")]
            Self::QemuFwCfg(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "q35-hostbridge")]
            Self::Q35HostBridge(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "ite8721f")]
            Self::Ite8721f(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "nsc-pc87382")]
            Self::NscPc87382(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "nsc-pc87392")]
            Self::NscPc87392(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "intel-pineview")]
            Self::IntelPineview(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "intel-ich7")]
            Self::IntelIch7(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "intel-gm965")]
            Self::IntelGm965(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "intel-ich8")]
            Self::IntelIch8(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "lenovo-x61-mainboard")]
            Self::LenovoX61Mainboard(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "i2c-ck505")]
            Self::I2cCk505(cfg) => serde::Serialize::serialize(cfg, ser),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_set_iterates_inserted_services() {
        let mut services = ServiceSet::empty();
        services.insert(Service::Console);
        services.insert(Service::BlockDevice);

        let collected: Vec<_> = services.iter().collect();
        assert_eq!(collected, vec![Service::Console, Service::BlockDevice]);
    }

    #[test]
    fn service_all_covers_every_bit_position() {
        for service in Service::ALL {
            let mut services = ServiceSet::empty();
            services.insert(*service);
            assert_eq!(services.iter().count(), 1);
            assert_eq!(services.iter().next(), Some(*service));
        }
    }

    #[test]
    fn structural_reports_structural_construction_kind() {
        let inst = DriverInstance::Structural(StructuralConfig::default());
        assert_eq!(inst.construction_kind(), ConstructionKind::Structural);
        assert!(!inst.has_runtime_driver());
        assert!(inst.provided_services().is_empty());
    }

    #[cfg(feature = "ns16550")]
    #[test]
    fn runtime_driver_reports_device_construction_kind() {
        let inst = DriverInstance::Ns16550(ns16550::Ns16550Config {
            regs: ns16550::AccessMode::Mmio {
                base: 0x1000_0000,
                reg_shift: 0,
                reg_width: 0,
            },
            clock_freq: 3_686_400,
            baud_rate: 115_200,
        });
        assert_eq!(inst.construction_kind(), ConstructionKind::Device);
        assert!(inst.has_runtime_driver());
        assert!(inst.provides(Service::Console));
    }

    #[cfg(feature = "ite8721f")]
    #[test]
    fn superio_console_service_depends_on_console_port() {
        let without_console = DriverInstance::Ite8721f(ite8721f::Ite8721fConfig::default());
        assert!(without_console.provides(Service::SuperIoHost));
        assert!(!without_console.provides(Service::Console));

        let mut cfg = ite8721f::Ite8721fConfig::default();
        cfg.console_port = Some(heapless::String::try_from("com1").unwrap());
        let with_console = DriverInstance::Ite8721f(cfg);
        assert!(with_console.provides(Service::Console));
    }

    #[cfg(feature = "nsc-pc87382")]
    #[test]
    fn pc87382_console_service_depends_on_console_port() {
        let without_console = DriverInstance::NscPc87382(nsc_pc87382::Pc87382Config::default());
        assert!(without_console.provides(Service::SuperIoHost));
        assert!(!without_console.provides(Service::Console));

        let mut cfg = nsc_pc87382::Pc87382Config::default();
        cfg.console_port = Some(heapless::String::try_from("com2").unwrap());
        let with_console = DriverInstance::NscPc87382(cfg);
        assert!(with_console.provides(Service::Console));
    }

    #[cfg(feature = "nsc-pc87392")]
    #[test]
    fn pc87392_console_service_depends_on_console_port() {
        let without_console = DriverInstance::NscPc87392(nsc_pc87392::Pc87392Config::default());
        assert!(without_console.provides(Service::SuperIoHost));
        assert!(!without_console.provides(Service::Console));

        let mut cfg = nsc_pc87392::Pc87392Config::default();
        cfg.console_port = Some(heapless::String::try_from("com1").unwrap());
        let with_console = DriverInstance::NscPc87392(cfg);
        assert!(with_console.provides(Service::Console));
    }

    #[cfg(feature = "intel-ich7")]
    #[test]
    fn ich7_reports_runtime_smbus_service() {
        let inst = DriverInstance::IntelIch7(intel_ich7::IntelIch7Config {
            rcba: 0xfed1_0000,
            pirq_routing: [0; 8],
            gpe0_en: 0,
            lpc_decode: Default::default(),
            hda: None,
            sata: None,
            usb: None,
            pata: false,
            smbus_base: 0x0400,
            gpio: Default::default(),
            acpi_name: None,
            c3_latency: 85,
            power_on_after_fail: 0,
        });
        assert!(inst.provides(Service::Southbridge));
        assert!(inst.provides(Service::SystemManagementBus));
    }

    #[cfg(feature = "intel-ich8")]
    #[test]
    fn ich8_reports_runtime_smbus_service() {
        let inst = DriverInstance::IntelIch8(intel_ich8::IntelIch8Config {
            rcba: 0xfed1_0000,
            dmibar: 0xfed1_8000,
            pirq_routing: [0; 8],
            gpe0_en: 0,
            gpi_routing: [0; 16],
            alt_gp_smi_en: 0,
            c4_on_c3: false,
            c5_enable: false,
            c6_enable: false,
            lpc_decode: Default::default(),
            hda: None,
            ide: None,
            sata: None,
            usb: None,
            pcie_ports: [true; 6],
            pcie_slots: [false; 6],
            pcie_power_limits: [Default::default(); 6],
            io_traps: Default::default(),
            smbus_base: 0x0400,
            gpio: Default::default(),
            acpi_name: None,
            c3_latency: 85,
            power_on_after_fail: 0,
            throttle_duty: 0,
            disable_lan: false,
            disable_sata2: true,
            disable_thermal: true,
        });
        assert!(inst.provides(Service::Southbridge));
        assert!(inst.provides(Service::SystemManagementBus));
    }
}
