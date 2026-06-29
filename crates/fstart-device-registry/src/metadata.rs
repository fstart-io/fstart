use crate::*;
use fstart_services::ServiceSet;
use std::{boxed::Box, vec::Vec};

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

impl fstart_board_meta::BoardDriver for DriverInstance {
    fn feature(&self) -> &'static str {
        self.driver_feature()
            .expect("structural device nodes are not runtime board drivers")
    }

    fn services(&self) -> ServiceSet {
        self.provided_services()
    }

    fn clone_box(&self) -> Box<dyn fstart_board_meta::BoardDriver> {
        Box::new(self.clone())
    }
}
