//! Reusable Rust board defaults for Intel Pineview + ICH7/NM10 boards.
//!
//! Board crates provide identity and board-specific payload policy.  Fixed
//! chipset flow facts, the flash/CAR memory model, and the standard Pineview
//! northbridge plus ICH7 southbridge wiring live here so mainboards stay small.

use fstart_device_registry::{i2c_ck505, intel_pineview, DriverInstance};
use fstart_driver_intel_ich7 as ich7;
use fstart_driver_intel_pineview as pineview;
use fstart_driver_ite8721f as ite8721f;
use fstart_gpio_ich as gpio;
use fstart_hda as hda;
use fstart_types::board::{IntelMicrocodeConfig, MicrocodeConfig};
use fstart_types::smbios::{
    CacheAssociativity, CacheType, ChassisType, MemoryDeviceType, ProcessorFamily, SmbiosCache,
    SmbiosMemoryDevice, SmbiosProcessor,
};
use fstart_types::{
    AcpiConfig, AcpiPlatform, Board, BoardConfig, BoardInfo, BootMedium, Build, BuildInfo,
    BuildProfile, Capability, CarConfig, Compression, CorebootSmmCompat, CpuDriverKind,
    DeviceConfig, DigestAlgorithm, FdtSource, FlowProfile, ImageBuildInfo, MemoryMap, MemoryRegion,
    PayloadConfig, PayloadInputInfo, PayloadKind, Platform, RegionKind, RunsFrom, SecurityConfig,
    SignatureAlgorithm, SmbiosConfig, SmmConfig, SmmPlatform, StageBuildInfo, StageConfig,
    StageLayout, TempRamBuffer,
};
use heapless::{String as HString, Vec as HVec};

/// Common Pineview + ICH7 two-stage board defaults.
#[derive(Debug, Clone)]
pub struct PineviewIch7Board {
    board_name: &'static str,
    board_package: &'static str,
    payload: PayloadConfig,
    hda: Option<hda::HdaConfig>,
    gpio: gpio::GpioConfig,
}

