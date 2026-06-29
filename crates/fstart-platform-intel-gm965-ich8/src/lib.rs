//! Intel GM965 + ICH8 board-support metadata.
//!
//! Runtime chipset and mainboard initialization stays in the no_std driver crates.
//! This crate composes reusable host-side metadata for GM965/ICH8 mainboards:
//! flash/CAR layout, stage flow, chipset defaults, device topology, and driver
//! bindings. Concrete board crates supply board policy such as dock wiring, GPIOs,
//! HDA verbs, SMBIOS identity, payload choice, and optional peripheral instances.

use fstart_device_registry::{DriverBinding, DriverInstance};
use fstart_driver_intel_gm965 as gm965;
use fstart_driver_intel_ich8 as ich8;
use fstart_gpio_ich as gpio;
use fstart_hda as hda;
use fstart_types::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_types::{
    board_info_from_config, build_info_from_config, dev_security_config, hstr, hvec, io16,
    x86_linuxboot_payload, AcpiConfig, AcpiPlatform, BoardConfig, BoardInfo, BootMedium, BuildInfo,
    BusAddress, Capability, CarConfig, Compression, DeviceConfig, DeviceTopology, FlashLayout,
    IntelIfdFlashLayout, IntelIfdRegion, IntelIfdRegionConfig, MemoryMap, MemoryRegion,
    PayloadConfig, Platform, RegionKind, RunsFrom, SmbiosConfig, StageConfig, StageLayout,
    TempRamBuffer,
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
    igd: Gm965IgdConfig,
    hda: Option<hda::HdaConfig>,
    gpio: gpio::GpioConfig,
    smbios: Option<SmbiosConfig>,
    mainboard: Option<RuntimeDevicePolicy>,
    dlpc_superio: Option<RuntimeDevicePolicy>,
    dock_superio: Option<RuntimeDevicePolicy>,
    uart0: Option<RuntimeDevicePolicy>,
    clock_generator: Option<RuntimeDevicePolicy>,
    pcie_ports: [bool; 6],
    pcie_slots: [bool; 6],
    pcie_power_limits: [ich8::PciePowerLimit; 6],
    lpc_decode: LpcDecodeConfig,
    gpe0_en: u32,
    gpi_routing: [u8; 16],
    c4_on_c3: bool,
    c5_enable: bool,
    c6_enable: bool,
    ide: Option<IdeConfig>,
    sata: Option<SataConfig>,
    usb: Option<UsbConfig>,
    io_traps: HVec<IoTrapConfig, 4>,
    c3_latency: u16,
    power_on_after_fail: u8,
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
            igd: Gm965IgdConfig::default(),
            hda: None,
            gpio: gpio::GpioConfig::default(),
            smbios: None,
            mainboard: None,
            dlpc_superio: None,
            dock_superio: None,
            uart0: None,
            clock_generator: None,
            pcie_ports: [false; 6],
            pcie_slots: [false; 6],
            pcie_power_limits: [ich8::PciePowerLimit::default(); 6],
            lpc_decode: LpcDecodeConfig::default(),
            gpe0_en: 0,
            gpi_routing: [0; 16],
            c4_on_c3: false,
            c5_enable: false,
            c6_enable: false,
            ide: None,
            sata: None,
            usb: None,
            io_traps: HVec::new(),
            c3_latency: 85,
            power_on_after_fail: 0,
            acpi_print_hex: false,
        }
    }

    #[must_use]
    pub fn payload(mut self, payload: PayloadConfig) -> Self {
        self.payload = payload;
        self
    }

    /// Set board-specific integrated graphics policy, including VBT source.
    #[must_use]
    pub fn igd(mut self, igd: Gm965IgdConfig) -> Self {
        self.igd = igd;
        self
    }

    /// Set board-specific HD Audio verb tables.
    #[must_use]
    pub fn hda(mut self, hda: hda::HdaConfig) -> Self {
        self.hda = Some(hda);
        self
    }

    /// Set board-specific ICH GPIO pad configuration.
    #[must_use]
    pub fn gpio(mut self, gpio: gpio::GpioConfig) -> Self {
        self.gpio = gpio;
        self
    }

    /// Set board-specific SMBIOS static identity metadata.
    #[must_use]
    pub fn smbios(mut self, smbios: SmbiosConfig) -> Self {
        self.smbios = Some(smbios);
        self
    }

    /// Attach the board-specific mainboard hook driver.
    #[must_use]
    pub fn mainboard(mut self, mainboard: DriverInstance) -> Self {
        self.mainboard = Some(RuntimeDevicePolicy {
            instance: mainboard,
            enabled: true,
        });
        self
    }

    /// Attach the laptop-side DLPC/Super I/O driver.
    #[must_use]
    pub fn dlpc_superio(mut self, superio: DriverInstance) -> Self {
        self.dlpc_superio = Some(RuntimeDevicePolicy {
            instance: superio,
            enabled: true,
        });
        self
    }

    /// Attach the optional dock-side Super I/O driver.
    #[must_use]
    pub fn dock_superio(mut self, superio: DriverInstance, enabled: bool) -> Self {
        self.dock_superio = Some(RuntimeDevicePolicy {
            instance: superio,
            enabled,
        });
        self
    }

    /// Attach the platform UART driver.
    #[must_use]
    pub fn uart0(mut self, uart: DriverInstance) -> Self {
        self.uart0 = Some(RuntimeDevicePolicy {
            instance: uart,
            enabled: true,
        });
        self
    }

    /// Attach the optional CK505/clock-generator driver.
    #[must_use]
    pub fn clock_generator(mut self, clock_generator: DriverInstance, enabled: bool) -> Self {
        self.clock_generator = Some(RuntimeDevicePolicy {
            instance: clock_generator,
            enabled,
        });
        self
    }

    /// Set board policy for one ICH8 PCIe root port.
    #[must_use]
    pub fn pcie_port(mut self, port: PcieRootPort, enabled: bool) -> Self {
        self.pcie_ports[port.index()] = enabled;
        self
    }

    /// Mark an ICH8 PCIe root port as a physical slot.
    #[must_use]
    pub fn pcie_slot(mut self, port: PcieRootPort, is_slot: bool) -> Self {
        self.pcie_slots[port.index()] = is_slot;
        self
    }

    /// Set PCIe slot power-limit encoding for one root port.
    #[must_use]
    pub fn pcie_power_limit(mut self, port: PcieRootPort, limit: ich8::PciePowerLimit) -> Self {
        self.pcie_power_limits[port.index()] = limit;
        self
    }

    /// Set board-selected LPC fixed decode policy.
    #[must_use]
    pub fn lpc_fixed_io(mut self, fixed_io: LpcFixedIoDecode) -> Self {
        self.lpc_decode.fixed_io = fixed_io;
        self
    }

    /// Add one board-selected LPC generic I/O decode window.
    #[must_use]
    pub fn lpc_generic_io(mut self, decode: LpcGenericIoDecode) -> Self {
        self.lpc_decode
            .generic_io
            .push(decode)
            .expect("ICH8 LPC generic I/O decode capacity");
        self
    }

    /// Set board-selected ACPI GPE0 enable bits.
    #[must_use]
    pub const fn gpe0_en(mut self, value: u32) -> Self {
        self.gpe0_en = value;
        self
    }

    /// Set board-selected GPI routing policy for GPIO0..15.
    #[must_use]
    pub const fn gpi_routing(mut self, value: [u8; 16]) -> Self {
        self.gpi_routing = value;
        self
    }

    /// Enable C4-on-C3 mobile power policy.
    #[must_use]
    pub const fn c4_on_c3(mut self, enabled: bool) -> Self {
        self.c4_on_c3 = enabled;
        self
    }

    /// Enable C5 PMSYNC policy.
    #[must_use]
    pub const fn c5_enable(mut self, enabled: bool) -> Self {
        self.c5_enable = enabled;
        self
    }

    /// Enable C6 PMSYNC exit timing policy.
    #[must_use]
    pub const fn c6_enable(mut self, enabled: bool) -> Self {
        self.c6_enable = enabled;
        self
    }

    /// Enable and configure IDE/PATA for this board.
    #[must_use]
    pub const fn ide(mut self, ide: IdeConfig) -> Self {
        self.ide = Some(ide);
        self
    }

    /// Enable and configure SATA for this board.
    #[must_use]
    pub const fn sata(mut self, sata: SataConfig) -> Self {
        self.sata = Some(sata);
        self
    }

    /// Enable and configure USB controllers for this board.
    #[must_use]
    pub const fn usb(mut self, usb: UsbConfig) -> Self {
        self.usb = Some(usb);
        self
    }

    /// Add one board-selected ICH8 I/O trap.
    #[must_use]
    pub fn io_trap(mut self, trap: IoTrapConfig) -> Self {
        self.io_traps.push(trap).expect("ICH8 I/O trap capacity");
        self
    }

    /// Set the ACPI C3 latency value in microseconds.
    #[must_use]
    pub const fn c3_latency(mut self, value: u16) -> Self {
        self.c3_latency = value;
        self
    }

    /// Set after-power-failure behaviour: 0=off, 1=on, 2=last-state.
    #[must_use]
    pub const fn power_on_after_fail(mut self, value: u8) -> Self {
        self.power_on_after_fail = value;
        self
    }

    /// Enable ACPI debug hex printing for this board.
    #[must_use]
    pub const fn acpi_print_hex(mut self, enabled: bool) -> Self {
        self.acpi_print_hex = enabled;
        self
    }

    #[must_use]
    pub fn board_config(&self) -> BoardConfig {
        BoardConfig {
            name: hstr(self.board_name),
            platform: Platform::X86_64,
            memory: gm965_ich8_memory(),
            devices: gm965_ich8_devices(
                self.pcie_ports,
                self.dlpc_superio
                    .as_ref()
                    .map_or(true, |policy| policy.enabled),
                self.dock_superio
                    .as_ref()
                    .map_or(false, |policy| policy.enabled),
                self.uart0.as_ref().map_or(true, |policy| policy.enabled),
                self.clock_generator
                    .as_ref()
                    .map_or(false, |policy| policy.enabled),
                self.mainboard
                    .as_ref()
                    .map_or(true, |policy| policy.enabled),
            ),
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
            boot_hart_id: 0,
        }
    }

    #[must_use]
    pub fn driver_bindings(&self) -> Vec<DriverBinding> {
        let mut bindings = vec![
            DriverInstance::IntelGm965(gm965_config(self.igd.clone())).bind("northbridge"),
            DriverInstance::IntelIch8(ich8_config(
                self.hda.clone(),
                self.gpio.clone(),
                self.lpc_decode.clone(),
                self.gpe0_en,
                self.gpi_routing,
                self.c4_on_c3,
                self.c5_enable,
                self.c6_enable,
                self.ide,
                self.sata,
                self.usb,
                self.pcie_ports,
                self.pcie_slots,
                self.pcie_power_limits,
                self.io_traps.clone(),
                self.c3_latency,
                self.power_on_after_fail,
            ))
            .bind("southbridge"),
        ];

        bindings.push(
            self.dlpc_superio
                .as_ref()
                .expect("mainboard must provide laptop-side DLPC Super I/O config")
                .instance
                .clone()
                .bind("dlpc_superio"),
        );
        bindings.push(
            self.dock_superio
                .as_ref()
                .expect("mainboard must provide dock-side Super I/O config")
                .instance
                .clone()
                .bind("dock_superio"),
        );
        bindings.push(
            self.uart0
                .as_ref()
                .expect("mainboard must provide UART config")
                .instance
                .clone()
                .bind("uart0"),
        );
        bindings.push(
            self.clock_generator
                .as_ref()
                .expect("mainboard must provide CK505 config")
                .instance
                .clone()
                .bind("ck505"),
        );
        bindings.push(
            self.mainboard
                .as_ref()
                .expect("mainboard must provide mainboard hook config")
                .instance
                .clone()
                .bind("mainboard"),
        );

        bindings
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
            drivers.iter().filter_map(DriverBinding::driver_feature),
        )
    }
}

