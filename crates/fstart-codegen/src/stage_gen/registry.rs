//! Driver registry — resolves driver metadata via [`DriverMeta`].
//!
//! With the typed [`DriverInstance`] enum, most of the old string-based
//! lookup is replaced by `instance.meta()`.  This module keeps the
//! bus-service helpers used by topology validation.

use fstart_device_registry::DriverMeta;
use fstart_types::DeviceConfig;

/// Bus service names that indicate a device is a bus controller.
const BUS_SERVICES: &[&str] = &["I2cBus", "SpiBus", "GpioController"];

/// Returns true if a device provides a bus service.
pub(super) fn is_bus_provider(dev: &DeviceConfig) -> bool {
    dev.services
        .iter()
        .any(|s| BUS_SERVICES.contains(&s.as_str()))
}

/// Look up a [`DriverMeta`] by driver name string.
///
/// Useful when codegen only has the driver name (from `DeviceConfig.driver`)
/// but not the full `DriverInstance`.  Falls back to a const table that
/// mirrors the data on `DriverInstance::meta()`.
pub(super) fn find_driver_meta(name: &str) -> Option<&'static DriverMeta> {
    KNOWN_DRIVER_META.iter().find(|m| m.name == name)
}

/// Static metadata table — kept in sync with [`DriverInstance`] variants.
///
/// Each entry is identical to what `DriverInstance::Xxx(_).meta()` returns.
/// This exists so that code paths which only have a driver-name string
/// (from `DeviceConfig.driver`) can still look up metadata without needing
/// the full `DriverInstance`.
const KNOWN_DRIVER_META: &[DriverMeta] = &[
    DriverMeta {
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
    DriverMeta {
        name: "pl011",
        type_name: "Pl011",
        module_path: "fstart_driver_pl011",
        config_type: "Pl011Config",
        services: &["Console"],
        compatible: &["arm,pl011", "pl011"],
    },
    DriverMeta {
        name: "designware-i2c",
        type_name: "DesignwareI2c",
        module_path: "fstart_driver_designware_i2c",
        config_type: "DesignwareI2cConfig",
        services: &["I2cBus"],
        compatible: &["snps,designware-i2c", "dw-apb-i2c"],
    },
    DriverMeta {
        name: "sunxi-a20-ccu",
        type_name: "SunxiA20Ccu",
        module_path: "fstart_driver_sunxi_ccu",
        config_type: "SunxiA20CcuConfig",
        services: &["ClockController"],
        compatible: &["allwinner,sun7i-a20-ccu"],
    },
    DriverMeta {
        name: "sunxi-a20-dramc",
        type_name: "SunxiA20Dramc",
        module_path: "fstart_driver_sunxi_a20_dramc",
        config_type: "SunxiA20DramcConfig",
        services: &["MemoryController"],
        compatible: &["allwinner,sun7i-a20-dramc"],
    },
    DriverMeta {
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
    DriverMeta {
        name: "sunxi-spi",
        type_name: "SunxiSpi",
        module_path: "fstart_driver_sunxi_spi",
        config_type: "SunxiSpiConfig",
        services: &["BlockDevice"],
        compatible: &["allwinner,sun4i-a10-spi", "allwinner,sun8i-h3-spi"],
    },
    DriverMeta {
        name: "sunxi-h3-ccu",
        type_name: "SunxiH3Ccu",
        module_path: "fstart_driver_sunxi_h3_ccu",
        config_type: "SunxiH3CcuConfig",
        services: &["ClockController"],
        compatible: &["allwinner,sun8i-h3-ccu"],
    },
    DriverMeta {
        name: "sunxi-h3-dramc",
        type_name: "SunxiH3Dramc",
        module_path: "fstart_driver_sunxi_h3_dramc",
        config_type: "SunxiH3DramcConfig",
        services: &["MemoryController"],
        compatible: &["allwinner,sun8i-h3-dramc", "allwinner,sun50i-h5-dramc"],
    },
];
