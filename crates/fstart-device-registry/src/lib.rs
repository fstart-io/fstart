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
            #[cfg(feature = "sunxi-h3-ccu")]
            Self::SunxiH3Ccu(_) => &DriverMeta {
                name: "sunxi-h3-ccu",
                type_name: "SunxiH3Ccu",
                module_path: "fstart_driver_sunxi_h3_ccu",
                config_type: "SunxiH3CcuConfig",
                services: &["ClockController"],
                compatible: &["allwinner,sun8i-h3-ccu"],
            },
            #[cfg(feature = "sunxi-a20-dramc")]
            Self::SunxiA20Dramc(_) => &DriverMeta {
                name: "sunxi-a20-dramc",
                type_name: "SunxiA20Dramc",
                module_path: "fstart_driver_sunxi_a20_dramc",
                config_type: "SunxiA20DramcConfig",
                services: &["MemoryController"],
                compatible: &["allwinner,sun7i-a20-dramc"],
            },
            #[cfg(feature = "sunxi-h3-dramc")]
            Self::SunxiH3Dramc(_) => &DriverMeta {
                name: "sunxi-h3-dramc",
                type_name: "SunxiH3Dramc",
                module_path: "fstart_driver_sunxi_h3_dramc",
                config_type: "SunxiH3DramcConfig",
                services: &["MemoryController"],
                compatible: &["allwinner,sun8i-h3-dramc", "allwinner,sun50i-h5-dramc"],
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
            },
            #[cfg(feature = "sunxi-spi")]
            Self::SunxiSpi(_) => &DriverMeta {
                name: "sunxi-spi",
                type_name: "SunxiSpi",
                module_path: "fstart_driver_sunxi_spi",
                config_type: "SunxiSpiConfig",
                services: &["BlockDevice"],
                compatible: &["allwinner,sun4i-a10-spi", "allwinner,sun8i-h3-spi"],
            },
            #[cfg(feature = "sunxi-d1-ccu")]
            Self::SunxiD1Ccu(_) => &DriverMeta {
                name: "sunxi-d1-ccu",
                type_name: "SunxiD1Ccu",
                module_path: "fstart_driver_sunxi_d1_ccu",
                config_type: "SunxiD1CcuConfig",
                services: &["ClockController"],
                compatible: &["allwinner,sun20i-d1-ccu"],
            },
            #[cfg(feature = "sunxi-d1-dramc")]
            Self::SunxiD1Dramc(_) => &DriverMeta {
                name: "sunxi-d1-dramc",
                type_name: "SunxiD1Dramc",
                module_path: "fstart_driver_sunxi_d1_dramc",
                config_type: "SunxiD1DramcConfig",
                services: &["MemoryController"],
                compatible: &["allwinner,sun20i-d1-mbus"],
            },
            #[cfg(feature = "sifive-uart")]
            Self::SifiveUart(_) => &DriverMeta {
                name: "sifive-uart",
                type_name: "SifiveUart",
                module_path: "fstart_driver_sifive_uart",
                config_type: "SifiveUartConfig",
                services: &["Console"],
                compatible: &["sifive,fu740-c000-uart", "sifive,uart0"],
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
        }
    }
}
