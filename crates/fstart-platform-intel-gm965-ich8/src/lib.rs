//! GM965/ICH8 platform defaults and recipe traits.
//!
//! Board crates provide board facts. This crate owns chipset defaults, topology,
//! stage layout, and the reusable GM965/ICH8 fixed-flow recipe.

#![no_std]

#[cfg(feature = "recipe")]
extern crate ufmt;

#[cfg(feature = "recipe")]
pub mod recipe;

#[cfg(any(feature = "acpi", feature = "smbios"))]
pub mod tables;

use fstart_driver_intel_gm965 as gm965;
pub use fstart_driver_intel_gm965::{Gm965IgdConfig, IntelGm965Config};
use fstart_driver_intel_ich8 as ich8;
pub use fstart_driver_intel_ich8::{
    HdaConfig, HdaVerbTable, IdeConfig, IntelIch8Config, IoTrapAccess, IoTrapConfig,
    LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode, LpcGenericIoDecode, LpcParallelDecode,
    LpcSerialDecode, PinColor, PinConfig, PinConn, PinConnector, PinDevice, PinGeoLoc, PinLoc,
    SataConfig, SataMode, UsbConfig,
};
use fstart_gpio_ich as gpio;
#[cfg(feature = "recipe")]
pub use fstart_stage::{
    payload::{HaltPayload, MainstagePayload},
    FirmwareBoard, StageKind, StageRecipe,
};
#[cfg(all(feature = "recipe", feature = "crabefi"))]
pub use fstart_stage::payload::X86UefiPayload;
use fstart_types::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_types::{
    hstr, hvec, BootMedium, Capability, CarConfig, Compression, DeviceRole, DeviceTopology,
    FlashLayout, MemoryMap, MemoryRegion, RegionKind, RunsFrom, StageConfig, StageLayout,
    TempRamBuffer,
};
#[cfg(feature = "recipe")]
pub use recipe::{Gm965Ich8Hooks, Gm965Ich8Mainstage, Gm965Ich8Recipe, Gm965Ich8StageBoard};

pub const GM965_NORTHBRIDGE_NODE: &str = "northbridge";
pub const ICH8_SOUTHBRIDGE_NODE: &str = "southbridge";
pub const ICH8_LPC_BUS_NODE: &str = "lpc";
pub const ICH8_SMBUS_NODE: &str = "smbus";
pub const GM965_BOOTBLOCK_LOAD_ADDR: u64 = 0xffff_ffff;
pub const GM965_RAMSTAGE_LOAD_ADDR: u64 = 0x0400_0000;
pub const GM965_RAMSTAGE_HEAP_SIZE: usize = 0x200000;
pub const GM965_NEXT_STAGE_NAME: &str = "ramstage";
pub const ICH8_PMBASE: u32 = 0x0500;

/// Closed GM965/ICH8 chipset policy consumed by fixed stage code.
///
/// Board-attached devices stay in board hooks/code; this only carries fields
/// the GM965/ICH8 drivers program directly.
#[derive(Debug, Clone, Copy)]
pub struct Gm965Ich8Config {
    pub northbridge: gm965::IntelGm965Config,
    pub southbridge: ich8::IntelIch8Config,
}

impl Gm965Ich8Config {
    #[must_use]
    pub const fn new() -> Self {
        let mut southbridge = ich8::IntelIch8Config::new();
        southbridge.pcie_ports = [false; 6];
        Self {
            northbridge: gm965::IntelGm965Config::new(),
            southbridge,
        }
    }

    #[must_use]
    pub const fn igd(mut self, igd: gm965::Gm965IgdConfig) -> Self {
        self.northbridge.igd = igd;
        self
    }

    #[must_use]
    pub const fn pcie_port(mut self, port_index: usize, enabled: bool) -> Self {
        if port_index >= self.southbridge.pcie_ports.len() {
            panic!("ICH8 PCIe port index out of range");
        }
        self.southbridge.pcie_ports[port_index] = enabled;
        self
    }

