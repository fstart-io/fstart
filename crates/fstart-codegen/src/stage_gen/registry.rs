//! Driver registry — resolves driver metadata via [`DriverMeta`].
//!
//! With the typed [`DriverInstance`] enum, most of the old string-based
//! lookup is replaced by `instance.meta()`.  This module keeps the
//! bus-service helpers used by topology validation.

use fstart_device_registry::DriverMeta;
use fstart_types::DeviceConfig;

/// Bus service names that indicate a device is a bus controller.
///
/// A parent device must provide at least one of these for a **bus-device**
/// child (`is_bus_device == true`) to be accepted by
/// [`super::topology::validate_device_tree`]. Plain-device children
/// (e.g., an NS16550 UART nested under a SuperIO for init ordering)
/// don't require the parent to provide any of these.
///
/// The list covers:
/// - Real bus controllers (`I2cBus`, `SpiBus`, `PciRootBus`).
/// - GPIO controllers that expose pins as children.
/// - x86 chipset sub-hierarchies: the northbridge (`PciHost`),
///   southbridge (`Southbridge`), PCIe root ports (`PciBridge`),
///   LPC bus (`LpcBus`), SMBus (`SmBus`), and SuperIO hosts
///   (`SuperIoHost`).
const BUS_SERVICES: &[&str] = &[
    "I2cBus",
    "SpiBus",
    "GpioController",
    "PciRootBus",
    "PciHost",
    "Southbridge",
    "PciBridge",
    "LpcBus",
    "SmBus",
    "SuperIoHost",
];

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
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "pl011",
        type_name: "Pl011",
        module_path: "fstart_driver_pl011",
        config_type: "Pl011Config",
        services: &["Console"],
        compatible: &["arm,pl011", "pl011"],
        has_acpi: true,
        is_bus_device: false,
    },
    DriverMeta {
        name: "designware-i2c",
        type_name: "DesignwareI2c",
        module_path: "fstart_driver_designware_i2c",
        config_type: "DesignwareI2cConfig",
        services: &["I2cBus"],
        compatible: &["snps,designware-i2c", "dw-apb-i2c"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "sunxi-a20-ccu",
        type_name: "SunxiA20Ccu",
        module_path: "fstart_driver_sunxi_ccu",
        config_type: "SunxiA20CcuConfig",
        services: &["ClockController"],
        compatible: &["allwinner,sun7i-a20-ccu"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "sunxi-a20-dramc",
        type_name: "SunxiA20Dramc",
        module_path: "fstart_driver_sunxi_a20_dramc",
        config_type: "SunxiA20DramcConfig",
        services: &["MemoryController"],
        compatible: &["allwinner,sun7i-a20-dramc"],
        has_acpi: false,
        is_bus_device: false,
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
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "sunxi-spi",
        type_name: "SunxiSpi",
        module_path: "fstart_driver_sunxi_spi",
        config_type: "SunxiSpiConfig",
        services: &["BlockDevice"],
        compatible: &["allwinner,sun4i-a10-spi", "allwinner,sun8i-h3-spi"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "sunxi-h3-ccu",
        type_name: "SunxiH3Ccu",
        module_path: "fstart_driver_sunxi_h3_ccu",
        config_type: "SunxiH3CcuConfig",
        services: &["ClockController"],
        compatible: &["allwinner,sun8i-h3-ccu"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "sunxi-h3-dramc",
        type_name: "SunxiH3Dramc",
        module_path: "fstart_driver_sunxi_h3_dramc",
        config_type: "SunxiH3DramcConfig",
        services: &["MemoryController"],
        compatible: &["allwinner,sun8i-h3-dramc", "allwinner,sun50i-h5-dramc"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "sifive-uart",
        type_name: "SifiveUart",
        module_path: "fstart_driver_sifive_uart",
        config_type: "SifiveUartConfig",
        services: &["Console"],
        compatible: &["sifive,fu740-c000-uart", "sifive,uart0"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "fu740-prci",
        type_name: "Fu740Prci",
        module_path: "fstart_driver_fu740_prci",
        config_type: "Fu740PrciConfig",
        services: &["ClockController"],
        compatible: &["sifive,fu740-c000-prci"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "fu740-ddr",
        type_name: "Fu740Ddr",
        module_path: "fstart_driver_fu740_ddr",
        config_type: "Fu740DdrConfig",
        services: &["MemoryController"],
        compatible: &["sifive,fu740-c000-ddr"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "pci-ecam",
        type_name: "PciEcam",
        module_path: "fstart_driver_pci_ecam",
        config_type: "PciEcamConfig",
        services: &["PciRootBus"],
        compatible: &["pci-host-ecam-generic"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "bochs-display",
        type_name: "BochsDisplay",
        module_path: "fstart_driver_bochs_display",
        config_type: "BochsDisplayConfig",
        services: &["Framebuffer"],
        compatible: &["bochs-display", "qemu-stdvga"],
        has_acpi: false,
        is_bus_device: true,
    },
    DriverMeta {
        name: "q35-hostbridge",
        type_name: "Q35HostBridge",
        module_path: "fstart_driver_q35_hostbridge",
        config_type: "Q35HostBridgeConfig",
        services: &["PciRootBus"],
        compatible: &["q35-hostbridge"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "ite8721f",
        type_name: "Ite8721f",
        module_path: "fstart_driver_ite8721f",
        config_type: "Ite8721fConfig",
        services: &["SuperIoHost"],
        compatible: &["ite,it8721f", "ite,8721f"],
        has_acpi: false,
        is_bus_device: true,
    },
    DriverMeta {
        name: "intel-pineview",
        type_name: "IntelPineview",
        module_path: "fstart_driver_intel_pineview",
        config_type: "IntelPineviewConfig",
        services: &["MemoryController", "PciHost"],
        compatible: &["intel,pineview-mch", "intel,atom-d4xx-mch"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "intel-ich7",
        type_name: "IntelIch7",
        module_path: "fstart_driver_intel_ich7",
        config_type: "IntelIch7Config",
        services: &["Southbridge"],
        compatible: &["intel,ich7", "intel,nm10"],
        has_acpi: false,
        is_bus_device: false,
    },
    DriverMeta {
        name: "i2c-ck505",
        type_name: "I2cCk505",
        module_path: "fstart_driver_i2c_ck505",
        config_type: "I2cCk505Config",
        services: &[],
        compatible: &["idt,ck505"],
        has_acpi: false,
        is_bus_device: true,
    },
    DriverMeta {
        name: "_structural",
        type_name: "_Structural",
        module_path: "fstart_device_registry",
        config_type: "StructuralConfig",
        services: &[],
        compatible: &[],
        has_acpi: false,
        is_bus_device: false,
    },
];