fn gm965_ich8_memory() -> MemoryMap {
    MemoryMap {
        regions: hvec([MemoryRegion {
            name: hstr("workram"),
            base: 0x0010_0000,
            size: 0x3FF0_0000,
            kind: RegionKind::Ram,
        }]),
        flash_layout: Some(FlashLayout::IntelIfd(IntelIfdFlashLayout {
            base: 0xFFC0_0000,
            size: 0x0040_0000,
            regions: hvec([
                IntelIfdRegionConfig {
                    kind: IntelIfdRegion::Descriptor,
                    offset: 0x000000,
                    size: 0x001000,
                    file: None,
                },
                IntelIfdRegionConfig {
                    kind: IntelIfdRegion::Gbe,
                    offset: 0x001000,
                    size: 0x002000,
                    file: None,
                },
                IntelIfdRegionConfig {
                    kind: IntelIfdRegion::Me,
                    offset: 0x003000,
                    size: 0x27D000,
                    file: None,
                },
                IntelIfdRegionConfig {
                    kind: IntelIfdRegion::Bios,
                    offset: 0x280000,
                    size: 0x180000,
                    file: None,
                },
            ]),
        })),
        car: Some(CarConfig {
            base: 0xFEF0_0000,
            size: 0x80000,
        }),
    }
}

