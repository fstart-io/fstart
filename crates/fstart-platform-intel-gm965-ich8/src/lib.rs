//! Intel GM965 + ICH8 board-support metadata.
//!
//! Runtime chipset and mainboard initialization stays in the no_std driver crates.
//! This crate composes reusable host-side metadata for GM965/ICH8 mainboards:
//! flash/CAR layout, stage flow, chipset defaults, and device topology. Concrete
//! board crates supply board policy such as dock wiring, GPIOs,
//! HDA verbs, SMBIOS identity, payload choice, and optional peripheral instances.

#![no_std]

use fstart_board_meta::{PlatformAttachPoint, PlatformDeviceExtensions, PlatformTopology};
use fstart_driver_intel_gm965 as gm965;
use fstart_driver_intel_ich8 as ich8;
use fstart_gpio_ich as gpio;
use fstart_hda as hda;
use fstart_types::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_types::{
    board_info_from_config, build_info_from_config, dev_security_config, hstr, hvec,
    x86_linuxboot_payload, AcpiConfig, AcpiPlatform, BoardConfig, BoardInfo, BootMedium, BuildInfo,
    BusAddress, Capability, CarConfig, Compression, DeviceRole, FlashLayout, IntelIfdRegion, Io16,
    IoAddr, MemoryMap, MemoryRegion, PayloadConfig, Platform, RegionKind, RunsFrom, SmbiosConfig,
    StageConfig, StageLayout, TempRamBuffer,
};
use heapless::Vec as HVec;

pub use fstart_driver_intel_gm965::Gm965IgdConfig;
pub use fstart_driver_intel_ich8::{
    IdeConfig, IoTrapAccess, IoTrapConfig, LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode,
    LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, SataConfig, SataMode, UsbConfig,
};

pub const GM965_NORTHBRIDGE_NODE: &str = "northbridge";
pub const ICH8_SOUTHBRIDGE_NODE: &str = "southbridge";
pub const GM965_ICH8_MAINBOARD_NODE: &str = "mainboard";
pub const GM965_DEFAULT_FIRMWARE_BASE: u64 = 0xFFE8_0000;
pub const GM965_DEFAULT_FIRMWARE_SIZE: usize = 0x0018_0000;
pub const GM965_BOOTBLOCK_LOAD_ADDR: u64 = 0xffff_ffff;
pub const GM965_RAMSTAGE_LOAD_ADDR: u64 = 0x0400_0000;
pub const GM965_RAMSTAGE_HEAP_SIZE: usize = 0x200000;
pub const GM965_NEXT_STAGE_NAME: &str = "ramstage";
pub const ICH8_PMBASE: u32 = 0x0500;

/// Runtime chipset policy derived from the same platform builder used for host metadata.
#[derive(Debug, Clone)]
pub struct Gm965Ich8RuntimeConfig {
    pub gm965: gm965::IntelGm965Config,
    pub ich8: ich8::IntelIch8Config,
    pub firmware_base: u64,
    pub firmware_size: usize,
    pub ram_base: u64,
    pub ram_size: u64,
}

/// Shared GM965 + ICH8 host metadata for a concrete mainboard.
///
/// This builder intentionally carries only host-visible board facts: payload
/// policy, flash layout, table metadata, and topology. Target-side driver
/// policy belongs in [`Gm965Ich8RuntimePolicy`], so host tools can construct a
/// [`BoardConfig`] without also constructing HDA, GPIO, IGD, LPC decode, or
/// other runtime-only configs.
#[derive(Debug, Clone)]
pub struct Gm965Ich8Board {
    board_name: &'static str,
    board_package: &'static str,
    payload: PayloadConfig,
    flash_layout: Option<FlashLayout>,
    smbios: Option<SmbiosConfig>,
    mainboard_enabled: bool,
    extensions: PlatformDeviceExtensions,
    acpi_print_hex: bool,
    pcie_ports: [bool; 6],
}

/// Target-side GM965 + ICH8 runtime driver policy.
#[derive(Debug, Clone)]
pub struct Gm965Ich8RuntimePolicy {
    gm965: gm965::IntelGm965Config,
    ich8: ich8::IntelIch8Config,
    flash_layout: Option<FlashLayout>,
}

/// ICH8 PCIe root-port selector.
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

impl Gm965Ich8Board {
    #[must_use]
    pub fn new(board_name: &'static str, board_package: &'static str) -> Self {
        Self {
            board_name,
            board_package,
            payload: x86_linuxboot_payload(),
            flash_layout: None,
            smbios: None,
            mainboard_enabled: false,
            extensions: PlatformDeviceExtensions::new(),
            acpi_print_hex: false,
            pcie_ports: [false; 6],
        }
    }

    pub fn payload(mut self, payload: PayloadConfig) -> Self {
        self.payload = payload;
        self
    }

    /// Set the board flash layout.
    pub fn flash_layout(mut self, flash_layout: Option<FlashLayout>) -> Self {
        self.flash_layout = flash_layout;
        self
    }

