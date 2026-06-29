//! Intel Pineview + ICH7/NM10 board-support metadata.
//!
//! Runtime hardware init stays in the no_std driver crates.  This crate only
//! composes shared host-side metadata for mainboards built from the Pineview
//! northbridge and ICH7/NM10 southbridge: flash/CAR layout, stage flow, and
//! chipset driver defaults.  Mainboard crates still provide board policy such
//! as Super I/O wiring, clock-generator programming, GPIOs, HDA verbs, SMBIOS,
//! and payload choice.

use fstart_device_registry::{i2c_ck505, intel_pineview, DriverBinding, DriverInstance};
use fstart_driver_intel_ich7 as ich7;
use fstart_driver_intel_pineview as pineview;
use fstart_gpio_ich as gpio;
use fstart_hda as hda;
use fstart_types::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_types::{
    board_info_from_config, build_info_from_config, dev_security_config, hstr, hvec, io16,
    x86_linuxboot_payload, AcpiConfig, AcpiPlatform, BoardConfig, BoardInfo, BootMedium, BuildInfo,
    BusAddress, Capability, CarConfig, Compression, CorebootSmmCompat, DeviceConfig,
    DeviceTopology, MemoryMap, MemoryRegion, PayloadConfig, Platform, RegionKind, RunsFrom,
    SmbiosConfig, SmmConfig, SmmPlatform, StageConfig, StageLayout, TempRamBuffer,
};
use heapless::Vec as HVec;

pub use fstart_driver_intel_ich7::{LpcGenericIoDecode, SataConfig, SataMode, UsbConfig};

/// Shared Pineview + ICH7/NM10 platform metadata for a concrete mainboard.
#[derive(Debug, Clone)]
pub struct PineviewIch7Platform {
    board_name: &'static str,
    board_package: &'static str,
    payload: PayloadConfig,
    hda: Option<hda::HdaConfig>,
    gpio: Option<gpio::GpioConfig>,
    superio: Option<DriverInstance>,
    clock_generator: Option<i2c_ck505::I2cCk505Config>,
    smbios: Option<SmbiosConfig>,
    pcie_ports: [bool; 4],
    lpc_generic_io: HVec<LpcGenericIoDecode, 4>,
    gpe0_en: u32,
    sata: Option<SataConfig>,
    usb: Option<UsbConfig>,
}

/// ICH7/NM10 PCIe root-port selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PcieRootPort {
    Port0,
    Port1,
    Port2,
    Port3,
}

impl PcieRootPort {
    const fn index(self) -> usize {
        match self {
            Self::Port0 => 0,
            Self::Port1 => 1,
            Self::Port2 => 2,
            Self::Port3 => 3,
        }
    }
}

impl PineviewIch7Platform {
    #[must_use]
    pub fn new(board_name: &'static str, board_package: &'static str) -> Self {
        Self {
            board_name,
            board_package,
            payload: x86_linuxboot_payload(),
            hda: None,
            gpio: None,
            superio: None,
            clock_generator: None,
            smbios: None,
            pcie_ports: [false; 4],
            lpc_generic_io: HVec::new(),
            gpe0_en: 0,
            sata: None,
            usb: None,
        }
    }

    #[must_use]
    pub fn payload(mut self, payload: PayloadConfig) -> Self {
        self.payload = payload;
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
        self.gpio = Some(gpio);
        self
    }

    /// Set board-specific Super I/O driver config.
    #[must_use]
    pub fn superio(mut self, superio: DriverInstance) -> Self {
        self.superio = Some(superio);
        self
    }

    /// Set board-specific CK505/clock-generator config.
    #[must_use]
    pub fn clock_generator(mut self, clock_generator: i2c_ck505::I2cCk505Config) -> Self {
        self.clock_generator = Some(clock_generator);
        self
    }

    /// Set board-specific SMBIOS static metadata.
    #[must_use]
    pub fn smbios(mut self, smbios: SmbiosConfig) -> Self {
        self.smbios = Some(smbios);
        self
    }

    /// Set board policy for one ICH7/NM10 PCIe root port.
    ///
    /// The platform default keeps all ports disabled. A mainboard enables only
    /// the ports routed on that board.
    #[must_use]
    pub fn pcie_port(mut self, port: PcieRootPort, enabled: bool) -> Self {
        self.pcie_ports[port.index()] = enabled;
        self
    }

    /// Add one board-selected LPC generic I/O decode window.
    #[must_use]
    pub fn lpc_generic_io(mut self, decode: LpcGenericIoDecode) -> Self {
        self.lpc_generic_io
            .push(decode)
            .expect("ICH7 LPC generic I/O decode capacity");
        self
    }

