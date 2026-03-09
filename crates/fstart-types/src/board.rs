//! Board configuration — the top-level type deserialized from board.ron.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

use crate::device::DeviceConfig;
use crate::memory::MemoryMap;
use crate::security::SecurityConfig;
use crate::stage::StageLayout;

/// Target platform / ISA.
///
/// This enum is the single source of truth for platform identity.
/// Adding a new platform variant automatically produces compiler errors
/// at every `match` site that needs updating — no stringly-typed
/// matching required.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Platform {
    /// RISC-V 64-bit (riscv64gc-unknown-none-elf)
    Riscv64,
    /// AArch64 / ARMv8-A (aarch64-unknown-none)
    Aarch64,
    /// ARMv7-A (armv7a-none-eabi)
    Armv7,
}

impl Platform {
    /// Rust target triple for cross-compilation.
    pub fn target_triple(&self) -> &'static str {
        match self {
            Platform::Riscv64 => "riscv64gc-unknown-none-elf",
            Platform::Aarch64 => "aarch64-unknown-none",
            Platform::Armv7 => "armv7a-none-eabi",
        }
    }

    /// Linker `OUTPUT_ARCH(...)` name.
    pub fn linker_arch(&self) -> &'static str {
        match self {
            Platform::Riscv64 => "riscv",
            Platform::Aarch64 => "aarch64",
            Platform::Armv7 => "arm",
        }
    }

    /// Short string identifier (used for cargo feature names, log messages).
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::Riscv64 => "riscv64",
            Platform::Aarch64 => "aarch64",
            Platform::Armv7 => "armv7",
        }
    }
}

impl core::fmt::Display for Platform {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Top-level board configuration, deserialized from a board.ron file.
///
/// This is the single source of truth for a board's hardware description,
/// driver bindings, stage composition, and security settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    /// Human-readable board name (e.g., "qemu-riscv64")
    pub name: HString<64>,
    /// Target platform / ISA
    pub platform: Platform,
    /// Memory map: ROM, RAM, MMIO regions
    pub memory: MemoryMap,
    /// Device declarations with driver and service bindings
    pub devices: heapless::Vec<DeviceConfig, 32>,
    /// Stage composition: monolithic or multi-stage
    pub stages: StageLayout,
    /// Security: signing algorithm, pubkey, digest requirements
    pub security: SecurityConfig,
    /// Build mode: rigid (compile-time) or flexible (runtime)
    pub mode: BuildMode,
    /// Optional payload configuration
    pub payload: Option<PayloadConfig>,
    /// SoC-specific binary image format required by the boot ROM.
    ///
    /// Each SoC family has its own boot ROM that expects a particular
    /// binary layout on the boot medium (SD card, SPI flash, eMMC).
    /// When set, codegen emits the required header/structure in dedicated
    /// linker sections and xtask patches length/checksum fields post-build.
    ///
    /// This is intentionally NOT a generic abstraction — each variant
    /// carries the exact semantics of one SoC family's boot ROM.
    #[serde(default)]
    pub soc_image_format: SocImageFormat,
}

/// SoC-specific binary image format required by the boot ROM.
///
/// Different SoC families have incompatible boot ROM requirements:
/// Allwinner uses eGON.BT0, TI uses MLO/CH, Mediatek uses BRLYT, etc.
/// Each variant here describes exactly one SoC family's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SocImageFormat {
    /// No SoC-specific image format — binary starts with `.text.entry`.
    #[default]
    None,
    /// Allwinner eGON.BT0 header (sun4i/sun5i/sun7i/sun8i/sun50i/sun20i).
    ///
    /// Required by the boot ROM on ALL Allwinner SoCs
    /// (A10/A13/A20/A31/A64/H3/H5/H6/D1/…).
    ///
    /// The BROM scans the boot medium for the `"eGON.BT0"` magic and
    /// validates the checksum, then loads `length` bytes into SRAM.
    /// Format:
    /// - Offset 0x00: ARM/RISC-V branch instruction (jumps over header)
    /// - Offset 0x04: `"eGON.BT0"` magic (8 bytes)
    /// - Offset 0x0C: checksum (word-add over entire image)
    /// - Offset 0x10: total image length (512-byte aligned)
    ///
    /// The length field is set to a placeholder at compile time. Xtask
    /// computes the actual binary size (rounded up to 512-byte alignment),
    /// pads the binary, and patches both the length and checksum fields
    /// post-build — just like U-Boot's `mksunxiboot` tool.
    ///
    /// See oreboot's D1 implementation for the reference Rust approach.
    AllwinnerEgon,
}

