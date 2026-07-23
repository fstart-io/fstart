//! HiFive Unmatched static policy and host build metadata.

#[cfg(feature = "host")]
use fstart_core::{
    dev_security_config, hstr, hvec, BoardBuildPolicy, BoardConfig, Compression, FdtSource,
    FirmwareConfig, FirmwareImageConfig, FirmwareImagePolicy, FirmwareKind, MemoryMap,
    MemoryRegion, MonolithicConfig, PayloadConfig, PayloadKind, RegionKind, StageBuildConfig,
    StageLayout,
};
#[cfg(feature = "stage")]
use fstart_core::mmio32;
use fstart_core::Platform;
#[cfg(feature = "stage")]
use fstart_driver_uart::sifive::SifiveUartConfig;

pub const BOARD_NAME: &str = "sifive-unmatched";
pub const BOARD_PACKAGE: &str = "fstart-board-sifive-unmatched";
pub const PLATFORM: Platform = Platform::Riscv64;

const LIM_BASE: u64 = 0x0800_0000;
const LIM_SIZE: u64 = 0x0020_0000;
const SPI_XIP_BASE: u64 = 0x2000_0000;
const SPI_XIP_SIZE: u64 = 0x0200_0000;
const DRAM_BASE: u64 = 0x8000_0000;
const DRAM_SIZE: u64 = 0x4_0000_0000;
const DTB_ADDR: u64 = 0x8f00_0000;
const KERNEL_ADDR: u64 = 0x8400_0000;
const FIRMWARE_ADDR: u64 = 0x8300_0000;
const BOOTARGS: &str = "console=ttySIF0 earlycon=sbi";

/// DDR register profile supported by the fixed FU740 flow.
#[cfg(feature = "stage")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Fu740DdrProfile {
    /// HiFive Unmatched DIMM/controller values from SiFive's reference setup.
    HiFiveUnmatched,
}

/// Static FU740 DDR policy.
#[cfg(feature = "stage")]
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fu740DdrConfig {
    pub profile: Fu740DdrProfile,
    pub dram_size: u64,
}

#[cfg(feature = "stage")]
impl Fu740DdrConfig {
    #[must_use]
    pub const fn hifive_unmatched(dram_size: u64) -> Self {
        Self {
            profile: Fu740DdrProfile::HiFiveUnmatched,
            dram_size,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.dram_size < 0x4000 || self.dram_size > u64::MAX - DRAM_BASE {
            panic!("FU740 DRAM window is invalid");
        }
        self
    }
}

/// Static FU740 PRCI policy.
#[cfg(feature = "stage")]
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fu740PrciConfig {
    pub reference_clock_hz: u32,
}

#[cfg(feature = "stage")]
impl Fu740PrciConfig {
    #[must_use]
    pub const fn hifive_unmatched() -> Self {
        Self {
            reference_clock_hz: 26_000_000,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        if self.reference_clock_hz == 0 {
            panic!("FU740 reference clock must not be zero");
        }
        self
    }
}

/// Closed FU740 policy consumed by the fixed early flow.
#[cfg(feature = "stage")]
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fu740Config {
    pub prci: Fu740PrciConfig,
    pub ddr: Fu740DdrConfig,
    pub firmware_base: u64,
    pub firmware_size: u64,
    pub dtb_addr: u64,
    pub kernel_addr: u64,
    pub firmware_addr: u64,
    pub boot_hart_id: u64,
    pub bootargs: &'static str,
}

#[cfg(feature = "stage")]
impl Fu740Config {
    #[must_use]
    pub const fn new(
        ddr: Fu740DdrConfig,
        firmware_base: u64,
        firmware_size: u64,
        dtb_addr: u64,
        kernel_addr: u64,
        firmware_addr: u64,
        bootargs: &'static str,
    ) -> Self {
        Self {
            prci: Fu740PrciConfig::hifive_unmatched(),
            ddr,
            firmware_base,
            firmware_size,
            dtb_addr,
            kernel_addr,
            firmware_addr,
            boot_hart_id: 1,
            bootargs,
        }
    }