    /// Set board-selected ACPI GPE0 enable bits.
    #[must_use]
    pub const fn gpe0_en(mut self, value: u32) -> Self {
        self.gpe0_en = value;
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

    #[must_use]
    pub fn board_config(&self) -> BoardConfig {
        BoardConfig {
            name: hstr(self.board_name),
            platform: Platform::X86_64,
            memory: pineview_ich7_memory(),
            devices: pineview_ich7_devices(self.pcie_ports),
            stages: pineview_ich7_stages(),
            security: dev_security_config("keys/dev-signing.pub"),
            payload: Some(self.payload.clone()),
            microcode: Some(pineview_microcode()),
            soc_image_format: Default::default(),
            full_flash_image: true,
            acpi: Some(AcpiConfig {
                print_hex: false,
                platform: AcpiPlatform::X86,
            }),
            smbios: self.smbios.clone(),
            smm: Some(SmmConfig {
                platform: SmmPlatform::PineviewIch7,
                entry_points: Some(4),
                stack_size: 0x400,
                coreboot: CorebootSmmCompat {
                    emit_header: true,
                    module_args: true,
                },
            }),
            boot_hart_id: 0,
        }
    }

    #[must_use]
    pub fn driver_bindings(&self) -> Vec<DriverBinding> {
        vec![
            DriverInstance::IntelPineview(pineview_config()).bind("northbridge"),
            DriverInstance::IntelIch7(ich7_config(
                self.hda.clone(),
                self.gpio.clone().unwrap_or_default(),
                self.lpc_generic_io.clone(),
                self.gpe0_en,
                self.sata,
                self.usb,
            ))
            .bind("southbridge"),
            self.superio
                .clone()
                .expect("mainboard must provide Super I/O config")
                .bind("superio"),
            DriverInstance::I2cCk505(
                self.clock_generator
                    .clone()
                    .expect("mainboard must provide CK505 config"),
            )
            .bind("ck505"),
        ]
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

fn pineview_ich7_memory() -> MemoryMap {
    let mut regions = HVec::new();
    for (name, base, size, kind) in [
        ("flash_ffs", 0xFF00_0000, 0x00F0_0000, RegionKind::Rom),
        ("flash_bootblock", 0xFFF0_0000, 0x0010_0000, RegionKind::Rom),
        ("ram", 0x0010_0000, 0x3FF0_0000, RegionKind::Ram),
    ] {
        regions
            .push(MemoryRegion {
                name: hstr(name),
                base,
                size,
                kind,
            })
            .expect("memory map capacity");
    }
    MemoryMap {
        regions,
        flash_layout: None,
        car: Some(CarConfig {
            base: 0xFEFC_0000,
            size: 0x8000,
        }),
    }
}

fn pineview_ich7_devices(pcie_ports: [bool; 4]) -> HVec<DeviceConfig, 32> {
    DeviceTopology::new()
        .root("northbridge")
        .root("southbridge")
        .pci_bridge("southbridge", "pcie0", 0x1c, 0, pcie_ports[0])
        .pci_bridge("southbridge", "pcie1", 0x1c, 1, pcie_ports[1])
        .pci_bridge("southbridge", "pcie2", 0x1c, 2, pcie_ports[2])
        .pci_bridge("southbridge", "pcie3", 0x1c, 3, pcie_ports[3])
        .lpc_bus("southbridge", "lpc", |lpc| {
            lpc.lpc_device("superio", io16(0x2e))
        })
        .smbus("southbridge", "smbus", |smbus| {
            smbus.smbus_device("ck505", 0x69)
        })
        .build()
}

fn pineview_ich7_stages() -> StageLayout {
    StageLayout::MultiStage(hvec([
        StageConfig {
            name: hstr("bootblock"),
            capabilities: pineview_bootblock_capabilities(),
            load_addr: 0,
            stack_size: 0x2000,
            heap_size: Some(0x100),
            runs_from: RunsFrom::Rom,
            compression: Compression::None,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
        StageConfig {
            name: hstr("ramstage"),
            capabilities: pineview_ramstage_capabilities(),
            load_addr: 0x0400_0000,
            stack_size: 0x8000,
            heap_size: Some(0x200000),
            runs_from: RunsFrom::Ram,
            compression: Compression::Lz4,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
    ]))
}

fn pineview_bootblock_capabilities() -> HVec<Capability, 16> {
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

fn pineview_ramstage_capabilities() -> HVec<Capability, 16> {
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
            max_cpus: 4,
            smm: true,
        },
        Capability::AcpiPrepare,
        Capability::SmBiosPrepare,
        Capability::PayloadLoad,
    ])
}

fn pineview_config() -> intel_pineview::IntelPineviewConfig {
    intel_pineview::IntelPineviewConfig {
        mchbar: 0xFED1_0000,
        dmibar: 0xFED1_8000,
        epbar: 0xFED1_9000,
        ecam_base: 0xE000_0000,
        igd: Some(pineview::IgdConfig {
            use_crt: true,
            use_lvds: false,
            spread_spectrum: false,
            vbt_file: Some(hstr("data.vbt")),
        }),
        spd_addresses: [0x50, 0x51, 0, 0],
        ck505_pre_raminit: true,
        acpi_name: Some(hstr("MCHC")),
    }
}

fn ich7_config(
    hda: Option<hda::HdaConfig>,
    gpio: gpio::GpioConfig,
    generic_io: HVec<LpcGenericIoDecode, 4>,
    gpe0_en: u32,
    sata: Option<SataConfig>,
    usb: Option<UsbConfig>,
) -> ich7::IntelIch7Config {
    ich7::IntelIch7Config {
        rcba: 0xFED1_C000,
        pirq_routing: [0x0b; 8],
        gpe0_en,
        lpc_decode: ich7::LpcDecodeConfig {
            fixed_io: ich7::LpcFixedIoDecode::default(),
            generic_io,
        },
        hda,
        sata,
        usb,
        pata: false,
        smbus_base: 0x0400,
        gpio,
        acpi_name: Some(hstr("LPCB")),
        c3_latency: 85,
        power_on_after_fail: 0,
    }
}

fn pineview_microcode() -> MicrocodeConfig {
    MicrocodeConfig::Intel(IntelMicrocodeConfig {
        files: hvec([
            hstr("../../intel-microcode/intel-ucode/06-1c-02"),
            hstr("../../intel-microcode/intel-ucode/06-1c-0a"),
        ]),
        early: true,
        mp: true,
    })
}