    #[must_use]
    pub const fn lpc_fixed_io(mut self, fixed_io: LpcFixedIoDecode) -> Self {
        self.southbridge.lpc_decode.fixed_io = fixed_io;
        self
    }

    #[must_use]
    pub const fn lpc_generic_io<const N: usize>(mut self, ranges: [LpcGenericIoDecode; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.southbridge.lpc_decode.generic_io =
                self.southbridge.lpc_decode.generic_io.push(ranges[idx]);
            idx += 1;
        }
        self
    }

    #[must_use]
    pub const fn gpe0_en(mut self, value: u32) -> Self {
        self.southbridge.gpe0_en = value;
        self
    }

    #[must_use]
    pub const fn gpi_routing(mut self, routing: [u8; 16]) -> Self {
        self.southbridge.gpi_routing = routing;
        self
    }

    #[must_use]
    pub const fn ide(mut self, ide: IdeConfig) -> Self {
        self.southbridge.ide = Some(ide);
        self
    }

    #[must_use]
    pub const fn sata(mut self, sata: SataConfig) -> Self {
        self.southbridge.sata = Some(sata);
        self
    }

    #[must_use]
    pub const fn usb(mut self, usb: UsbConfig) -> Self {
        self.southbridge.usb = Some(usb);
        self
    }

    #[must_use]
    pub const fn io_traps<const N: usize>(mut self, traps: [IoTrapConfig; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.southbridge.io_traps = self.southbridge.io_traps.push(traps[idx]);
            idx += 1;
        }
        self
    }

    #[must_use]
    pub const fn gpio_pins<const N: usize>(mut self, pins: [gpio::GpioPin; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            self.southbridge.gpio.pins = self.southbridge.gpio.pins.push(pins[idx]);
            idx += 1;
        }
        self
    }

    #[must_use]
    pub const fn hda_verb<const P: usize, const E: usize>(
        mut self,
        vendor_id: u32,
        subsystem_id: u32,
        pins: [PinConfig; P],
        extra_verbs: [u32; E],
    ) -> Self {
        let mut table = HdaVerbTable::new(vendor_id, subsystem_id);

        let mut idx = 0;
        while idx < P {
            table = table.pin(pins[idx]);
            idx += 1;
        }
        idx = 0;
        while idx < E {
            table = table.extra_verb(extra_verbs[idx]);
            idx += 1;
        }

        let mut hda = match self.southbridge.hda {
            Some(hda) => hda,
            None => HdaConfig::new(),
        };
        hda = hda.verb(table);
        self.southbridge.hda = Some(hda);
        self
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.southbridge.lpc_decode.fixed_io.com_a as u8
            == self.southbridge.lpc_decode.fixed_io.com_b as u8
        {
            panic!("ICH8 COMA and COMB decode the same port");
        }
        if let Some(sata) = self.southbridge.sata {
            if sata.ports == 0 {
                panic!("ICH8 SATA enabled with no ports");
            }
        }

        let generic_io = &self.southbridge.lpc_decode.generic_io;
        let mut idx = 0;
        while idx < generic_io.len() {
            let range = generic_io.get(idx);
            if !ich8::valid_lpc_generic_io(range) {
                panic!("ICH8 LPC generic I/O decode is invalid");
            }

            let mut next = idx + 1;
            while next < generic_io.len() {
                if ich8::lpc_generic_io_overlaps(range, generic_io.get(next)) {
                    panic!("ICH8 LPC generic I/O decodes overlap");
                }
                next += 1;
            }
            idx += 1;
        }

        idx = 0;
        while idx < self.southbridge.io_traps.len() {
            if !ich8::valid_io_trap(self.southbridge.io_traps.get(idx)) {
                panic!("ICH8 I/O trap is invalid");
            }
            idx += 1;
        }

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
