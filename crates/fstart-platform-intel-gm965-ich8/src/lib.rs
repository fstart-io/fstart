//! GM965/ICH8 platform defaults and recipe traits.
//!
//! Board crates provide board facts. This crate owns chipset defaults, topology,
//! stage layout, and the reusable GM965/ICH8 fixed-flow recipe.

#![no_std]

#[cfg(feature = "recipe")]
extern crate ufmt;

#[cfg(feature = "recipe")]
pub mod recipe;

#[cfg(feature = "recipe")]
use fstart_driver_intel_gm965 as gm965;
use fstart_driver_intel_ich8 as ich8;
use fstart_gpio_ich as gpio;
use fstart_types::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_types::{
    hstr, hvec, BootMedium, Capability, CarConfig, Compression, DeviceRole, DeviceTopology,
    FlashLayout, MemoryMap, MemoryRegion, RegionKind, RunsFrom, StageConfig, StageLayout,
    TempRamBuffer,
};
#[cfg(feature = "recipe")]
use heapless::Vec as HVec;

pub use fstart_driver_intel_ich8::{
    HdaConfig, HdaVerbTable, IdeConfig, IoTrapAccess, IoTrapConfig, LpcFixedIoDecode,
    LpcFloppyDecode, LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, PinColor, PinConfig,
    PinConn, PinConnector, PinDevice, PinGeoLoc, PinLoc, SataConfig, SataMode, UsbConfig,
};
#[cfg(feature = "recipe")]
pub use fstart_stage::{FirmwareBoard, StageKind, StageRecipe};
#[cfg(feature = "recipe")]
pub use recipe::{Gm965Ich8Hooks, Gm965Ich8RamstageDevices, Gm965Ich8Recipe, Gm965Ich8StageBoard};

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

const MAX_LPC_GENERIC_IO: usize = 4;
const MAX_IO_TRAPS: usize = 4;
const MAX_HDA_VERBS: usize = 4;
const MAX_HDA_PINS: usize = 16;
const MAX_HDA_EXTRA_VERBS: usize = 32;
const MAX_GPIO_PINS: usize = 76;

const EMPTY_LPC_GENERIC_IO: LpcGenericIoDecode = LpcGenericIoDecode { base: 0, size: 0 };
const EMPTY_IO_TRAP: IoTrapConfig = IoTrapConfig {
    index: 0,
    base: 0,
    size: 4,
    access: IoTrapAccess::Any,
};
const EMPTY_HDA_PIN: PinConfig = PinConfig {
    nid: 0,
    nc: Some(0),
    conn: PinConn::Nc,
    loc: PinLoc::External,
    geo: PinGeoLoc::NA,
    device: PinDevice::LineOut,
    connector: PinConnector::Unknown,
    color: PinColor::ColorUnknown,
    misc: 0,
    group: 0,
    seq: 0,
};
const EMPTY_HDA_VERB_TABLE: Gm965Ich8HdaVerbTable = Gm965Ich8HdaVerbTable {
    vendor_id: 0,
    subsystem_id: 0,
    pins: [EMPTY_HDA_PIN; MAX_HDA_PINS],
    pin_count: 0,
    extra_verbs: [0; MAX_HDA_EXTRA_VERBS],
    extra_verb_count: 0,
};

/// Integrated graphics configuration kept as const-friendly platform data.
#[derive(Debug, Clone, Copy)]
pub struct Gm965IgdConfig {
    pub enable_vga: bool,
    pub enable_pipe_b: bool,
    pub gtt_mmio_base: u64,
    pub stolen_memory_mb: u16,
    pub vbt_file: Option<&'static str>,
    pub vbt_addr: Option<u64>,
    pub vbt_size: u32,
    pub legacy_vbt_probe: Option<u64>,
    pub panel_power_up_delay: u16,
    pub panel_power_down_delay: u16,
    pub panel_backlight_on_delay: u16,
    pub panel_backlight_off_delay: u16,
    pub panel_power_cycle_delay: u8,
    pub default_pwm_freq: u16,
    pub duty_cycle: u8,
}

