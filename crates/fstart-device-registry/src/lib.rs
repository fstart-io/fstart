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

use heapless::{String as HString, Vec as HVec};
use serde::{Deserialize, Serialize};

use fstart_superio::SuperIoChip;

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
    pub use fstart_driver_sunxi_h3_dramc::{SunxiDramcVariant, SunxiH3DramcConfig};
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
    /// Plug-and-Play logical device below a SuperIO chip.
    PnpDevice,
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
#[serde(deny_unknown_fields)]
pub struct DriverBinding {
    /// Device name in the board's flat topology table.
    pub device: HString<32>,
    /// Typed runtime driver configuration for that device.
    pub instance: DriverInstance,
}

/// PnP logical-device descriptor derived from a concrete SuperIO driver config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuperIoLdnDescriptor {
    /// Stable child-node suffix. Platform code prefixes this with the SuperIO node name.
    pub suffix: &'static str,
    /// Chip-specific logical-device number selected through config register `0x07`.
    pub ldn: u8,
    /// Whether this function is configured/enabled on this board.
    pub enabled: bool,
}

/// Board-supplied runtime device to attach to a platform-owned topology point.
///
/// Platform crates own the canonical chipset skeleton, while board crates add
/// devices at named extension points such as an LPC bus, SMBus, or PCIe root
/// port.  This helper keeps the runtime device declaration and its driver
/// binding together so board/platform code cannot forget one side.
#[derive(Debug, Clone)]
pub struct PlatformRuntimeDevice {
    /// Runtime device name in the flattened board topology.
    pub name: HString<32>,
    /// Parent node supplied by the platform topology template.
    pub parent: HString<32>,
    /// Physical attachment below the parent.
    pub bus: fstart_types::BusAddress,
    /// Whether the board declares this device present/enabled.
    pub enabled: bool,
    /// Typed runtime driver configuration for this device.
    pub instance: DriverInstance,
}

/// Reusable collection of board additions for platform topology templates.
#[derive(Debug, Clone, Default)]
pub struct PlatformDeviceExtensions {
    runtime_devices: Vec<PlatformRuntimeDevice>,
}

/// Scoped builder for one platform-owned attachment point.
pub struct PlatformAttachPoint<'a> {
    parent: &'static str,
    extensions: &'a mut PlatformDeviceExtensions,
}

/// Platform topology plus the driver bindings produced while authoring it.
#[derive(Debug, Clone)]
pub struct PlatformTopology {
    topology: fstart_types::DeviceTopology,
    bindings: Vec<DriverBinding>,
}