    #[must_use]
    pub const fn build(self) -> Self {
        let _ = self.prci.build();
        let _ = self.ddr.build();
        if self.firmware_size == 0 || self.firmware_base.checked_add(self.firmware_size).is_none() {
            panic!("FU740 firmware window is invalid");
        }
        if self.boot_hart_id != 1 {
            panic!("FU740 must boot on U74 hart 1");
        }
        if self.kernel_addr < DRAM_BASE
            || self.firmware_addr < DRAM_BASE
            || self.dtb_addr < DRAM_BASE
        {
            panic!("FU740 payload addresses must be in DRAM");
        }
        self
    }
}

/// Unmatched's DDR profile and SPI FFS window, retained in ROM.
#[cfg(feature = "stage")]
pub static HIFIVE_UNMATCHED: Fu740Config = Fu740Config::new(
    Fu740DdrConfig::hifive_unmatched(DRAM_SIZE),
    SPI_XIP_BASE,
    SPI_XIP_SIZE,
    DTB_ADDR,
    KERNEL_ADDR,
    FIRMWARE_ADDR,
    BOOTARGS,
)
.build();

/// Unmatched UART wiring and precomputed divisor, retained in ROM.
#[cfg(feature = "stage")]
pub static HIFIVE_UNMATCHED_UART: SifiveUartConfig =
    SifiveUartConfig::new(mmio32(0x1001_0000), 130_000_000, 115_200);

#[cfg(feature = "host")]
#[must_use]
pub fn board_config() -> BoardConfig {
    BoardConfig {
        name: hstr(BOARD_NAME),
        platform: PLATFORM,
        memory: MemoryMap {
            regions: hvec([
                MemoryRegion {
                    name: hstr("lim"),
                    base: LIM_BASE,
                    size: LIM_SIZE,
                    kind: RegionKind::Ram,
                },
                MemoryRegion {
                    name: hstr("spi-xip"),
                    base: SPI_XIP_BASE,
                    size: SPI_XIP_SIZE,
                    kind: RegionKind::Rom,
                },
                MemoryRegion {
                    name: hstr("dram"),
                    base: DRAM_BASE,
                    size: DRAM_SIZE,
                    kind: RegionKind::Ram,
                },
            ]),
            flash_layout: None,
            car: None,
        },
        stages: StageLayout::Monolithic(MonolithicConfig {
            build: StageBuildConfig {
                firmware_image: Some(FirmwareImageConfig {
                    temp_ram_buffer: None,
                }),
                verify_firmware: true,
                payload: true,
                fdt: true,
                ..StageBuildConfig::default()
            },
            load_addr: LIM_BASE,
            stack_size: 0x4000,
            heap_size: Some(0x4000),
            data_addr: None,
            page_table_addr: None,
            page_size: Default::default(),
        }),
        security: dev_security_config("keys/dev-signing.pub"),
        payload: Some(PayloadConfig {
            kind: PayloadKind::LinuxBoot,
            kernel_file: Some(hstr("Image-riscv64")),
            kernel_load_addr: Some(KERNEL_ADDR),
            fdt: FdtSource::Override(hstr("unmatched.dtb")),
            dtb_addr: Some(DTB_ADDR),
            src_dtb_addr: None,
            bootargs: Some(hstr(BOOTARGS)),
            print_x86_mtrrs: false,
            compression: Compression::Lz4,
            firmware: Some(FirmwareConfig {
                kind: FirmwareKind::OpenSbi,
                file: hstr("fw_dynamic.bin"),
                load_addr: FIRMWARE_ADDR,
            }),
            fit_file: None,
            fit_config: None,
            fit_parse: None,
        }),
        microcode: None,
        soc_image_format: Default::default(),
        full_flash_image: false,
        build: BoardBuildPolicy {
            firmware_image: FirmwareImagePolicy::memory_mapped(SPI_XIP_BASE, SPI_XIP_SIZE),
            ..BoardBuildPolicy::default()
        },
        acpi: None,
        smbios: None,
        smm: None,
        boot_hart_id: 1,
    }
}

#[must_use]
pub const fn board_name() -> &'static str {
    BOARD_NAME
}