impl Gm965IgdConfig {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            enable_vga: true,
            enable_pipe_b: true,
            gtt_mmio_base: 0xfeb0_0000,
            stolen_memory_mb: 32,
            vbt_file: None,
            vbt_addr: None,
            vbt_size: 0,
            legacy_vbt_probe: Some(0x000c_0000),
            panel_power_up_delay: 2000,
            panel_power_down_delay: 2000,
            panel_backlight_on_delay: 2000,
            panel_backlight_off_delay: 2000,
            panel_power_cycle_delay: 6,
            default_pwm_freq: 0,
            duty_cycle: 100,
        }
    }
}

impl Default for Gm965IgdConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// GM965 northbridge policy consumed by the GM965/ICH8 platform flow.
#[derive(Debug, Clone, Copy)]
pub struct Gm965NorthbridgeConfig {
    pub mchbar: u64,
    pub dmibar: u64,
    pub epbar: u64,
    pub ecam_base: u64,
    pub ecam_buses: u16,
    pub enable_peg: bool,
    pub igd: Gm965IgdConfig,
    pub smbus_base: u16,
    pub spd_addresses: [u8; 4],
    pub acpi_name: Option<&'static str>,
}

/// One HDA codec verb table stored without heapless vectors.
#[derive(Debug, Clone, Copy)]
pub struct Gm965Ich8HdaVerbTable {
    pub vendor_id: u32,
    pub subsystem_id: u32,
    pins: [PinConfig; MAX_HDA_PINS],
    pin_count: usize,
    extra_verbs: [u32; MAX_HDA_EXTRA_VERBS],
    extra_verb_count: usize,
}

/// ICH8 southbridge policy consumed by the GM965/ICH8 platform flow.
#[derive(Debug, Clone, Copy)]
pub struct Ich8SouthbridgeConfig {
    pub rcba: u64,
    pub dmibar: u64,
    pub pirq_routing: [u8; 8],
    pub gpe0_en: u32,
    pub gpi_routing: [u8; 16],
    pub alt_gp_smi_en: u16,
    pub c4_on_c3: bool,
    pub c5_enable: bool,
    pub c6_enable: bool,
    pub lpc_fixed_io: LpcFixedIoDecode,
    lpc_generic_io: [LpcGenericIoDecode; MAX_LPC_GENERIC_IO],
    lpc_generic_io_count: usize,
    pub ide: Option<IdeConfig>,
    pub sata: Option<SataConfig>,
    pub usb: Option<UsbConfig>,
    pub pcie_ports: [bool; 6],
    pub pcie_slots: [bool; 6],
    pub pcie_power_limits: [ich8::PciePowerLimit; 6],
    io_traps: [IoTrapConfig; MAX_IO_TRAPS],
    io_trap_count: usize,
    pub smbus_base: u16,
    gpio_pins: [gpio::GpioPin; MAX_GPIO_PINS],
    gpio_pin_count: usize,
    hda_verbs: [Gm965Ich8HdaVerbTable; MAX_HDA_VERBS],
    hda_verb_count: usize,
    pub acpi_name: Option<&'static str>,
    pub c3_latency: u16,
    pub power_on_after_fail: u8,
    pub throttle_duty: u8,
    pub disable_lan: bool,
    pub disable_sata2: bool,
    pub disable_thermal: bool,
}

/// Closed GM965/ICH8 chipset policy consumed by fixed stage code.
///
/// Board-attached devices stay in board hooks/code; this only carries fields
/// the GM965/ICH8 drivers program directly.
#[derive(Debug, Clone, Copy)]
pub struct Gm965Ich8Config {
    pub northbridge: Gm965NorthbridgeConfig,
    pub southbridge: Ich8SouthbridgeConfig,
    pub firmware_base: u64,
    pub firmware_size: usize,
}

