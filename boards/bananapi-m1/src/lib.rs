//! Rust board metadata for LeMaker BananaPi M1.

use fstart_device_registry::{DriverBinding, DriverInstance};
use fstart_driver_ns16550::{AccessMode, Ns16550Config};
use fstart_driver_sunxi_a20_dramc::SunxiA20DramcConfig;
use fstart_driver_sunxi_ccu::SunxiA20CcuConfig;
use fstart_driver_sunxi_mmc::SunxiMmcConfig;
use fstart_types::{
    board_info_from_config, build_info_from_config, hstr, hvec, BoardConfig, BoardInfo, BuildInfo,
    Capability, Compression, DeviceTopology, DigestAlgorithm, FdtSource, MemoryMap, MemoryRegion,
    PayloadConfig, PayloadKind, Platform, RegionKind, RunsFrom, SecurityConfig, SignatureAlgorithm,
    SocImageFormat, StageConfig, StageLayout,
};

pub const BOARD_NAME: &str = "bananapi-m1";
pub const BOARD_PACKAGE: &str = "fstart-board-bananapi-m1";
pub const PLATFORM: Platform = Platform::Armv7;

fn config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: memory_map([
            ("sram", 0x0000_0000, 0x8000, RegionKind::Ram),
            ("dram", 0x4000_0000, 0x4000_0000, RegionKind::Ram),
        ]),
        devices: DeviceTopology::new()
            .root("ccu0")
            .root("uart0")
            .root("dramc0")
            .root("mmc0")
            .build(),
        stages: sunxi_stages(0x0000_0000, 0x1000),
        security: security_config([DigestAlgorithm::Sha256]),
        payload: Some(sunxi_payload(
            "zImage",
            0x4200_0000,
            "sun7i-a20-bananapi.dtb",
            0x4300_0000,
            "earlycon console=ttyS0,115200",
        )),
        microcode: None,
        soc_image_format: SocImageFormat::AllwinnerEgon,
        full_flash_image: false,
        acpi: None,
        smbios: None,
        smm: None,
        build: Default::default(),
        boot_hart_id: 0,
    }
}

#[must_use]
pub fn driver_bindings() -> Vec<DriverBinding> {
    vec![
        DriverInstance::SunxiA20Ccu(SunxiA20CcuConfig {
            ccu_base: 0x01c2_0000,
            pio_base: 0x01c2_0800,
            uart_index: 0,
        })
        .bind("ccu0"),
        DriverInstance::Ns16550(Ns16550Config {
            regs: AccessMode::Mmio {
                base: 0x01c2_8000,
                reg_shift: 2,
                reg_width: 0,
            },
            clock_freq: 24_000_000,
            baud_rate: 115_200,
        })
        .bind("uart0"),
        DriverInstance::SunxiA20Dramc(SunxiA20DramcConfig {
            dramc_base: 0x01c0_1000,
            ccu_base: 0x01c2_0000,
            clock: 432,
            mbus_clock: 0,
            zq: 123,
            odt_en: false,
            cas: 6,
            tpr0: 0x3092_6692,
            tpr1: 0x1090,
            tpr2: 0x1a0c8,
            tpr3: 0,
            tpr4: 0,
            emr1: 4,
            emr2: 0,
            emr3: 0,
            dqs_gating_delay: 0,
            active_windowing: false,
        })
        .bind("dramc0"),
        DriverInstance::SunxiMmc(SunxiMmcConfig::Sun7iA20 {
            base_addr: 0x01c0_f000,
            ccu_base: 0x01c2_0000,
            pio_base: 0x01c2_0800,
            mmc_index: 0,
        })
        .bind("mmc0"),
    ]
}

#[must_use]
pub fn board_config() -> BoardConfig {
    config()
}

#[must_use]
pub fn board_info() -> BoardInfo {
    board_info_from_config(board_config())
}

#[must_use]
pub fn build_info() -> BuildInfo {
    let config = board_config();
    let bindings = driver_bindings();
    build_info_from_config(
        BOARD_NAME,
        BOARD_PACKAGE,
        &config,
        bindings.iter().filter_map(DriverBinding::driver_feature),
    )
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}

fn memory_map<const N: usize>(regions: [(&str, u64, u64, RegionKind); N]) -> MemoryMap {
    MemoryMap {
        regions: hvec(regions.map(|(name, base, size, kind)| MemoryRegion {
            name: hstr(name),
            base,
            size,
            kind,
        })),
        flash_layout: None,
        car: None,
    }
}

fn security_config<const N: usize>(digests: [DigestAlgorithm; N]) -> SecurityConfig {
    SecurityConfig {
        signing_algorithm: SignatureAlgorithm::Ed25519,
        pubkey_file: hstr("keys/dev-signing.pub"),
        required_digests: hvec(digests),
    }
}

fn firmware_boot_media() -> Capability {
    Capability::BootMedia(fstart_types::BootMedium::FirmwareImage {
        temp_ram_buffer: None,
    })
}

fn sunxi_stages(bootblock_load_addr: u64, bootblock_stack_size: u32) -> StageLayout {
    StageLayout::MultiStage(hvec([
        StageConfig {
            name: hstr("bootblock"),
            capabilities: hvec([
                Capability::ClockInit,
                Capability::ConsoleInit,
                Capability::DramInit,
                Capability::DriverInit,
                Capability::LoadNextStage {
                    next_stage: hstr("main"),
                },
            ]),
            load_addr: bootblock_load_addr,
            stack_size: bootblock_stack_size,
            heap_size: None,
            runs_from: RunsFrom::Ram,
            compression: Compression::None,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
        StageConfig {
            name: hstr("main"),
            capabilities: hvec([
                Capability::ConsoleInit,
                Capability::DriverInit,
                firmware_boot_media(),
                Capability::SigVerify,
                Capability::FdtPrepare,
                Capability::PayloadLoad,
            ]),
            load_addr: 0x4100_0000,
            stack_size: 0x10000,
            heap_size: None,
            runs_from: RunsFrom::Ram,
            compression: Compression::None,
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        },
    ]))
}

fn sunxi_payload(
    kernel: &str,
    kernel_load_addr: u64,
    dtb: &str,
    dtb_addr: u64,
    bootargs: &str,
) -> PayloadConfig {
    PayloadConfig {
        kind: PayloadKind::LinuxBoot,
        kernel_file: Some(hstr(kernel)),
        kernel_load_addr: Some(kernel_load_addr),
        fdt: FdtSource::Override(hstr(dtb)),
        dtb_addr: Some(dtb_addr),
        src_dtb_addr: None,
        bootargs: Some(hstr(bootargs)),
        print_x86_mtrrs: false,
        compression: Compression::Lz4,
        firmware: None,
        fit_file: None,
        fit_config: None,
        fit_parse: None,
    }
}