    /// Set board-specific SMBIOS static identity metadata.
    pub fn smbios(mut self, smbios: SmbiosConfig) -> Self {
        self.smbios = Some(smbios);
        self
    }

    /// Declare the board-specific mainboard hook device.
    pub const fn mainboard(mut self) -> Self {
        self.mainboard_enabled = true;
        self
    }

    /// Attach a SuperIO chip on the ICH8 LPC bus.
    pub fn superio(mut self, name: &str, address: IoAddr<Io16>, enabled: bool) -> Self {
        self.extensions.on("lpc", |lpc| {
            lpc.runtime_enabled(name, BusAddress::Lpc(address.raw()), enabled);
        });
        self
    }

    /// Add board devices below the platform-owned LPC bus.
    pub fn on_lpc<F>(mut self, extend: F) -> Self
    where
        F: FnOnce(&mut PlatformAttachPoint<'_>),
    {
        self.extensions.on("lpc", extend);
        self
    }

    /// Add board devices below the platform-owned SMBus.
    pub fn on_smbus<F>(mut self, extend: F) -> Self
    where
        F: FnOnce(&mut PlatformAttachPoint<'_>),
    {
        self.extensions.on("smbus", extend);
        self
    }

    /// Add board devices below one platform-owned PCIe root port.
    pub fn on_pcie<F>(mut self, port: PcieRootPort, extend: F) -> Self
    where
        F: FnOnce(&mut PlatformAttachPoint<'_>),
    {
        self.extensions.on(port.node_name(), extend);
        self
    }

    /// Set board policy for one ICH8 PCIe root port.
    pub fn pcie_port(mut self, port: PcieRootPort, enabled: bool) -> Self {
        self.pcie_ports[port.index()] = enabled;
        self
    }

    /// Enable ACPI debug hex printing for this board.
    pub const fn acpi_print_hex(mut self, enabled: bool) -> Self {
        self.acpi_print_hex = enabled;
        self
    }

    #[must_use]
    pub fn board_config(&self) -> BoardConfig {
        BoardConfig {
            name: hstr(self.board_name),
            platform: Platform::X86_64,
            memory: gm965_ich8_memory(self.flash_layout.clone()),
            devices: self.platform_topology().build_devices(),
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
                cpu_feature: Some(fstart_types::hstr("cpu-intel-core2")),
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
}

impl Gm965Ich8RuntimePolicy {
    #[must_use]
    pub fn new() -> Self {
        Self {
            gm965: gm965_defaults(),
            ich8: ich8_defaults(),
            flash_layout: None,
        }
    }

    /// Set the board flash layout used to derive the firmware window.
    pub fn flash_layout(mut self, flash_layout: Option<FlashLayout>) -> Self {
        self.flash_layout = flash_layout;
        self
    }

    /// Set board-specific integrated graphics policy, including VBT source.
    pub fn igd(mut self, igd: Gm965IgdConfig) -> Self {
        self.gm965.igd = igd;
        self
    }

    /// Override the northbridge config defaults directly.
    pub fn gm965<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(&mut gm965::IntelGm965Config),
    {
        configure(&mut self.gm965);
        self
    }

    /// Override the southbridge config defaults directly.
    pub fn ich8<F>(mut self, configure: F) -> Self
    where
        F: FnOnce(&mut ich8::IntelIch8Config),
    {
        configure(&mut self.ich8);
        self
    }

    /// Set board-specific HD Audio verb tables.
    pub fn hda(mut self, hda: hda::HdaConfig) -> Self {
        self.ich8.hda = Some(hda);
        self
    }

    /// Set board-specific ICH GPIO pad configuration.
    pub fn gpio(mut self, gpio: gpio::GpioConfig) -> Self {
        self.ich8.gpio = gpio;
        self
    }

    /// Set board policy for one ICH8 PCIe root port.
    pub fn pcie_port(mut self, port: PcieRootPort, enabled: bool) -> Self {
        self.ich8.pcie_ports[port.index()] = enabled;
        self
    }

    /// Mark an ICH8 PCIe root port as a physical slot.
    pub fn pcie_slot(mut self, port: PcieRootPort, is_slot: bool) -> Self {
        self.ich8.pcie_slots[port.index()] = is_slot;
        self
    }

    /// Set PCIe slot power-limit encoding for one root port.
    pub fn pcie_power_limit(mut self, port: PcieRootPort, limit: ich8::PciePowerLimit) -> Self {
        self.ich8.pcie_power_limits[port.index()] = limit;
        self
    }

    /// Set board-selected LPC fixed decode policy.
    pub fn lpc_fixed_io(mut self, fixed_io: LpcFixedIoDecode) -> Self {
        self.ich8.lpc_decode.fixed_io = fixed_io;
        self
    }

    /// Add one board-selected LPC generic I/O decode window.
    pub fn lpc_generic_io(mut self, decode: LpcGenericIoDecode) -> Self {
        self.ich8
            .lpc_decode
            .generic_io
            .push(decode)
            .expect("ICH8 LPC generic I/O decode capacity");
        self
    }

    /// Set board-selected ACPI GPE0 enable bits.
    pub const fn gpe0_en(mut self, value: u32) -> Self {
        self.ich8.gpe0_en = value;
        self
    }

    /// Set board-selected GPI routing policy for GPIO0..15.
    pub const fn gpi_routing(mut self, value: [u8; 16]) -> Self {
        self.ich8.gpi_routing = value;
        self
    }

    /// Enable C4-on-C3 mobile power policy.
    pub const fn c4_on_c3(mut self, enabled: bool) -> Self {
        self.ich8.c4_on_c3 = enabled;
        self
    }

    /// Enable C5 PMSYNC policy.
    pub const fn c5_enable(mut self, enabled: bool) -> Self {
        self.ich8.c5_enable = enabled;
        self
    }

    /// Enable C6 PMSYNC exit timing policy.
    pub const fn c6_enable(mut self, enabled: bool) -> Self {
        self.ich8.c6_enable = enabled;
        self
    }

    /// Enable and configure IDE/PATA for this board.
    pub const fn ide(mut self, ide: IdeConfig) -> Self {
        self.ich8.ide = Some(ide);
        self
    }

    /// Enable and configure SATA for this board.
    pub const fn sata(mut self, sata: SataConfig) -> Self {
        self.ich8.sata = Some(sata);
        self
    }

    /// Enable and configure USB controllers for this board.
    pub const fn usb(mut self, usb: UsbConfig) -> Self {
        self.ich8.usb = Some(usb);
        self
    }

    /// Add one board-selected ICH8 I/O trap.
    pub fn io_trap(mut self, trap: IoTrapConfig) -> Self {
        self.ich8
            .io_traps
            .push(trap)
            .expect("ICH8 I/O trap capacity");
        self
    }

    /// Set the ACPI C3 latency value in microseconds.
    pub const fn c3_latency(mut self, value: u16) -> Self {
        self.ich8.c3_latency = value;
        self
    }

    /// Set after-power-failure behaviour: 0=off, 1=on, 2=last-state.
    pub const fn power_on_after_fail(mut self, value: u8) -> Self {
        self.ich8.power_on_after_fail = value;
        self
    }

    /// Return target-side runtime policy for chipset drivers.
    #[must_use]
    pub fn runtime_config(&self) -> Gm965Ich8RuntimeConfig {
        let (firmware_base, firmware_size) = firmware_window(self.flash_layout.as_ref());
        Gm965Ich8RuntimeConfig {
            gm965: self.gm965.clone(),
            ich8: self.ich8.clone(),
            firmware_base,
            firmware_size,
            ram_base: 0x0010_0000,
            ram_size: 0x3FF0_0000,
        }
    }
}

impl Default for Gm965Ich8RuntimePolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl PcieRootPort {
    const fn node_name(self) -> &'static str {
        match self {
            Self::Port1 => "pcie1",
            Self::Port2 => "pcie2",
            Self::Port3 => "pcie3",
            Self::Port4 => "pcie4",
            Self::Port5 => "pcie5",
            Self::Port6 => "pcie6",
        }
    }
}

impl Gm965Ich8Board {
    fn platform_topology(&self) -> PlatformTopology {
        PlatformTopology::new()
            .root("northbridge")
            .root("southbridge")
            .pci_bridge("southbridge", "pcie1", 0x1c, 0, self.pcie_ports[0])
            .pci_bridge("southbridge", "pcie2", 0x1c, 1, self.pcie_ports[1])
            .pci_bridge("southbridge", "pcie3", 0x1c, 2, self.pcie_ports[2])
            .pci_bridge("southbridge", "pcie4", 0x1c, 3, self.pcie_ports[3])
            .pci_bridge("southbridge", "pcie5", 0x1c, 4, self.pcie_ports[4])
            .pci_bridge("southbridge", "pcie6", 0x1c, 5, self.pcie_ports[5])
            .child_bus("southbridge", "lpc", DeviceRole::LpcBus)
            .child_bus("southbridge", "smbus", DeviceRole::SmBus)
            .root_enabled("mainboard", self.mainboard_enabled)
            .extend(&self.extensions)
    }
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
            capabilities: gm965_bootblock_capabilities(),
            load_addr: GM965_BOOTBLOCK_LOAD_ADDR,
            stack_size: 0x2000,
            heap_size: None,
            runs_from: RunsFrom::Rom,
            compression: Compression::None,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
        StageConfig {
            name: hstr("ramstage"),
            capabilities: gm965_ramstage_capabilities(),
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

fn gm965_bootblock_capabilities() -> HVec<Capability, 16> {
    hvec([
        Capability::ConsoleInit,
        Capability::DramInit,
        Capability::BootMedia(BootMedium::FirmwareImage {
            temp_ram_buffer: None,
        }),
        Capability::StageLoad {
            next_stage: hstr("ramstage"),
        },
    ])
}

fn gm965_ramstage_capabilities() -> HVec<Capability, 16> {
    hvec([
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
    ])
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