impl Gm965Ich8Config {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            northbridge: Gm965NorthbridgeConfig {
                mchbar: 0xFED1_4000,
                dmibar: 0xFED1_8000,
                epbar: 0xFED1_9000,
                ecam_base: 0xE000_0000,
                ecam_buses: 64,
                enable_peg: false,
                igd: Gm965IgdConfig::new(),
                smbus_base: 0x0400,
                spd_addresses: [0x50, 0, 0x51, 0],
                acpi_name: Some("PCI0"),
            },
            southbridge: Ich8SouthbridgeConfig {
                rcba: 0xFED1_C000,
                dmibar: 0xFED1_8000,
                pirq_routing: [0x0b; 8],
                gpe0_en: 0,
                gpi_routing: [0; 16],
                alt_gp_smi_en: 0,
                c4_on_c3: true,
                c5_enable: false,
                c6_enable: false,
                lpc_fixed_io: LpcFixedIoDecode {
                    com_a: LpcSerialDecode::Com1,
                    com_b: LpcSerialDecode::Com2,
                    lpt: None,
                    fdd: None,
                },
                lpc_generic_io: [EMPTY_LPC_GENERIC_IO; MAX_LPC_GENERIC_IO],
                lpc_generic_io_count: 0,
                ide: None,
                sata: None,
                usb: None,
                pcie_ports: [false; 6],
                pcie_slots: [false; 6],
                pcie_power_limits: [ich8::PciePowerLimit { value: 0, scale: 0 }; 6],
                io_traps: [EMPTY_IO_TRAP; MAX_IO_TRAPS],
                io_trap_count: 0,
                smbus_base: 0x0400,
                gpio_pins: [gpio::input(0); MAX_GPIO_PINS],
                gpio_pin_count: 0,
                hda_verbs: [EMPTY_HDA_VERB_TABLE; MAX_HDA_VERBS],
                hda_verb_count: 0,
                acpi_name: Some("LPCB"),
                c3_latency: 85,
                power_on_after_fail: 0,
                throttle_duty: 0,
                disable_lan: false,
                disable_sata2: true,
                disable_thermal: true,
            },
            firmware_base: GM965_DEFAULT_FIRMWARE_BASE,
            firmware_size: GM965_DEFAULT_FIRMWARE_SIZE,
        }
    }

    #[must_use]
    pub const fn firmware_window(mut self, base: u64, size: usize) -> Self {
        self.firmware_base = base;
        self.firmware_size = size;
        self
    }

    #[must_use]
    pub const fn igd(mut self, igd: Gm965IgdConfig) -> Self {
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
        self.southbridge.lpc_fixed_io = fixed_io;
        self
    }

    #[must_use]
    pub const fn lpc_generic_io<const N: usize>(mut self, ranges: [LpcGenericIoDecode; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            if self.southbridge.lpc_generic_io_count >= MAX_LPC_GENERIC_IO {
                panic!("too many ICH8 LPC generic I/O windows");
            }
            self.southbridge.lpc_generic_io[self.southbridge.lpc_generic_io_count] = ranges[idx];
            self.southbridge.lpc_generic_io_count += 1;
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
            if self.southbridge.io_trap_count >= MAX_IO_TRAPS {
                panic!("too many ICH8 I/O traps");
            }
            self.southbridge.io_traps[self.southbridge.io_trap_count] = traps[idx];
            self.southbridge.io_trap_count += 1;
            idx += 1;
        }
        self
    }

    #[must_use]
    pub const fn gpio_pins<const N: usize>(mut self, pins: [gpio::GpioPin; N]) -> Self {
        let mut idx = 0;
        while idx < N {
            if self.southbridge.gpio_pin_count >= MAX_GPIO_PINS {
                panic!("too many ICH8 GPIO pins");
            }
            self.southbridge.gpio_pins[self.southbridge.gpio_pin_count] = pins[idx];
            self.southbridge.gpio_pin_count += 1;
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
        if self.southbridge.hda_verb_count >= MAX_HDA_VERBS {
            panic!("too many ICH8 HDA verb tables");
        }
        if P > MAX_HDA_PINS || E > MAX_HDA_EXTRA_VERBS {
            panic!("ICH8 HDA verb table exceeds capacity");
        }

        let mut table = EMPTY_HDA_VERB_TABLE;
        table.vendor_id = vendor_id;
        table.subsystem_id = subsystem_id;
        table.pin_count = P;
        table.extra_verb_count = E;

        let mut idx = 0;
        while idx < P {
            table.pins[idx] = pins[idx];
            idx += 1;
        }
        idx = 0;
        while idx < E {
            table.extra_verbs[idx] = extra_verbs[idx];
            idx += 1;
        }

        self.southbridge.hda_verbs[self.southbridge.hda_verb_count] = table;
        self.southbridge.hda_verb_count += 1;
        self
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.firmware_size == 0
            || self
                .firmware_base
                .checked_add(self.firmware_size as u64)
                .is_none()
        {
            panic!("GM965/ICH8 firmware window is invalid");
        }
        if self.southbridge.lpc_fixed_io.com_a as u8 == self.southbridge.lpc_fixed_io.com_b as u8 {
            panic!("ICH8 COMA and COMB decode the same port");
        }
        if let Some(sata) = self.southbridge.sata {
            if sata.ports == 0 {
                panic!("ICH8 SATA enabled with no ports");
            }
        }

        let mut idx = 0;
        while idx < self.southbridge.lpc_generic_io_count {
            let range = self.southbridge.lpc_generic_io[idx];
            if !valid_lpc_generic_io(range) {
                panic!("ICH8 LPC generic I/O decode is invalid");
            }

            let mut next = idx + 1;
            while next < self.southbridge.lpc_generic_io_count {
                if ranges_overlap(
                    range.base,
                    range.size,
                    self.southbridge.lpc_generic_io[next].base,
                    self.southbridge.lpc_generic_io[next].size,
                ) {
                    panic!("ICH8 LPC generic I/O decodes overlap");
                }
                next += 1;
            }
            idx += 1;
        }

        idx = 0;
        while idx < self.southbridge.io_trap_count {
            if !valid_io_trap(self.southbridge.io_traps[idx]) {
                panic!("ICH8 I/O trap is invalid");
            }
            idx += 1;
        }

        self
    }

    #[cfg(feature = "recipe")]
    pub(crate) fn gm965_driver_config(&self) -> gm965::IntelGm965Config {
        gm965::IntelGm965Config {
            mchbar: self.northbridge.mchbar,
            dmibar: self.northbridge.dmibar,
            epbar: self.northbridge.epbar,
            ecam_base: self.northbridge.ecam_base,
            ecam_buses: self.northbridge.ecam_buses,
            enable_peg: self.northbridge.enable_peg,
            igd: self.northbridge.igd.to_driver_config(),
            smbus_base: self.northbridge.smbus_base,
            spd_addresses: self.northbridge.spd_addresses,
            acpi_name: self.northbridge.acpi_name.map(hstr),
        }
    }

    #[cfg(feature = "recipe")]
    pub(crate) fn ich8_driver_config(&self) -> ich8::IntelIch8Config {
        ich8::IntelIch8Config {
            rcba: self.southbridge.rcba,
            dmibar: self.southbridge.dmibar,
            pirq_routing: self.southbridge.pirq_routing,
            gpe0_en: self.southbridge.gpe0_en,
            gpi_routing: self.southbridge.gpi_routing,
            alt_gp_smi_en: self.southbridge.alt_gp_smi_en,
            c4_on_c3: self.southbridge.c4_on_c3,
            c5_enable: self.southbridge.c5_enable,
            c6_enable: self.southbridge.c6_enable,
            lpc_decode: ich8::LpcDecodeConfig {
                fixed_io: self.southbridge.lpc_fixed_io,
                generic_io: hvec_prefix(
                    &self.southbridge.lpc_generic_io,
                    self.southbridge.lpc_generic_io_count,
                ),
            },
            hda: self.hda_driver_config(),
            ide: self.southbridge.ide,
            sata: self.southbridge.sata,
            usb: self.southbridge.usb,
            pcie_ports: self.southbridge.pcie_ports,
            pcie_slots: self.southbridge.pcie_slots,
            pcie_power_limits: self.southbridge.pcie_power_limits,
            io_traps: hvec_prefix(&self.southbridge.io_traps, self.southbridge.io_trap_count),
            smbus_base: self.southbridge.smbus_base,
            gpio: gpio::GpioConfig {
                pins: hvec_prefix(&self.southbridge.gpio_pins, self.southbridge.gpio_pin_count),
            },
            acpi_name: self.southbridge.acpi_name.map(hstr),
            c3_latency: self.southbridge.c3_latency,
            power_on_after_fail: self.southbridge.power_on_after_fail,
            throttle_duty: self.southbridge.throttle_duty,
            disable_lan: self.southbridge.disable_lan,
            disable_sata2: self.southbridge.disable_sata2,
            disable_thermal: self.southbridge.disable_thermal,
        }
    }

    #[cfg(feature = "recipe")]
    fn hda_driver_config(&self) -> Option<HdaConfig> {
        if self.southbridge.hda_verb_count == 0 {
            return None;
        }

        let mut verbs = HVec::new();
        let mut idx = 0;
        while idx < self.southbridge.hda_verb_count {
            let table = self.southbridge.hda_verbs[idx];
            verbs
                .push(HdaVerbTable {
                    vendor_id: table.vendor_id,
                    subsystem_id: table.subsystem_id,
                    pins: hvec_prefix(&table.pins, table.pin_count),
                    extra_verbs: hvec_prefix(&table.extra_verbs, table.extra_verb_count),
                })
                .expect("HDA verb table capacity");
            idx += 1;
        }
        Some(HdaConfig { verbs })
    }
}

