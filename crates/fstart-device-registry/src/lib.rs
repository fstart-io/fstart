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

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Re-export driver config types (conditionally based on features)
// ---------------------------------------------------------------------------

#[cfg(feature = "ns16550")]
pub mod ns16550 {
    pub use fstart_driver_ns16550::Ns16550Config;
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

#[cfg(feature = "intel-pineview")]
pub mod intel_pineview {
    pub use fstart_driver_intel_pineview::IntelPineviewConfig;
}

#[cfg(feature = "intel-ich7")]
pub mod intel_ich7 {
    pub use fstart_driver_intel_ich7::IntelIch7Config;
}

#[cfg(feature = "i2c-ck505")]
pub mod i2c_ck505 {
    pub use fstart_driver_i2c_ck505::I2cCk505Config;
}

// ---------------------------------------------------------------------------
// DriverMeta — static metadata about a driver
// ---------------------------------------------------------------------------

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
    /// Service traits this driver implements.
    pub services: &'static [&'static str],
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

// ---------------------------------------------------------------------------
// DriverInstance — typed enum of all known driver configs
// ---------------------------------------------------------------------------

/// Empty configuration for structural (driverless) device tree nodes.
///
/// Used by `DriverInstance::Structural`. Carries no data — the node's
/// identity is fully captured by `DeviceConfig` (name, parent, bus,
/// services). Needed so the parallel `driver_instances` array stays
/// aligned with `devices`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuralConfig {}

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

    // -----------------------------------------------------------------
    // ACPI-only devices — no runtime driver, only contribute ACPI tables
    // -----------------------------------------------------------------
    /// AHCI SATA controller (ACPI-only, no runtime driver).
    Ahci(fstart_types::acpi::AcpiAhciDevice),

    /// xHCI USB controller (ACPI-only, no runtime driver).
    Xhci(fstart_types::acpi::AcpiXhciDevice),

    /// PCIe Root Complex (ACPI-only, no runtime driver).
    PcieRoot(fstart_types::acpi::AcpiPcieRootDevice),

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

    /// Intel Atom D4xx/D5xx (Pineview) northbridge / MCH.
    #[cfg(feature = "intel-pineview")]
    IntelPineview(intel_pineview::IntelPineviewConfig),

    /// Intel ICH7 / NM10 southbridge.
    #[cfg(feature = "intel-ich7")]
    IntelIch7(intel_ich7::IntelIch7Config),

    /// IDT CK505 clock generator (SMBus-attached).
    #[cfg(feature = "i2c-ck505")]
    I2cCk505(i2c_ck505::I2cCk505Config),
}

