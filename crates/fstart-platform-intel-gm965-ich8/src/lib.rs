//! GM965/ICH8 platform defaults and recipe traits.
//!
//! Board crates provide board facts. This crate owns chipset defaults, topology,
//! stage layout, and the reusable GM965/ICH8 UEFI-style recipe.

#![no_std]

#[cfg(feature = "recipe")]
extern crate ufmt;

#[cfg(feature = "recipe")]
pub mod recipe;

use fstart_driver_intel_gm965 as gm965;
use fstart_driver_intel_ich8 as ich8;
use fstart_gpio_ich as gpio;
use fstart_types::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_types::{
    board_info_from_config, build_info_from_config, dev_security_config, hstr, hvec,
    x86_uefi_payload, AcpiConfig, AcpiPlatform, BoardConfig, BoardInfo, BootMedium, BuildInfo,
    Capability, CarConfig, Compression, DeviceRole, DeviceTopology, FlashLayout, IntelIfdRegion,
    MemoryMap, MemoryRegion, PayloadConfig, Platform, RegionKind, RunsFrom, SmbiosConfig,
    StageConfig, StageLayout, TempRamBuffer,
};
use heapless::Vec as HVec;

pub use fstart_driver_intel_gm965::Gm965IgdConfig;
pub use fstart_driver_intel_ich8::{
    IdeConfig, IoTrapAccess, IoTrapConfig, LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode,
    LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, SataConfig, SataMode, UsbConfig,
};
#[cfg(feature = "recipe")]
pub use fstart_stage::{FirmwareBoard, StageKind, StageRecipe};
#[cfg(feature = "recipe")]
pub use recipe::{
    Gm965Ich8Mainboard, Gm965Ich8RamstageDevices, Gm965Ich8UefiBoard, Gm965Ich8UefiRecipe,
};

pub const GM965_NORTHBRIDGE_NODE: &str = "northbridge";
pub const ICH8_SOUTHBRIDGE_NODE: &str = "southbridge";
pub const GM965_ICH8_MAINBOARD_NODE: &str = "mainboard";
pub const ICH8_LPC_BUS_NODE: &str = "lpc";
pub const ICH8_SMBUS_NODE: &str = "smbus";
pub const GM965_DEFAULT_FIRMWARE_BASE: u64 = 0xFFE8_0000;
pub const GM965_DEFAULT_FIRMWARE_SIZE: usize = 0x0018_0000;
pub const GM965_BOOTBLOCK_LOAD_ADDR: u64 = 0xffff_ffff;
pub const GM965_RAMSTAGE_LOAD_ADDR: u64 = 0x0400_0000;
pub const GM965_RAMSTAGE_HEAP_SIZE: usize = 0x200000;
pub const GM965_NEXT_STAGE_NAME: &str = "ramstage";
pub const ICH8_PMBASE: u32 = 0x0500;

/// Closed GM965/ICH8 chipset policy consumed by fixed stage code.
///
/// Board-attached devices stay in board hooks/code; this only carries fields
/// the GM965/ICH8 drivers program directly.
#[derive(Debug, Clone)]
pub struct Gm965Ich8Config {
    pub northbridge: gm965::IntelGm965Config,
    pub southbridge: ich8::IntelIch8Config,
    pub firmware_base: u64,
    pub firmware_size: usize,
}

impl Gm965Ich8Config {
    #[must_use]
    pub fn new() -> Self {
        Self {
            northbridge: gm965_defaults(),
            southbridge: ich8_defaults(),
            firmware_base: GM965_DEFAULT_FIRMWARE_BASE,
            firmware_size: GM965_DEFAULT_FIRMWARE_SIZE,
        }
    }

    #[must_use]
    pub fn with_flash_layout(mut self, flash_layout: Option<&FlashLayout>) -> Self {
        let (base, size) = firmware_window(flash_layout);
        self.firmware_base = base;
        self.firmware_size = size;
        self
    }
}

impl Default for Gm965Ich8Config {
    fn default() -> Self {
        Self::new()
    }
}