impl PineviewIch7Board {
    #[must_use]
    pub fn new(board_name: &'static str, board_package: &'static str) -> Self {
        Self {
            board_name,
            board_package,
            payload: linuxboot_payload(),
            hda: None,
            gpio: Default::default(),
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
        self.gpio = gpio;
        self
    }

    #[must_use]
    pub fn board_config(&self) -> BoardConfig {
        BoardConfig {
            name: hstr(self.board_name),
            platform: Platform::X86_64,
            memory: pineview_ich7_memory(),
            devices: pineview_ich7_devices(),
            stages: pineview_ich7_stages(),
            security: security_config(),
            payload: Some(self.payload.clone()),
            microcode: Some(pineview_microcode()),
            soc_image_format: Default::default(),
            full_flash_image: true,
            acpi: Some(AcpiConfig {
                print_hex: false,
                platform: AcpiPlatform::X86,
            }),
            smbios: Some(d41s_smbios()),
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
    pub fn driver_instances(&self) -> Vec<DriverInstance> {
        vec![
            DriverInstance::IntelPineview(pineview_config()),
            DriverInstance::IntelIch7(ich7_config(self.hda.clone(), self.gpio.clone())),
            DriverInstance::Structural(Default::default()),
            DriverInstance::Structural(Default::default()),
            DriverInstance::Structural(Default::default()),
            DriverInstance::Structural(Default::default()),
            DriverInstance::Structural(Default::default()),
            DriverInstance::Ite8721f(superio_config()),
            DriverInstance::Structural(Default::default()),
            DriverInstance::I2cCk505(ck505_config()),
        ]
    }

    #[must_use]
    pub fn board_info(&self) -> BoardInfo {
        board_info_from_config(self.board_config())
    }

    #[must_use]
    pub fn build_info(&self) -> BuildInfo {
        build_info_from_parts(
            self.board_name,
            self.board_package,
            self.board_config(),
            self.driver_instances(),
        )
    }
}

#[must_use]
pub fn linuxboot_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr("bzImage")),
        kernel_load_addr: Some(0x0200_0000),
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: Some(hstr(
            "console=ttyS0,115200n8 earlycon=uart8250,io,0x3f8,115200n8 ignore_loglevel loglevel=8",
        )),
        print_x86_mtrrs: true,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}

#[must_use]
pub fn uefi_payload() -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::UefiPayload,
        kernel_file: None,
        kernel_load_addr: None,
        fdt: FdtSource::Platform,
        dtb_addr: None,
        src_dtb_addr: None,
        bootargs: None,
        print_x86_mtrrs: true,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
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

fn pineview_ich7_devices() -> HVec<DeviceConfig, 32> {
    let mut devices = HVec::new();
    for device in [
        dev("northbridge", None, None, true),
        dev("southbridge", None, None, true),
        dev(
            "pcie0",
            Some("southbridge"),
            Some(fstart_types::BusAddress::Pci(0x1c, 0)),
            true,
        ),
        dev(
            "pcie1",
            Some("southbridge"),
            Some(fstart_types::BusAddress::Pci(0x1c, 1)),
            true,
        ),
        dev(
            "pcie2",
            Some("southbridge"),
            Some(fstart_types::BusAddress::Pci(0x1c, 2)),
            false,
        ),
        dev(
            "pcie3",
            Some("southbridge"),
            Some(fstart_types::BusAddress::Pci(0x1c, 3)),
            false,
        ),
        dev("lpc", Some("southbridge"), None, true),
        dev(
            "superio",
            Some("lpc"),
            Some(fstart_types::BusAddress::Lpc(0x2e)),
            true,
        ),
        dev("smbus", Some("southbridge"), None, true),
        dev(
            "ck505",
            Some("smbus"),
            Some(fstart_types::BusAddress::I2c(0x69)),
            true,
        ),
    ] {
        devices.push(device).expect("device table capacity");
    }
    devices
}

fn pineview_ich7_stages() -> StageLayout {
    StageLayout::MultiStage(hvec([
        StageConfig {
            name: hstr("bootblock"),
            capabilities: hvec([
                Capability::PreConsoleInit {
                    devices: names(["northbridge", "southbridge"]),
                },
                Capability::ConsoleInit {
                    device: hstr("superio"),
                },
                Capability::EarlyInit {
                    devices: names(["northbridge", "southbridge"]),
                },
                Capability::DramInit {
                    device: hstr("northbridge"),
                },
                Capability::BootMedia(BootMedium::FirmwareImage {
                    provider: None,
                    temp_ram_buffer: None,
                }),
                Capability::StageLoad {
                    next_stage: hstr("ramstage"),
                },
            ]),
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
            capabilities: hvec([
                Capability::ConsoleInit {
                    device: hstr("superio"),
                },
                Capability::BootMedia(BootMedium::FirmwareImage {
                    provider: None,
                    temp_ram_buffer: Some(TempRamBuffer {
                        base: 0x0200_0000,
                        size: 0x0100_0000,
                    }),
                }),
                Capability::SigVerify,
                Capability::DriverInit,
                Capability::StageLocalInit {
                    devices: names(["northbridge"]),
                },
                Capability::MemoryDetect {
                    device: hstr("northbridge"),
                },
                Capability::PciInit {
                    device: hstr("northbridge"),
                },
                Capability::PostDramInit {
                    devices: names(["southbridge"]),
                },
                Capability::FinalizeInit {
                    devices: names(["southbridge"]),
                },
                Capability::MpInit {
                    cpu_drivers: hvec([CpuDriverKind::IntelPineview]),
                    max_cpus: 4,
                    smm: true,
                    smm_provider: None,
                },
                Capability::AcpiPrepare,
                Capability::SmBiosPrepare,
                Capability::PayloadLoad,
            ]),
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

fn ich7_config(hda: Option<hda::HdaConfig>, gpio: gpio::GpioConfig) -> ich7::IntelIch7Config {
    let mut generic_io = HVec::new();
    generic_io
        .push(ich7::LpcGenericIoDecode {
            base: 0x0a00,
            size: 0x0100,
        })
        .expect("generic I/O decode capacity");
    ich7::IntelIch7Config {
        rcba: 0xFED1_C000,
        pirq_routing: [0x0b; 8],
        gpe0_en: 0x441,
        lpc_decode: ich7::LpcDecodeConfig {
            fixed_io: ich7::LpcFixedIoDecode::default(),
            generic_io,
        },
        hda,
        sata: Some(ich7::SataConfig {
            mode: ich7::SataMode::Ahci,
            ports: 0x3,
        }),
        usb: Some(ich7::UsbConfig {
            ehci: true,
            uhci: [true, true, true, true],
        }),
        pata: false,
        smbus_base: 0x0400,
        gpio,
        acpi_name: Some(hstr("LPCB")),
        c3_latency: 85,
        power_on_after_fail: 0,
    }
}

fn superio_config() -> ite8721f::Ite8721fConfig {
    ite8721f::Ite8721fConfig {
        com1: Some(ite8721f::ComPortConfig {
            io_base: 0x3f8,
            irq: 4,
            baud_rate: 115200,
        }),
        com2: Some(ite8721f::ComPortConfig {
            io_base: 0x2f8,
            irq: 3,
            baud_rate: 115200,
        }),
        parallel: Some(ite8721f::ParallelConfig {
            io_base: 0x378,
            irq: 7,
        }),
        env_controller: Some(ite8721f::EcConfig {
            io_base: 0xa10,
            io_ext: 0xa00,
        }),
        keyboard: Some(ite8721f::KbcConfig {
            io_base: 0x60,
            io_ext: 0x64,
            irq: 1,
        }),
        mouse: Some(ite8721f::MouseConfig { irq: 12 }),
        cir: Some(ite8721f::CirConfig {
            io_base: 0x3e0,
            irq: 10,
        }),
        gpio: None,
        acpi_name: Some(hstr("SIO0")),
        console_port: Some(hstr("com1")),
    }
}

fn ck505_config() -> i2c_ck505::I2cCk505Config {
    i2c_ck505::I2cCk505Config {
        mask: hvec([0x00, 0x80, 0xff, 0xff, 0xff]),
        regs: hvec([0x00, 0x80, 0xfe, 0xff, 0xfc]),
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

fn security_config() -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        pubkey_file: hstr("keys/dev-signing.pub"),
        required_digests: hvec([DigestAlgorithm::Sha256]),
    }
}

fn d41s_smbios() -> SmbiosConfig {
    let mut caches = HVec::new();
    for cache in [
        SmbiosCache {
            designation: hstr("L1 Data Cache"),
            level: 1,
            size_kb: 24,
            associativity: CacheAssociativity::Way8,
            cache_type: CacheType::Data,
        },
        SmbiosCache {
            designation: hstr("L1 Instruction Cache"),
            level: 1,
            size_kb: 32,
            associativity: CacheAssociativity::Way8,
            cache_type: CacheType::Instruction,
        },
        SmbiosCache {
            designation: hstr("L2 Cache"),
            level: 2,
            size_kb: 1024,
            associativity: CacheAssociativity::Way8,
            cache_type: CacheType::Unified,
        },
    ] {
        caches.push(cache).expect("cache table capacity");
    }

    let mut processors = HVec::new();
    processors
        .push(SmbiosProcessor {
            socket: hstr("FCBGA559"),
            manufacturer: hstr("Intel"),
            processor_family: ProcessorFamily::X86_64,
            max_speed_mhz: Some(1660),
            core_count: Some(2),
            thread_count: Some(4),
            caches,
        })
        .expect("processor table capacity");

    SmbiosConfig {
        bios_vendor: hstr("fstart"),
        bios_version: hstr("0.1.0"),
        bios_release_date: hstr("04/15/2026"),
        system_manufacturer: hstr("Foxconn"),
        system_product: hstr("D41S"),
        system_version: hstr("1.0"),
        system_serial: HString::new(),
        baseboard_manufacturer: hstr("Foxconn"),
        baseboard_product: hstr("D41S"),
        chassis_type: ChassisType::Desktop,
        chassis_manufacturer: hstr("Foxconn"),
        processors,
        memory_devices: hvec([
            SmbiosMemoryDevice {
                locator: hstr("DIMM0"),
                size_mb: Some(1024),
                speed_mhz: Some(800),
                memory_type: Some(MemoryDeviceType::Ddr2),
            },
            SmbiosMemoryDevice {
                locator: hstr("DIMM1"),
                size_mb: Some(1024),
                speed_mhz: Some(800),
                memory_type: Some(MemoryDeviceType::Ddr2),
            },
        ]),
    }
}

fn board_info_from_config(config: BoardConfig) -> BoardInfo {
    let mut board = Board::new(config.name.as_str())
        .platform(config.platform)
        .memory(config.memory);
    for device in config.devices {
        board = board.device(device);
    }
    if let Some(payload) = config.payload {
        board = board.payload(payload);
    }
    board.build()
}

fn build_info_from_parts(
    board_name: &str,
    board_package: &str,
    config: BoardConfig,
    drivers: Vec<DriverInstance>,
) -> BuildInfo {
    let mut build = Build::new(board_name)
        .board_package(board_package)
        .target(Platform::X86_64.target_triple())
        .profile(BuildProfile::Dev)
        .flow_profile(FlowProfile::LinuxBoot)
        .image(ImageBuildInfo {
            full_flash_image: config.full_flash_image,
            soc_image_format: config.soc_image_format,
        })
        .stage(StageBuildInfo::new("bootblock", 0xFFFF_FFFF))
        .stage(StageBuildInfo::new("ramstage", 0x0400_0000))
        .feature(Platform::X86_64.as_str());
    for driver in drivers {
        if let Some(feature) = driver.driver_feature() {
            build = build.feature(feature);
        }
    }
    if let Some(payload) = &config.payload {
        if let Some(kernel) = &payload.kernel_file {
            build = build.payload_input(PayloadInputInfo::new("kernel", kernel.as_str()));
        }
        if let Some(firmware) = &payload.firmware {
            build = build.payload_input(PayloadInputInfo::new("firmware", firmware.file.as_str()));
        }
    }
    build.build()
}

fn dev(
    name: &str,
    parent: Option<&str>,
    bus: Option<fstart_types::BusAddress>,
    enabled: bool,
) -> DeviceConfig {
    DeviceConfig {
        name: hstr(name),
        parent: parent.map(hstr),
        bus,
        enabled,
    }
}

fn names<const N: usize>(items: [&str; N]) -> HVec<HString<32>, 8> {
    let mut out = HVec::new();
    for item in items {
        out.push(hstr(item)).expect("names capacity");
    }
    out
}

fn hvec<T, const N: usize, const C: usize>(items: [T; N]) -> HVec<T, C> {
    let mut out = HVec::new();
    for item in items {
        out.push(item).ok().expect("heapless vec capacity");
    }
    out
}

fn hstr<const N: usize>(value: &str) -> HString<N> {
    HString::try_from(value).expect("string exceeds heapless capacity")
}
