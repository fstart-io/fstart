//! GM965/ICH8 platform defaults and recipe traits.
//!
//! Board crates provide board facts. This crate owns chipset defaults, topology,
//! stage layout, and the reusable GM965/ICH8 fixed-flow recipe.

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
    hstr, hvec, BootMedium, Capability, CarConfig, Compression, DeviceRole, DeviceTopology,
    FlashLayout, IntelIfdRegion, MemoryMap, MemoryRegion, RegionKind, RunsFrom, StageConfig,
    StageLayout, TempRamBuffer,
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
    Gm965Ich8Mainboard, Gm965Ich8RamstageDevices, Gm965Ich8Recipe, Gm965Ich8StageBoard,
};

pub const GM965_NORTHBRIDGE_NODE: &str = "northbridge";
pub const ICH8_SOUTHBRIDGE_NODE: &str = "southbridge";
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

pub fn gm965_ich8_topology(config: &Gm965Ich8Config) -> DeviceTopology {
    let mut topology = DeviceTopology::new()
        .root(GM965_NORTHBRIDGE_NODE)
        .root(ICH8_SOUTHBRIDGE_NODE);

    for (idx, enabled) in config.southbridge.pcie_ports.iter().copied().enumerate() {
        topology = topology.pci_bridge(
            ICH8_SOUTHBRIDGE_NODE,
            PCIE_ROOT_PORTS[idx],
            0x1c,
            idx as u8,
            enabled,
        );
    }

    topology
        .child_bus(ICH8_SOUTHBRIDGE_NODE, ICH8_LPC_BUS_NODE, DeviceRole::LpcBus)
        .child_bus(ICH8_SOUTHBRIDGE_NODE, ICH8_SMBUS_NODE, DeviceRole::SmBus)
}

const PCIE_ROOT_PORTS: [&str; 6] = ["pcie1", "pcie2", "pcie3", "pcie4", "pcie5", "pcie6"];

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

pub fn gm965_ich8_microcode() -> MicrocodeConfig {
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