/// Platform-owned ACPI namespace context for GM965/ICH8 recipes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gm965Ich8AcpiContext;

impl Gm965Ich8AcpiContext {
    #[must_use]
    pub const fn sb_scope(self) -> &'static str {
        "\\_SB_"
    }

    #[must_use]
    pub const fn lpc_scope(self) -> &'static str {
        "\\_SB_.PCI0.LPCB"
    }

    #[must_use]
    pub const fn gpe_scope(self) -> &'static str {
        "\\_GPE"
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcieRootPort {
    Port1,
    Port2,
    Port3,
    Port4,
    Port5,
    Port6,
}

impl PcieRootPort {
    const fn index(self) -> usize {
        match self {
            Self::Port1 => 0,
            Self::Port2 => 1,
            Self::Port3 => 2,
            Self::Port4 => 3,
            Self::Port5 => 4,
            Self::Port6 => 5,
        }
    }
}

/// GM965 northbridge config plus its topology node name.
#[derive(Debug, Clone)]
pub struct Gm965Northbridge {
    pub name: &'static str,
    pub config: gm965::IntelGm965Config,
}

/// ICH8 PCIe root port state owned by the closed chipset config.
#[derive(Debug, Clone)]
pub struct PcieRootPortNode {
    pub name: &'static str,
    pub device: u8,
    pub function: u8,
    pub enabled: bool,
}

impl PcieRootPortNode {
    #[must_use]
    pub const fn new(name: &'static str, device: u8, function: u8) -> Self {
        Self {
            name,
            device,
            function,
            enabled: false,
        }
    }
}

/// ICH8 southbridge config plus the topology owned by that config.
#[derive(Debug, Clone)]
pub struct Ich8Southbridge {
    pub name: &'static str,
    pub config: ich8::IntelIch8Config,
    pub pcie: [PcieRootPortNode; 6],
}

impl Ich8Southbridge {
    #[must_use]
    pub fn pcie_mut(&mut self, port: PcieRootPort) -> &mut PcieRootPortNode {
        &mut self.pcie[port.index()]
    }

    #[must_use]
    pub fn driver_config(&self) -> ich8::IntelIch8Config {
        let mut config = self.config.clone();
        for (idx, port) in self.pcie.iter().enumerate() {
            config.pcie_ports[idx] = port.enabled;
        }
        config
    }
}

/// GM965/ICH8 board definition used by metadata and runtime recipes.
///
/// This is the platform-family BSP data structure: it keeps chipset driver
/// configs and the topology nodes they imply in one place. Board crates start
/// from [`Gm965Ich8Board::new`] defaults and mutate nested fields directly.
#[derive(Debug, Clone)]
pub struct Gm965Ich8Board {
    pub board_name: &'static str,
    pub board_package: &'static str,
    pub payload: PayloadConfig,
    pub flash_layout: Option<FlashLayout>,
    pub smbios: Option<SmbiosConfig>,
    pub mainboard_enabled: bool,
    pub acpi_print_hex: bool,
    pub northbridge: Gm965Northbridge,
    pub southbridge: Ich8Southbridge,
}

impl Gm965Ich8Board {
    #[must_use]
    pub fn new(board_name: &'static str, board_package: &'static str) -> Self {
        Self {
            board_name,
            board_package,
            payload: x86_uefi_payload(),
            flash_layout: None,
            smbios: None,
            mainboard_enabled: false,
            acpi_print_hex: false,
            northbridge: Gm965Northbridge {
                name: GM965_NORTHBRIDGE_NODE,
                config: gm965_defaults(),
            },
            southbridge: Ich8Southbridge {
                name: ICH8_SOUTHBRIDGE_NODE,
                config: ich8_defaults(),
                pcie: pcie_root_ports(),
            },
        }
    }