/// Build mode determines how the firmware is compiled and how drivers are bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuildMode {
    /// Single board, compile-time driver binding, maximum dead code elimination.
    Rigid,
    /// Multiple boards possible, runtime driver binding via enum or trait objects.
    Flexible,
}

/// Payload configuration: what to boot after firmware init.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PayloadConfig {
    /// Kind of payload
    pub kind: PayloadKind,
    /// Filename of kernel in FFS (e.g., "vmlinux")
    pub kernel_file: Option<HString<64>>,
    /// Load address for the kernel in RAM
    pub kernel_load_addr: Option<u64>,
    /// FDT source
    pub fdt: FdtSource,
    /// Target address for the patched DTB in RAM
    pub dtb_addr: Option<u64>,
    /// Explicit source address for the platform-provided DTB.
    ///
    /// On RISC-V, QEMU passes DTB in `a1` and the platform crate saves it,
    /// so this field is unnecessary. On AArch64 firmware boot (`-bios`),
    /// QEMU zeroes all registers and places the DTB at the base of RAM —
    /// `x0` is 0, not a DTB pointer. Set this to the known DTB address
    /// (e.g., `0x40000000` for QEMU AArch64 virt).
    ///
    /// When `None`, codegen uses `boot_dtb_addr()` from the platform crate.
    #[serde(default)]
    pub src_dtb_addr: Option<u64>,
    /// Kernel command line (set in /chosen/bootargs)
    pub bootargs: Option<HString<256>>,
    /// SBI / ATF firmware blob configuration
    pub firmware: Option<FirmwareConfig>,
    /// Path to a FIT (.itb) image file (relative to board directory).
    ///
    /// Used when `kind` is `FitImage`. The FIT bundles kernel, ramdisk,
    /// and optionally FDT into a single DTB-format blob.
    #[serde(default)]
    pub fit_file: Option<HString<128>>,
    /// FIT configuration name to use (e.g., "conf-1").
    ///
    /// When `None`, the FIT's `default` configuration is used.
    #[serde(default)]
    pub fit_config: Option<HString<64>>,
    /// Whether to parse the FIT at buildtime or runtime.
    ///
    /// Defaults to `None` (same as `Buildtime` when `kind` is `FitImage`).
    #[serde(default)]
    pub fit_parse: Option<FitParseMode>,
}

/// What kind of payload to boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PayloadKind {
    /// Boot Linux kernel via platform boot protocol
    LinuxBoot,
    /// Boot from a FIT (Flattened Image Tree) image.
    ///
    /// FIT images bundle kernel, ramdisk, FDT, and firmware into a single
    /// DTB-format blob with hash integrity and configuration selection.
    /// See `fit_parse` for whether the FIT is parsed at buildtime or runtime.
    FitImage,
    /// Interactive debug shell
    Shell,
    /// Custom ELF payload
    CustomElf,
}

/// When to parse a FIT image.
///
/// Both modes use the same parser code (`fstart-fit`); this controls
/// whether extraction happens at buildtime (xtask) or runtime (firmware).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FitParseMode {
    /// Parse at buildtime: xtask reads the .itb, extracts kernel/ramdisk/fdt
    /// components, and embeds them as separate FFS entries. The firmware
    /// loads them as individual blobs (like LinuxBoot).
    Buildtime,
    /// Parse at runtime: the whole .itb is embedded in FFS as a single entry.
    /// The firmware parses the FIT in-place (zero-copy on memory-mapped flash)
    /// and copies each component to its load address.
    Runtime,
}

/// Where the Flattened Device Tree comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FdtSource {
    /// Use the DTB passed by QEMU/firmware at reset
    Platform,
    /// Generate FDT automatically from this board.ron
    Generated,
    /// Use a separate DTS file (path relative to board directory)
    Override(HString<128>),
    /// Generate from RON but merge in DTS fragments
    GeneratedWithOverride(HString<128>),
}

/// Configuration for the SBI firmware (RISC-V) or ATF BL31 (AArch64)
/// binary that is loaded before jumping to the OS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirmwareConfig {
    /// Kind of firmware
    pub kind: FirmwareKind,
    /// Path to the firmware binary (relative to board directory)
    pub file: HString<128>,
    /// Address in RAM where the firmware blob is loaded
    pub load_addr: u64,
}

/// Kind of runtime firmware loaded before the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FirmwareKind {
    /// OpenSBI or RustSBI using the fw_dynamic protocol.
    /// Entry: a0=hartid, a1=dtb, a2=&fw_dynamic_info.
    OpenSbi,
    /// ARM Trusted Firmware BL31.
    /// Entry: x0=&bl_params.
    ArmTrustedFirmware,
}
