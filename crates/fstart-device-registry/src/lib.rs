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

pub use fstart_board_meta::{StructuralConfig, StructuralKind};

mod driver_configs;
pub use driver_configs::*;

// DriverMeta — static metadata about a driver
// ---------------------------------------------------------------------------

/// Service traits a driver instance can provide.
///
/// Kept as a compatibility alias while board metadata moves out of this crate.
pub use fstart_services::ServiceKind as Service;

/// Compact set of driver-provided services used by host tooling.
///
/// Kept as a compatibility re-export while call sites migrate to `fstart-services`.
pub use fstart_services::ServiceSet;

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

mod boot_media;
pub use boot_media::*;

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

mod binding;
pub use binding::*;

mod superio;
pub use superio::*;

mod topology;
pub use topology::*;

mod metadata;

#[cfg(test)]
#[cfg(test)]
mod tests;
