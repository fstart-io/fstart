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

#[cfg(feature = "sunxi-a20-dramc")]
pub mod sunxi_a20_dramc {
    pub use fstart_driver_sunxi_dramc::SunxiA20DramcConfig;
}

#[cfg(feature = "sunxi-a20-mmc")]
pub mod sunxi_a20_mmc {
    pub use fstart_driver_sunxi_mmc::SunxiA20MmcConfig;
}

#[cfg(feature = "sunxi-a20-spi")]
pub mod sunxi_a20_spi {
    pub use fstart_driver_sunxi_spi::SunxiA20SpiConfig;
}

#[cfg(feature = "sunxi-h3-ccu")]
pub mod sunxi_h3_ccu {
    pub use fstart_driver_sunxi_h3_ccu::SunxiH3CcuConfig;
}

#[cfg(feature = "sunxi-h3-dramc")]
pub mod sunxi_h3_dramc {
    pub use fstart_driver_sunxi_h3_dramc::SunxiH3DramcConfig;
}

#[cfg(feature = "sunxi-h3-mmc")]
pub mod sunxi_h3_mmc {
    pub use fstart_driver_sunxi_h3_mmc::SunxiH3MmcConfig;
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
}

// ---------------------------------------------------------------------------
// DriverInstance — typed enum of all known driver configs
// ---------------------------------------------------------------------------

/// A driver instance with its typed configuration.
///
/// Each variant carries the driver's own `Config` struct — the same type
/// that `Device::new()` takes.
///
/// Variants are feature-gated to match the driver modules.  On the host
/// (codegen), enable `all-drivers` to parse any board config.  On the
/// target, only the drivers the board actually uses are compiled in.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DriverInstance {
    /// NS16550(A) UART
    #[cfg(feature = "ns16550")]
    Ns16550(ns16550::Ns16550Config),

    /// ARM PL011 UART
    #[cfg(feature = "pl011")]
    Pl011(pl011::Pl011Config),

    /// Synopsys DesignWare APB I2C controller.
    #[cfg(feature = "designware-i2c")]
    DesignwareI2c(designware_i2c::DesignwareI2cConfig),

    /// Allwinner A20 Clock Control Unit
    #[cfg(feature = "sunxi-a20-ccu")]
    SunxiA20Ccu(sunxi_a20_ccu::SunxiA20CcuConfig),

    /// Allwinner A20 DRAM controller
    #[cfg(feature = "sunxi-a20-dramc")]
    SunxiA20Dramc(sunxi_a20_dramc::SunxiA20DramcConfig),

    /// Allwinner A20 SD/MMC controller
    #[cfg(feature = "sunxi-a20-mmc")]
    SunxiA20Mmc(sunxi_a20_mmc::SunxiA20MmcConfig),

    /// Allwinner A20 SPI NOR flash (sun4i SPI controller)
    #[cfg(feature = "sunxi-a20-spi")]
    SunxiA20Spi(sunxi_a20_spi::SunxiA20SpiConfig),

    /// Allwinner H3 Clock Control Unit
    #[cfg(feature = "sunxi-h3-ccu")]
    SunxiH3Ccu(sunxi_h3_ccu::SunxiH3CcuConfig),

    /// Allwinner H3 DRAM controller (DesignWare)
    #[cfg(feature = "sunxi-h3-dramc")]
    SunxiH3Dramc(sunxi_h3_dramc::SunxiH3DramcConfig),

    /// Allwinner H3 SD/MMC controller
    #[cfg(feature = "sunxi-h3-mmc")]
    SunxiH3Mmc(sunxi_h3_mmc::SunxiH3MmcConfig),
}