impl Default for Gm965Ich8Config {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "recipe")]
impl Gm965IgdConfig {
    fn to_driver_config(self) -> gm965::Gm965IgdConfig {
        gm965::Gm965IgdConfig {
            enable_vga: self.enable_vga,
            enable_pipe_b: self.enable_pipe_b,
            gtt_mmio_base: self.gtt_mmio_base,
            stolen_memory_mb: self.stolen_memory_mb,
            vbt_file: self.vbt_file.map(hstr),
            vbt_addr: self.vbt_addr,
            vbt_size: self.vbt_size,
            legacy_vbt_probe: self.legacy_vbt_probe,
            panel_power_up_delay: self.panel_power_up_delay,
            panel_power_down_delay: self.panel_power_down_delay,
            panel_backlight_on_delay: self.panel_backlight_on_delay,
            panel_backlight_off_delay: self.panel_backlight_off_delay,
            panel_power_cycle_delay: self.panel_power_cycle_delay,
            default_pwm_freq: self.default_pwm_freq,
            duty_cycle: self.duty_cycle,
        }
    }
}

#[cfg(feature = "recipe")]
fn hvec_prefix<T: Copy, const N: usize, const C: usize>(
    items: &[T; N],
    count: usize,
) -> HVec<T, C> {
    let mut out = HVec::new();
    let mut idx = 0;
    while idx < count {
        out.push(items[idx]).ok().expect("heapless vec capacity");
        idx += 1;
    }
    out
}

const fn valid_lpc_generic_io(range: LpcGenericIoDecode) -> bool {
    range.base & 0x0003 == 0 && range.size != 0 && range.size <= 0x0100 && range.size & 0x0003 == 0
}

const fn valid_io_trap(trap: IoTrapConfig) -> bool {
    trap.index <= 3
        && trap.base & 0x3 == 0
        && trap.size != 0
        && trap.size <= 0x100
        && trap.size & 0x3 == 0
        && trap.size.is_power_of_two()
        && trap.base & (trap.size - 1) == 0
}

const fn ranges_overlap(a_base: u16, a_size: u16, b_base: u16, b_size: u16) -> bool {
    let a_end = a_base as u32 + a_size as u32;
    let b_end = b_base as u32 + b_size as u32;
    (a_base as u32) < b_end && (b_base as u32) < a_end
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