impl DriverInstance {
    /// Static metadata for this driver variant.
    pub fn meta(&self) -> &'static DriverMeta {
        match self {
            Self::Structural(_) => &DriverMeta {
                name: "_structural",
                type_name: "_Structural",
                module_path: "fstart_device_registry",
                config_type: "StructuralConfig",
                services: &[],
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
                services: &["Console"],
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
                services: &["Console"],
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
                services: &["I2cBus"],
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
                services: &["ClockController"],
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
                services: &["ClockController"],
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
                services: &["MemoryController"],
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
                services: &["MemoryController"],
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
                services: &["BlockDevice"],
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
                services: &["BlockDevice"],
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
                services: &["ClockController"],
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
                services: &["MemoryController"],
                compatible: &["allwinner,sun20i-d1-mbus"],
                has_acpi: false,
                is_bus_device: false,
            },
            Self::Ahci(_) => &DriverMeta {
                name: "ahci",
                type_name: "AcpiAhciDevice",
                module_path: "fstart_types::acpi",
                config_type: "AcpiAhciDevice",
                services: &[],
                compatible: &[],
                has_acpi: true,
                is_bus_device: false,
            },
            Self::Xhci(_) => &DriverMeta {
                name: "xhci",
                type_name: "AcpiXhciDevice",
                module_path: "fstart_types::acpi",
                config_type: "AcpiXhciDevice",
                services: &[],
                compatible: &[],
                has_acpi: true,
                is_bus_device: false,
            },
            Self::PcieRoot(_) => &DriverMeta {
                name: "pcie-root",
                type_name: "AcpiPcieRootDevice",
                module_path: "fstart_types::acpi",
                config_type: "AcpiPcieRootDevice",
                services: &[],
                compatible: &[],
                has_acpi: true,
                is_bus_device: false,
            },
            #[cfg(feature = "sifive-uart")]
            Self::SifiveUart(_) => &DriverMeta {
                name: "sifive-uart",
                type_name: "SifiveUart",
                module_path: "fstart_driver_sifive_uart",
                config_type: "SifiveUartConfig",
                services: &["Console"],
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
                services: &["ClockController"],
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
                services: &["MemoryController"],
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
                services: &["PciRootBus"],
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
                services: &["Framebuffer"],
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
                services: &["AcpiTableProvider", "MemoryDetector"],
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
                services: &["PciRootBus"],
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
                // SuperIOs program LDNs and optionally provide Console
                // directly when `console_port` is set — no separate
                // NS16550 child needed.  Also expose `SuperIoHost` for
                // init-ordering of any remaining children.
                services: &["SuperIoHost", "Console"],
                compatible: &["ite,it8721f", "ite,8721f"],
                has_acpi: true,
                is_bus_device: true,
            },
            #[cfg(feature = "intel-pineview")]
            Self::IntelPineview(_) => &DriverMeta {
                name: "intel-pineview",
                type_name: "IntelPineview",
                module_path: "fstart_driver_intel_pineview",
                config_type: "IntelPineviewConfig",
                services: &["MemoryController", "PciHost"],
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
                services: &["Southbridge"],
                compatible: &["intel,ich7", "intel,nm10"],
                has_acpi: true,
                is_bus_device: false,
            },
            #[cfg(feature = "i2c-ck505")]
            Self::I2cCk505(_) => &DriverMeta {
                name: "i2c-ck505",
                type_name: "I2cCk505",
                module_path: "fstart_driver_i2c_ck505",
                config_type: "I2cCk505Config",
                services: &[],
                compatible: &["idt,ck505"],
                has_acpi: false,
                is_bus_device: true,
            },
        }
    }

    /// The cargo feature / RON driver name for this variant.
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
            Self::Ahci(cfg) => Some(cfg.name.as_str()),
            Self::Xhci(cfg) => Some(cfg.name.as_str()),
            Self::PcieRoot(cfg) => Some(cfg.name.as_str()),
            _ => None,
        }
    }

    /// Returns `true` if this is an ACPI-only device (no runtime driver).
    ///
    /// ACPI-only devices are skipped by `DriverInit` and device construction
    /// in the generated stage code. They only contribute ACPI table entries.
    pub fn is_acpi_only(&self) -> bool {
        matches!(self, Self::Ahci(_) | Self::Xhci(_) | Self::PcieRoot(_))
    }

    /// Returns `true` if this is a structural (driverless) bus node.
    ///
    /// Structural nodes exist in the device tree to give downstream
    /// devices a parent, but have no driver code. They are skipped by
    /// device construction and `DriverInit`.
    pub fn is_structural(&self) -> bool {
        matches!(self, Self::Structural(_))
    }

    /// Return the SoC boot-source register values that select this device.
    ///
    /// Used by `plan_gen` and `board_gen` to emit match arms for runtime
    /// boot-device auto-detection (e.g. sunxi eGON `boot_media` byte).
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
            Self::Ahci(cfg) => serde::Serialize::serialize(cfg, ser),
            Self::Xhci(cfg) => serde::Serialize::serialize(cfg, ser),
            Self::PcieRoot(cfg) => serde::Serialize::serialize(cfg, ser),
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
            #[cfg(feature = "intel-pineview")]
            Self::IntelPineview(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "intel-ich7")]
            Self::IntelIch7(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "i2c-ck505")]
            Self::I2cCk505(cfg) => serde::Serialize::serialize(cfg, ser),
        }
    }
}