impl DriverInstance {
    /// Static metadata for this driver variant.
    pub fn meta(&self) -> &'static DriverMeta {
        match self {
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
            },
            #[cfg(feature = "pl011")]
            Self::Pl011(_) => &DriverMeta {
                name: "pl011",
                type_name: "Pl011",
                module_path: "fstart_driver_pl011",
                config_type: "Pl011Config",
                services: &["Console"],
                compatible: &["arm,pl011", "pl011"],
            },
            #[cfg(feature = "designware-i2c")]
            Self::DesignwareI2c(_) => &DriverMeta {
                name: "designware-i2c",
                type_name: "DesignwareI2c",
                module_path: "fstart_driver_designware_i2c",
                config_type: "DesignwareI2cConfig",
                services: &["I2cBus"],
                compatible: &["snps,designware-i2c", "dw-apb-i2c"],
            },
            #[cfg(feature = "sunxi-a20-ccu")]
            Self::SunxiA20Ccu(_) => &DriverMeta {
                name: "sunxi-a20-ccu",
                type_name: "SunxiA20Ccu",
                module_path: "fstart_driver_sunxi_ccu",
                config_type: "SunxiA20CcuConfig",
                services: &["ClockController"],
                compatible: &["allwinner,sun7i-a20-ccu"],
            },
            #[cfg(feature = "sunxi-a20-dramc")]
            Self::SunxiA20Dramc(_) => &DriverMeta {
                name: "sunxi-a20-dramc",
                type_name: "SunxiA20Dramc",
                module_path: "fstart_driver_sunxi_dramc",
                config_type: "SunxiA20DramcConfig",
                services: &["MemoryController"],
                compatible: &["allwinner,sun7i-a20-dramc"],
            },
            #[cfg(feature = "sunxi-a20-mmc")]
            Self::SunxiA20Mmc(_) => &DriverMeta {
                name: "sunxi-a20-mmc",
                type_name: "SunxiA20Mmc",
                module_path: "fstart_driver_sunxi_mmc",
                config_type: "SunxiA20MmcConfig",
                services: &["BlockDevice"],
                compatible: &["allwinner,sun7i-a20-mmc"],
            },
            #[cfg(feature = "sunxi-a20-spi")]
            Self::SunxiA20Spi(_) => &DriverMeta {
                name: "sunxi-a20-spi",
                type_name: "SunxiA20Spi",
                module_path: "fstart_driver_sunxi_spi",
                config_type: "SunxiA20SpiConfig",
                services: &["BlockDevice"],
                compatible: &["allwinner,sun4i-a10-spi"],
            },
            #[cfg(feature = "sunxi-h3-ccu")]
            Self::SunxiH3Ccu(_) => &DriverMeta {
                name: "sunxi-h3-ccu",
                type_name: "SunxiH3Ccu",
                module_path: "fstart_driver_sunxi_h3_ccu",
                config_type: "SunxiH3CcuConfig",
                services: &["ClockController"],
                compatible: &["allwinner,sun8i-h3-ccu"],
            },
            #[cfg(feature = "sunxi-h3-dramc")]
            Self::SunxiH3Dramc(_) => &DriverMeta {
                name: "sunxi-h3-dramc",
                type_name: "SunxiH3Dramc",
                module_path: "fstart_driver_sunxi_h3_dramc",
                config_type: "SunxiH3DramcConfig",
                services: &["MemoryController"],
                compatible: &["allwinner,sun8i-h3-dramc"],
            },
            #[cfg(feature = "sunxi-h3-mmc")]
            Self::SunxiH3Mmc(_) => &DriverMeta {
                name: "sunxi-h3-mmc",
                type_name: "SunxiH3Mmc",
                module_path: "fstart_driver_sunxi_h3_mmc",
                config_type: "SunxiH3MmcConfig",
                services: &["BlockDevice"],
                compatible: &["allwinner,sun8i-h3-mmc"],
            },
        }
    }

    /// The cargo feature / RON driver name for this variant.
    pub fn driver_name(&self) -> &'static str {
        self.meta().name
    }

    /// Serialize just the inner config struct via the given serializer.
    ///
    /// This enables generic config-to-tokens conversion in `fstart-codegen`
    /// without per-driver match arms there.
    pub fn serialize_config<S: serde::Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match self {
            #[cfg(feature = "ns16550")]
            Self::Ns16550(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "pl011")]
            Self::Pl011(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "designware-i2c")]
            Self::DesignwareI2c(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-a20-ccu")]
            Self::SunxiA20Ccu(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-a20-dramc")]
            Self::SunxiA20Dramc(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-a20-mmc")]
            Self::SunxiA20Mmc(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-a20-spi")]
            Self::SunxiA20Spi(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-h3-ccu")]
            Self::SunxiH3Ccu(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-h3-dramc")]
            Self::SunxiH3Dramc(cfg) => serde::Serialize::serialize(cfg, ser),
            #[cfg(feature = "sunxi-h3-mmc")]
            Self::SunxiH3Mmc(cfg) => serde::Serialize::serialize(cfg, ser),
        }
    }
}