impl PlatformDeviceExtensions {
    /// Create an empty extension set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Author board devices below a platform-owned parent node.
    pub fn on<F>(&mut self, parent: &'static str, extend: F)
    where
        F: FnOnce(&mut PlatformAttachPoint<'_>),
    {
        let mut point = PlatformAttachPoint {
            parent,
            extensions: self,
        };
        extend(&mut point);
    }

    fn iter(&self) -> impl Iterator<Item = &PlatformRuntimeDevice> {
        self.runtime_devices.iter()
    }
}

impl<'a> PlatformAttachPoint<'a> {
    /// Add a runtime child with an explicit attachment address.
    pub fn runtime(
        &mut self,
        name: &str,
        bus: fstart_types::BusAddress,
        instance: DriverInstance,
    ) -> &mut Self {
        self.runtime_enabled(name, bus, true, instance)
    }

    /// Add a runtime child with an explicit enabled policy.
    pub fn runtime_enabled(
        &mut self,
        name: &str,
        bus: fstart_types::BusAddress,
        enabled: bool,
        instance: DriverInstance,
    ) -> &mut Self {
        self.extensions.runtime_devices.push(PlatformRuntimeDevice {
            name: fstart_types::hstr(name),
            parent: fstart_types::hstr(self.parent),
            bus,
            enabled,
            instance,
        });
        self
    }

    /// Add a PCI child below this attachment point.
    pub fn pci(
        &mut self,
        name: &str,
        device: u8,
        function: u8,
        instance: DriverInstance,
    ) -> &mut Self {
        self.runtime(
            name,
            fstart_types::BusAddress::Pci(device, function),
            instance,
        )
    }

    /// Add an LPC child below this attachment point.
    pub fn lpc(&mut self, name: &str, config_port: u16, instance: DriverInstance) -> &mut Self {
        self.runtime(name, fstart_types::BusAddress::Lpc(config_port), instance)
    }

    /// Add an SMBus/I2C-addressed child below this attachment point.
    pub fn i2c(&mut self, name: &str, address: u8, instance: DriverInstance) -> &mut Self {
        self.runtime(name, fstart_types::BusAddress::I2c(address), instance)
    }

    /// Add an SPI child below this attachment point.
    pub fn spi(&mut self, name: &str, chip_select: u8, instance: DriverInstance) -> &mut Self {
        self.runtime(name, fstart_types::BusAddress::Spi(chip_select), instance)
    }
}

impl PlatformTopology {
    /// Start an empty platform topology template.
    #[must_use]
    pub fn new() -> Self {
        Self {
            topology: fstart_types::DeviceTopology::new(),
            bindings: Vec::new(),
        }
    }

    /// Add a root runtime device and bind its driver in one step.
    #[must_use]
    pub fn root(mut self, name: &str, instance: DriverInstance) -> Self {
        self.topology = self.topology.root(name);
        self.bindings.push(instance.bind(name));
        self
    }

    /// Add a root runtime device with an explicit enabled policy and bind its driver.
    #[must_use]
    pub fn root_enabled(mut self, name: &str, enabled: bool, instance: DriverInstance) -> Self {
        self.topology = self.topology.runtime_root(name, enabled);
        self.bindings.push(instance.bind(name));
        self
    }

    /// Add a driverless child bus owned by a platform device.
    #[must_use]
    pub fn child_bus(mut self, parent: &str, name: &str, role: fstart_types::DeviceRole) -> Self {
        self.topology = self.topology.child_bus(parent, name, role);
        self
    }

    /// Add a driverless PCI/PCIe bridge or root-port node.
    #[must_use]
    pub fn pci_bridge(
        mut self,
        parent: &str,
        name: &str,
        device: u8,
        function: u8,
        enabled: bool,
    ) -> Self {
        self.topology = self
            .topology
            .pci_bridge(parent, name, device, function, enabled);
        self
    }

    /// Add a runtime child and bind its driver in one step.
    #[must_use]
    pub fn runtime(
        mut self,
        parent: &str,
        name: &str,
        bus: fstart_types::BusAddress,
        enabled: bool,
        instance: DriverInstance,
    ) -> Self {
        self.topology = self.topology.runtime_child(parent, name, bus, enabled);
        self.bindings.push(instance.bind(name));
        self
    }

    /// Apply board-supplied runtime devices to this platform topology.
    #[must_use]
    pub fn extend(mut self, extensions: &PlatformDeviceExtensions) -> Self {
        for device in extensions.iter() {
            self = self.runtime(
                device.parent.as_str(),
                device.name.as_str(),
                device.bus,
                device.enabled,
                device.instance.clone(),
            );
        }
        self
    }

    /// Finish as the flattened runtime device table.
    #[must_use]
    pub fn build_devices(self) -> heapless::Vec<fstart_types::DeviceConfig, 32> {
        self.topology.build()
    }

    /// Finish as flattened devices and matching driver bindings.
    #[must_use]
    pub fn build(
        self,
    ) -> (
        heapless::Vec<fstart_types::DeviceConfig, 32>,
        Vec<DriverBinding>,
    ) {
        (self.topology.build(), self.bindings)
    }
}

impl Default for PlatformTopology {
    fn default() -> Self {
        Self::new()
    }
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

    /// Derive the SuperIO PnP logical devices for this driver instance.
    ///
    /// Boards select and configure the concrete SuperIO chip. The chip driver owns
    /// the mapping from function to LDN, so board crates never spell out what an
    /// LDN means.
    pub fn superio_ldns(&self) -> HVec<SuperIoLdnDescriptor, 16> {
        let mut ldns = HVec::new();
        match self {
            #[cfg(feature = "ite8721f")]
            Self::Ite8721f(cfg) => {
                push_superio_ldn(
                    &mut ldns,
                    "com1",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::COM1_LDN,
                    cfg.com1.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "com2",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::COM2_LDN,
                    cfg.com2.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "parallel",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::PARALLEL_LDN,
                    cfg.parallel.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "ec",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::EC_LDN,
                    cfg.env_controller.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "keyboard",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::KBC_LDN,
                    cfg.keyboard.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "mouse",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::MOUSE_LDN,
                    cfg.mouse.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "gpio",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::GPIO_LDN,
                    cfg.gpio.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "cir",
                    <fstart_driver_ite8721f::Ite8721fChip as SuperIoChip>::CIR_LDN,
                    cfg.cir.is_some(),
                );
            }
            #[cfg(feature = "nsc-pc87382")]
            Self::NscPc87382(cfg) => {
                push_superio_ldn(
                    &mut ldns,
                    "com2",
                    <fstart_driver_nsc_pc87382::Pc87382Chip as SuperIoChip>::COM2_LDN,
                    cfg.com2.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "cir",
                    <fstart_driver_nsc_pc87382::Pc87382Chip as SuperIoChip>::CIR_LDN,
                    cfg.cir.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "gpio",
                    <fstart_driver_nsc_pc87382::Pc87382Chip as SuperIoChip>::GPIO_LDN,
                    cfg.gpio.is_some(),
                );
                ldns.push(SuperIoLdnDescriptor {
                    suffix: "dlpc",
                    ldn: fstart_driver_nsc_pc87382::PC87382_DLPC_LDN,
                    enabled: true,
                })
                .expect("SuperIO LDN descriptor capacity");
            }
            #[cfg(feature = "nsc-pc87392")]
            Self::NscPc87392(cfg) => {
                ldns.push(SuperIoLdnDescriptor {
                    suffix: "fdc",
                    ldn: fstart_driver_nsc_pc87392::PC87392_FDC_LDN,
                    enabled: false,
                })
                .expect("SuperIO LDN descriptor capacity");
                push_superio_ldn(
                    &mut ldns,
                    "parallel",
                    <fstart_driver_nsc_pc87392::Pc87392Chip as SuperIoChip>::PARALLEL_LDN,
                    cfg.parallel.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "com2",
                    <fstart_driver_nsc_pc87392::Pc87392Chip as SuperIoChip>::COM2_LDN,
                    cfg.com2.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "com1",
                    <fstart_driver_nsc_pc87392::Pc87392Chip as SuperIoChip>::COM1_LDN,
                    cfg.com1.is_some(),
                );
                push_superio_ldn(
                    &mut ldns,
                    "gpio",
                    <fstart_driver_nsc_pc87392::Pc87392Chip as SuperIoChip>::GPIO_LDN,
                    cfg.gpio.is_some(),
                );
                ldns.push(SuperIoLdnDescriptor {
                    suffix: "wdt",
                    ldn: fstart_driver_nsc_pc87392::PC87392_WDT_LDN,
                    enabled: false,
                })
                .expect("SuperIO LDN descriptor capacity");
            }
            _ => {}
        }
        ldns
    }
}

fn push_superio_ldn(
    ldns: &mut HVec<SuperIoLdnDescriptor, 16>,
    suffix: &'static str,
    ldn: Option<u8>,
    enabled: bool,
) {
    if let Some(ldn) = ldn {
        ldns.push(SuperIoLdnDescriptor {
            suffix,
            ldn,
            enabled,
        })
        .expect("SuperIO LDN descriptor capacity");
    }
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