    #[must_use]
    pub fn board_config(&self) -> BoardConfig {
        BoardConfig {
            name: hstr(self.board_name),
            platform: Platform::X86_64,
            memory: gm965_ich8_memory(self.flash_layout.clone()),
            devices: self.topology().build(),
            stages: gm965_ich8_stages(),
            security: dev_security_config("keys/dev-signing.pub"),
            payload: Some(self.payload.clone()),
            microcode: Some(gm965_microcode()),
            soc_image_format: Default::default(),
            full_flash_image: true,
            acpi: Some(AcpiConfig {
                print_hex: self.acpi_print_hex,
                platform: AcpiPlatform::X86,
            }),
            smbios: self.smbios.clone(),
            smm: None,
            build: fstart_types::BoardBuildPolicy {
                cpu_feature: Some(hstr("cpu-intel-core2")),
                ..Default::default()
            },
            boot_hart_id: 0,
        }
    }

    #[must_use]
    pub fn board_info(&self) -> BoardInfo {
        board_info_from_config(self.board_config())
    }

    #[must_use]
    pub fn build_info<I>(&self, driver_features: I) -> BuildInfo
    where
        I: IntoIterator<Item = &'static str>,
    {
        let config = self.board_config();
        build_info_from_config(
            self.board_name,
            self.board_package,
            &config,
            driver_features,
        )
    }

    #[must_use]
    pub fn firmware_base(&self) -> u64 {
        firmware_window(self.flash_layout.as_ref()).0
    }

    #[must_use]
    pub fn firmware_size(&self) -> usize {
        firmware_window(self.flash_layout.as_ref()).1
    }

    fn topology(&self) -> DeviceTopology {
        let mut topology = DeviceTopology::new()
            .root(self.northbridge.name)
            .root(self.southbridge.name);

        for port in &self.southbridge.pcie {
            topology = topology.pci_bridge(
                self.southbridge.name,
                port.name,
                port.device,
                port.function,
                port.enabled,
            );
        }

        topology
            .child_bus(self.southbridge.name, ICH8_LPC_BUS_NODE, DeviceRole::LpcBus)
            .child_bus(self.southbridge.name, ICH8_SMBUS_NODE, DeviceRole::SmBus)
            .runtime_root(GM965_ICH8_MAINBOARD_NODE, self.mainboard_enabled)
    }
}

impl Default for Gm965Ich8Board {
    fn default() -> Self {
        Self::new("gm965-ich8", "fstart-platform-intel-gm965-ich8")
    }
}

fn pcie_root_ports() -> [PcieRootPortNode; 6] {
    [
        PcieRootPortNode::new("pcie1", 0x1c, 0),
        PcieRootPortNode::new("pcie2", 0x1c, 1),
        PcieRootPortNode::new("pcie3", 0x1c, 2),
        PcieRootPortNode::new("pcie4", 0x1c, 3),
        PcieRootPortNode::new("pcie5", 0x1c, 4),
        PcieRootPortNode::new("pcie6", 0x1c, 5),
    ]
}

pub fn gm965_ich8_memory(flash_layout: Option<FlashLayout>) -> MemoryMap {
    MemoryMap {
        regions: hvec([MemoryRegion {
            name: hstr("workram"),
            base: 0x0010_0000,
            size: 0x3FF0_0000,
            kind: RegionKind::Ram,
        }]),
        flash_layout,
        car: Some(CarConfig {
            base: 0xFEF0_0000,
            size: 0x80000,
        }),
    }
}