fn gm965_ich8_devices(
    pcie_ports: [bool; 6],
    dlpc_superio_enabled: bool,
    dock_superio_enabled: bool,
    uart0_enabled: bool,
    clock_generator_enabled: bool,
    mainboard_enabled: bool,
) -> HVec<DeviceConfig, 32> {
    DeviceTopology::new()
        .root("northbridge")
        .root("southbridge")
        .pci_bridge("southbridge", "pcie1", 0x1c, 0, pcie_ports[0])
        .pci_bridge("southbridge", "pcie2", 0x1c, 1, pcie_ports[1])
        .pci_bridge("southbridge", "pcie3", 0x1c, 2, pcie_ports[2])
        .pci_bridge("southbridge", "pcie4", 0x1c, 3, pcie_ports[3])
        .pci_bridge("southbridge", "pcie5", 0x1c, 4, pcie_ports[4])
        .pci_bridge("southbridge", "pcie6", 0x1c, 5, pcie_ports[5])
        .lpc_bus("southbridge", "lpc", |lpc| {
            lpc.runtime_child(
                "dlpc_superio",
                BusAddress::Lpc(0x164e),
                dlpc_superio_enabled,
            )
            .runtime_child("dock_superio", BusAddress::Lpc(0x2e), dock_superio_enabled)
            .runtime_child("uart0", BusAddress::Lpc(io16(0x3f8).raw()), uart0_enabled)
        })
        .smbus("southbridge", "smbus", |smbus| {
            smbus.runtime_child("ck505", BusAddress::I2c(0x69), clock_generator_enabled)
        })
        .runtime_root("mainboard", mainboard_enabled)
        .build()
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

fn gm965_config(igd: Gm965IgdConfig) -> gm965::IntelGm965Config {
    gm965::IntelGm965Config {
        mchbar: 0xFED1_4000,
        dmibar: 0xFED1_8000,
        epbar: 0xFED1_9000,
        ecam_base: 0xE000_0000,
        ecam_buses: 64,
        enable_peg: false,
        igd,
        smbus_base: 0x0400,
        spd_addresses: [0x50, 0, 0x51, 0],
        acpi_name: Some(hstr("PCI0")),
    }
}

#[allow(clippy::too_many_arguments)]
fn ich8_config(
    hda: Option<hda::HdaConfig>,
    gpio: gpio::GpioConfig,
    lpc_decode: LpcDecodeConfig,
    gpe0_en: u32,
    gpi_routing: [u8; 16],
    c4_on_c3: bool,
    c5_enable: bool,
    c6_enable: bool,
    ide: Option<IdeConfig>,
    sata: Option<SataConfig>,
    usb: Option<UsbConfig>,
    pcie_ports: [bool; 6],
    pcie_slots: [bool; 6],
    pcie_power_limits: [ich8::PciePowerLimit; 6],
    io_traps: HVec<IoTrapConfig, 4>,
    c3_latency: u16,
    power_on_after_fail: u8,
) -> ich8::IntelIch8Config {
    ich8::IntelIch8Config {
        rcba: 0xFED1_C000,
        dmibar: 0xFED1_8000,
        pirq_routing: [0x0b; 8],
        gpe0_en,
        gpi_routing,
        alt_gp_smi_en: 0,
        c4_on_c3,
        c5_enable,
        c6_enable,
        lpc_decode,
        hda,
        ide,
        sata,
        usb,
        pcie_ports,
        pcie_slots,
        pcie_power_limits,
        io_traps,
        smbus_base: 0x0400,
        gpio,
        acpi_name: Some(hstr("LPCB")),
        c3_latency,
        power_on_after_fail,
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
