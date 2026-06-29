//! Intel GM965 + ICH8 board-support metadata.
//!
//! Runtime chipset and mainboard initialization stays in the no_std driver crates.
//! This crate composes reusable host-side metadata for GM965/ICH8 mainboards:
//! flash/CAR layout, stage flow, chipset defaults, device topology, and driver
//! bindings. Concrete board crates supply board policy such as dock wiring, GPIOs,
//! HDA verbs, SMBIOS identity, payload choice, and optional peripheral instances.

use fstart_device_registry::{
    DriverInstance, DriverInstanceBinding, PlatformAttachPoint, PlatformDeviceExtensions,
    PlatformTopology,
};
use fstart_driver_intel_gm965 as gm965;
use fstart_driver_intel_ich8 as ich8;
use fstart_gpio_ich as gpio;
use fstart_hda as hda;
use fstart_types::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_types::{
    board_info_from_config, build_info_from_config, dev_security_config, hstr, hvec,
    x86_linuxboot_payload, AcpiConfig, AcpiPlatform, BoardConfig, BoardInfo, BootMedium, BuildInfo,
    BusAddress, Capability, CarConfig, Compression, DeviceRole, FlashLayout, Io16, IoAddr,
    MemoryMap, MemoryRegion, PayloadConfig, Platform, RegionKind, RunsFrom, SmbiosConfig,
    StageConfig, StageLayout, TempRamBuffer,
};
use heapless::Vec as HVec;

pub use fstart_driver_intel_gm965::Gm965IgdConfig;
pub use fstart_driver_intel_ich8::{
    IdeConfig, IoTrapAccess, IoTrapConfig, LpcDecodeConfig, LpcFixedIoDecode, LpcFloppyDecode,
    LpcGenericIoDecode, LpcParallelDecode, LpcSerialDecode, SataConfig, SataMode, UsbConfig,
};

#[derive(Debug, Clone)]
struct RuntimeDevicePolicy {
    instance: DriverInstance,
    enabled: bool,
}

/// Shared GM965 + ICH8 platform metadata for a concrete mainboard.
#[derive(Debug, Clone)]
pub struct Gm965Ich8Platform {
    board_name: &'static str,
    board_package: &'static str,
    payload: PayloadConfig,
    flash_layout: Option<FlashLayout>,
    gm965: gm965::IntelGm965Config,
    ich8: ich8::IntelIch8Config,
    smbios: Option<SmbiosConfig>,
    mainboard: Option<RuntimeDevicePolicy>,
    extensions: PlatformDeviceExtensions,
    acpi_print_hex: bool,
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

impl Gm965Ich8Platform {
    #[must_use]
    pub fn new(board_name: &'static str, board_package: &'static str) -> Self {
        Self {
            board_name,
            board_package,
            payload: x86_linuxboot_payload(),
            flash_layout: None,
            gm965: gm965_defaults(),
            ich8: ich8_defaults(),
            smbios: None,
            mainboard: None,
            extensions: PlatformDeviceExtensions::new(),
            acpi_print_hex: false,
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

    /// Set board-specific SMBIOS static identity metadata.
    pub fn smbios(mut self, smbios: SmbiosConfig) -> Self {
        self.smbios = Some(smbios);
        self
    }

    /// Attach the board-specific mainboard hook driver.
    pub fn mainboard(mut self, mainboard: DriverInstance) -> Self {
        self.mainboard = Some(RuntimeDevicePolicy {
            instance: mainboard,
            enabled: true,
        });
        self
    }

    /// Attach a SuperIO chip on the ICH8 LPC bus.
    pub fn superio(
        mut self,
        name: &str,
        address: IoAddr<Io16>,
        instance: DriverInstance,
        enabled: bool,
    ) -> Self {
        self.extensions.on("lpc", |lpc| {
            lpc.runtime_enabled(name, BusAddress::Lpc(address.raw()), enabled, instance);
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
            build: Default::default(),
            boot_hart_id: 0,
        }
    }

    #[must_use]
    pub fn driver_bindings(&self) -> Vec<DriverInstanceBinding> {
        self.platform_topology().build().1
    }

    #[must_use]
    pub fn board_info(&self) -> BoardInfo {
        board_info_from_config(self.board_config())
    }

    #[must_use]
    pub fn build_info(&self) -> BuildInfo {
        let config = self.board_config();
        let drivers = self.driver_bindings();
        build_info_from_config(
            self.board_name,
            self.board_package,
            &config,
            drivers
                .iter()
                .filter_map(DriverInstanceBinding::driver_feature),
        )
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

impl Gm965Ich8Platform {
    fn platform_topology(&self) -> PlatformTopology {
        PlatformTopology::new()
            .root(
                "northbridge",
                DriverInstance::IntelGm965(self.gm965.clone()),
            )
            .root("southbridge", DriverInstance::IntelIch8(self.ich8.clone()))
            .pci_bridge("southbridge", "pcie1", 0x1c, 0, self.ich8.pcie_ports[0])
            .pci_bridge("southbridge", "pcie2", 0x1c, 1, self.ich8.pcie_ports[1])
            .pci_bridge("southbridge", "pcie3", 0x1c, 2, self.ich8.pcie_ports[2])
            .pci_bridge("southbridge", "pcie4", 0x1c, 3, self.ich8.pcie_ports[3])
            .pci_bridge("southbridge", "pcie5", 0x1c, 4, self.ich8.pcie_ports[4])
            .pci_bridge("southbridge", "pcie6", 0x1c, 5, self.ich8.pcie_ports[5])
            .child_bus("southbridge", "lpc", DeviceRole::LpcBus)
            .child_bus("southbridge", "smbus", DeviceRole::SmBus)
            .root_enabled(
                "mainboard",
                self.mainboard.as_ref().is_none_or(|policy| policy.enabled),
                self.mainboard
                    .as_ref()
                    .expect("mainboard must provide mainboard hook config")
                    .instance
                    .clone(),
            )
            .extend(&self.extensions)
    }
}

fn gm965_ich8_memory(flash_layout: Option<FlashLayout>) -> MemoryMap {
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

fn gm965_ich8_stages() -> StageLayout {
    StageLayout::MultiStage(hvec([
        StageConfig {
            name: hstr("bootblock"),
            capabilities: gm965_bootblock_capabilities(),
            load_addr: 0,
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
            load_addr: 0x0400_0000,
            stack_size: 0x400000,
            heap_size: Some(0x200000),
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

fn gm965_defaults() -> gm965::IntelGm965Config {
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

fn ich8_defaults() -> ich8::IntelIch8Config {
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