pub fn gm965_ich8_stages() -> StageLayout {
    StageLayout::MultiStage(hvec([
        StageConfig {
            name: hstr("bootblock"),
            capabilities: hvec([
                Capability::ConsoleInit,
                Capability::DramInit,
                Capability::BootMedia(BootMedium::FirmwareImage {
                    temp_ram_buffer: None,
                }),
                Capability::StageLoad {
                    next_stage: hstr(GM965_NEXT_STAGE_NAME),
                },
            ]),
            load_addr: GM965_BOOTBLOCK_LOAD_ADDR,
            stack_size: 0x2000,
            // Small CAR heap for FFS/LZ4 scratch allocations.
            heap_size: Some(0x100),
            runs_from: RunsFrom::Rom,
            compression: Compression::None,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
        StageConfig {
            name: hstr(GM965_NEXT_STAGE_NAME),
            capabilities: hvec([
                Capability::ConsoleInit,
                Capability::BootMedia(BootMedium::FirmwareImage {
                    temp_ram_buffer: Some(TempRamBuffer {
                        base: 0x0200_0000,
                        size: 0x0100_0000,
                    }),
                }),
                Capability::SigVerify,
                Capability::DriverInit,
                Capability::MemoryDetect,
                Capability::PciInit,
                Capability::MpInit {
                    max_cpus: 2,
                    smm: false,
                },
                Capability::AcpiPrepare,
                Capability::SmBiosPrepare,
                Capability::PayloadLoad,
            ]),
            load_addr: GM965_RAMSTAGE_LOAD_ADDR,
            stack_size: 0x400000,
            heap_size: Some(GM965_RAMSTAGE_HEAP_SIZE as u32),
            runs_from: RunsFrom::Ram,
            compression: Compression::Lz4,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
    ]))
}

pub fn gm965_defaults() -> gm965::IntelGm965Config {
    gm965::IntelGm965Config {
        mchbar: 0xFED1_4000,
        dmibar: 0xFED1_8000,
        epbar: 0xFED1_9000,
        ecam_base: 0xE000_0000,
        ecam_buses: 64,
        enable_peg: false,
        igd: Gm965IgdConfig::default(),
        smbus_base: 0x0400,
        spd_addresses: [0x50, 0, 0x51, 0],
        acpi_name: Some(hstr("PCI0")),
    }
}

pub fn ich8_defaults() -> ich8::IntelIch8Config {
    ich8::IntelIch8Config {
        rcba: 0xFED1_C000,
        dmibar: 0xFED1_8000,
        pirq_routing: [0x0b; 8],
        gpe0_en: 0,
        gpi_routing: [0; 16],
        alt_gp_smi_en: 0,
        c4_on_c3: true,
        c5_enable: false,
        c6_enable: false,
        lpc_decode: LpcDecodeConfig::default(),
        hda: None,
        ide: None,
        sata: None,
        usb: None,
        pcie_ports: [false; 6],
        pcie_slots: [false; 6],
        pcie_power_limits: [ich8::PciePowerLimit::default(); 6],
        io_traps: HVec::new(),
        smbus_base: 0x0400,
        gpio: gpio::GpioConfig::default(),
        acpi_name: Some(hstr("LPCB")),
        c3_latency: 85,
        power_on_after_fail: 0,
        throttle_duty: 0,
        disable_lan: false,
        disable_sata2: true,
        disable_thermal: true,
    }
}

fn gm965_microcode() -> MicrocodeConfig {
    MicrocodeConfig::Intel(IntelMicrocodeConfig {
        files: hvec([
            hstr("../../intel-microcode/intel-ucode/06-0f-02"),
            hstr("../../intel-microcode/intel-ucode/06-0f-06"),
            hstr("../../intel-microcode/intel-ucode/06-0f-07"),
            hstr("../../intel-microcode/intel-ucode/06-0f-0a"),
            hstr("../../intel-microcode/intel-ucode/06-0f-0b"),
            hstr("../../intel-microcode/intel-ucode/06-0f-0d"),
            hstr("../../intel-microcode/intel-ucode/06-16-01"),
        ]),
        early: true,
        mp: true,
    })
}

fn firmware_window(flash_layout: Option<&FlashLayout>) -> (u64, usize) {
    let Some(FlashLayout::IntelIfd(layout)) = flash_layout else {
        return (GM965_DEFAULT_FIRMWARE_BASE, GM965_DEFAULT_FIRMWARE_SIZE);
    };

    layout
        .regions
        .iter()
        .find(|region| region.kind == IntelIfdRegion::Bios)
        .map(|region| {
            (
                layout.base.saturating_add(u64::from(region.offset)),
                region.size as usize,
            )
        })
        .unwrap_or((GM965_DEFAULT_FIRMWARE_BASE, GM965_DEFAULT_FIRMWARE_SIZE))
}
