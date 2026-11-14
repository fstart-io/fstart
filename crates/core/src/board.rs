//! Board configuration — the top-level Rust board metadata type.

use heapless::String as HString;

use crate::memory::MemoryMap;
use crate::security::SecurityConfig;
use crate::stage::StageLayout;

use crate::platform::{FirmwareKind, Platform};

/// Host-side projection of a resolved board assembly.
///
/// Board crates never construct this: the platform's host plan fills it from
/// typed board facts and shared geometry, and the assembler and linker read
/// it. Firmware links no part of it.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "host", derive(serde::Serialize))]
pub struct BoardConfig {
    /// Human-readable board name (e.g., "qemu-riscv64")
    pub name: HString<64>,
    /// Target platform / ISA
    pub platform: Platform,
    /// Memory map: ROM, RAM, MMIO regions
    pub memory: MemoryMap,
    /// Stage composition: monolithic or multi-stage
    pub stages: StageLayout,
    /// Security: signing algorithm, pubkey, digest requirements
    pub security: SecurityConfig,
    /// Optional payload configuration
    pub payload: Option<PayloadConfig>,
    /// Optional CPU microcode updates to package into FFS.
    ///
    /// x86 stages can use this blob twice: very early BSP assembly reads the
    /// anchor-patched location before Rust starts, and MP init reloads the
    /// matching patch on BSP/APs during CPU bring-up.
    #[cfg_attr(feature = "host", serde(default))]
    pub microcode: Option<MicrocodeConfig>,

    /// Board-shipped files packaged verbatim into the FFS as verified
    /// `FileType::Data` assets.
    ///
    /// Each name is both the board-directory file to read and the name the
    /// firmware looks the asset up by at runtime
    /// (`services::ffs_context::read_verified_asset`), so a board's runtime
    /// request and its packaging cannot disagree.
    #[cfg_attr(feature = "host", serde(default))]
    pub data_assets: heapless::Vec<HString<64>, 8>,
    /// Files outside the board directory (vendor blobs such as a video
    /// BIOS) packaged as verified `FileType::Data` assets under `name`.
    #[cfg_attr(
        feature = "host",
        serde(default, skip_serializing_if = "heapless::Vec::is_empty")
    )]
    pub file_assets: heapless::Vec<FileAsset, 8>,

    /// Also emit a complete flash image padded/laid out to the firmware window size.
    ///
    /// The normal `.ffs` output is a firmware filesystem blob. Hardware flash
    /// programmers usually need the entire NOR image, with XIP stages overlaid
    /// at their linked physical flash addresses and unused bytes filled with
    /// `0xff`. When true, `fbuild assemble` writes
    /// `target/ffs/<board>-<size>m.pflash` in addition to `<board>.ffs`.
    #[cfg_attr(feature = "host", serde(default))]
    pub full_flash_image: bool,

    /// Host build policy that belongs to the board/platform, not to driver metadata.
    #[cfg_attr(feature = "host", serde(default))]
    pub build: BoardBuildPolicy,

    /// ACPI table generation configuration.
    ///
    /// Used by fixed platform flows that emit ACPI tables. Contains the target
    /// address for tables, platform parameters (MADT, GTDT, FADT), and
    /// declarations for ACPI-only devices (hardware without fstart driver crates).
    ///
    /// Per-driver ACPI fields (e.g., `acpi_name`, `acpi_gsiv`) live in each
    /// driver's own `Config` struct, not here.
    #[cfg_attr(feature = "host", serde(default))]
    pub acpi: Option<crate::acpi::AcpiConfig>,

    /// SMBIOS table generation configuration.
    ///
    /// Used by fixed platform flows that emit SMBIOS tables. Contains system
    /// identity strings, processor descriptions, and memory device declarations
    /// for SMBIOS Type 0/1/2/3/4/16/17/19.
    #[cfg_attr(feature = "host", serde(default))]
    pub smbios: Option<crate::smbios::SmbiosConfig>,

    /// System Management Mode handler/image configuration.
    ///
    /// Required when an x86 stage build enables SMM. The normal stage embeds
    /// the separately built SMM image and asks the platform adapter selected
    /// here to copy its precompiled PIC entry stubs into SMRAM.
    #[cfg_attr(feature = "host", serde(default))]
    pub smm: Option<crate::smm::SmmConfig>,

    /// Boot hart ID for multi-hart platforms.
    ///
    /// On multi-hart SoCs (e.g., SiFive FU740 with 5 harts), the boot ROM
    /// starts all harts simultaneously. Only the hart matching this ID
    /// continues past `_start`; all others enter a WFI loop waiting for
    /// an IPI from the SBI firmware.
    ///
    /// Defaults to `0` when absent, which is correct for single-hart
    /// platforms and for QEMU `virt` (which starts only hart 0 by default).
    /// Set to `1` for the SiFive FU740 (hart 0 is the S7 monitor core
    /// without S-mode; hart 1 is the first U74 application core).
    #[cfg_attr(feature = "host", serde(default))]
    pub boot_hart_id: u32,
}

/// Requested UEFI payload build capabilities, not immutable hardware facts or
/// a variable-persistence policy. Effective Cargo features remain additive:
/// a full consumer unifies a basic request upward to the full runtime bundle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UefiBuildProfile {
    /// Existing non-graphical CrabEFI capabilities.
    #[default]
    Full,
    /// Full CrabEFI capabilities plus the graphical boot manager and setup UI.
    Ui,
    /// Normal runtime services without TPM, xHCI, UEFI Secure Boot or SPI tools.
    Basic,
}

/// A platform-owned mutable region before the read-only FFS container.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "host", serde(deny_unknown_fields))]
pub struct MutableFlashRegion {
    pub name: HString<32>,
    pub size: u32,
    /// Erase/alignment requirement in bytes, supplied by the flash owner.
    pub alignment: u32,
}

pub type MutableFlashRegions = heapless::Vec<MutableFlashRegion, { crate::ffs::MAX_IMAGE_REGIONS }>;

/// Validate a contiguous mutable prefix at a physical flash offset.
/// No cache format, chipset, region names or erase geometry are inferred.
pub fn mutable_prefix_size(regions: &[MutableFlashRegion], base: u32) -> Result<u32, &'static str> {
    let mut end = base;
    for (index, region) in regions.iter().enumerate() {
        if region.name.is_empty()
            || region.name.eq_ignore_ascii_case("ro")
            || regions[..index]
                .iter()
                .any(|other| other.name.eq_ignore_ascii_case(&region.name))
        {
            return Err("mutable flash region names must be unique and not reserved");
        }
        if !region.alignment.is_power_of_two()
            || region.size == 0
            || !end.is_multiple_of(region.alignment)
            || region.size % region.alignment != 0
        {
            return Err("mutable flash region violates its declared alignment");
        }
        end = end
            .checked_add(region.size)
            .ok_or("mutable flash prefix overflows")?;
    }
    Ok(end - base)
}

/// Board/platform-owned host build policy.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "host", serde(deny_unknown_fields))]
pub struct BoardBuildPolicy {
    /// Named, aligned mutable regions, disjoint from the RO/reset container.
    #[cfg_attr(
        feature = "host",
        serde(default, skip_serializing_if = "heapless::Vec::is_empty")
    )]
    pub mutable_regions: MutableFlashRegions,
    /// How host tooling should map the runtime firmware filesystem.
    #[cfg_attr(feature = "host", serde(default))]
    pub firmware_image: FirmwareImagePolicy,
    /// Complete flash aperture for composite XIP firmware images.
    ///
    /// This is distinct from `firmware_image` when the stage and FFS
    /// occupy separate windows, as on QEMU virt.
    #[cfg_attr(feature = "host", serde(default))]
    pub flash_image: Option<FirmwareImagePolicy>,
    /// Optional stage feature for the board/platform PCI root implementation.
    ///
    /// When absent, `PciInit` uses the generic ECAM implementation.
    #[cfg_attr(feature = "host", serde(default))]
    pub pci_root_feature: Option<HString<32>>,
    /// Optional stage feature for board/platform CPU initialization.
    #[cfg_attr(feature = "host", serde(default))]
    pub cpu_feature: Option<HString<32>>,
}

/// Board/platform-owned firmware-image mapping policy for host tooling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "host", serde(deny_unknown_fields))]
pub enum FirmwareImagePolicy {
    /// Derive from board memory metadata, such as an explicit flash layout or a
    /// contiguous ROM window.
    #[default]
    Auto,
    /// This board does not expose a host-side firmware-image mapping.
    None,
    /// Firmware image is a fixed memory-mapped window.
    MemoryMapped { cpu_base: u64, size: u64 },
}

impl FirmwareImagePolicy {
    /// Construct a fixed memory-mapped firmware-image policy.
    pub const fn memory_mapped(cpu_base: u64, size: u64) -> Self {
        Self::MemoryMapped { cpu_base, size }
    }
}

/// A file packaged into the verified FFS under `name`, the name the
/// firmware looks it up by.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "host", serde(deny_unknown_fields))]
pub struct FileAsset {
    pub name: HString<64>,
    /// Relative to the workspace root unless absolute.
    pub path: HString<256>,
}

/// CPU microcode packaging configuration.
// Build-time board configuration, not hot data; no_std has no Box.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
pub enum MicrocodeConfig {
    /// Intel concatenated microcode update files.
    Intel(IntelMicrocodeConfig),
    /// AMD Family 17h+ flat microcode patches.
    Amd(AmdMicrocodeConfig),
}

/// AMD flat patches to concatenate into the FFS file `cpu_microcode.bin`,
/// which the CPU driver loads during MP init
/// (`fstart_amdfw_format::microcode`).
#[derive(Debug, Clone)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "host", serde(deny_unknown_fields))]
pub struct AmdMicrocodeConfig {
    /// Patch files, resolved relative to the workspace root unless absolute.
    #[cfg_attr(feature = "host", serde(default))]
    pub files: heapless::Vec<HString<128>, 16>,
    /// Vendor flash images (or halves of one) whose BIOS directory
    /// microcode entries (type 0x66) are packed too, found the way the PSP
    /// finds that directory. A patch two of them carry is packed once.
    #[cfg_attr(feature = "host", serde(default))]
    pub vendor_images: heapless::Vec<AmdVendorMicrocode, 4>,
}

/// The microcode of a vendor AMD flash image.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "host", serde(deny_unknown_fields))]
pub struct AmdVendorMicrocode {
    /// The vendor flash image, relative to the workspace root unless
    /// absolute.
    pub image: HString<256>,
    /// The EFS field the SoC's PSP takes the BIOS directory from.
    pub bios_directory: fstart_amdfw_format::efs::BiosDirectorySlot,
    /// Flash offset of the half that carries this directory (0, or 16 MiB
    /// for the upper half of a dual image).
    #[cfg_attr(feature = "host", serde(default))]
    pub origin: u32,
}

/// Intel microcode files to concatenate into `cpu_microcode_blob.bin`.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "host", serde(deny_unknown_fields))]
pub struct IntelMicrocodeConfig {
    /// Source files, resolved relative to the board directory unless absolute.
    pub files: heapless::Vec<HString<128>, 16>,
    /// Patch the blob location into the FFS anchor for pre-Rust BSP update.
    #[cfg_attr(feature = "host", serde(default = "default_true"))]
    pub early: bool,
    /// Use the blob again during MP init for BSP/AP updates.
    #[cfg_attr(feature = "host", serde(default = "default_true"))]
    pub mp: bool,
}

const fn default_true() -> bool {
    true
}

/// Payload configuration: what to boot after firmware init.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "host", serde(deny_unknown_fields))]
pub struct PayloadConfig {
    /// Kind of payload
    pub kind: PayloadKind,
    /// Filename of kernel in FFS (e.g., "vmlinux")
    pub kernel_file: Option<HString<64>>,
    /// Load address for the kernel in RAM
    pub kernel_load_addr: Option<u64>,
    /// Physical address of the x86 Linux boot-parameter zero page.
    #[cfg_attr(
        feature = "host",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub x86_zero_page_addr: Option<u64>,
    /// FDT source.
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
    /// When `None`, platform payload code uses its default DTB address.
    #[cfg_attr(feature = "host", serde(default))]
    pub src_dtb_addr: Option<u64>,
    /// Kernel command line (set in /chosen/bootargs)
    pub bootargs: Option<HString<256>>,
    /// Print BSP x86 MTRR/control-register state immediately before Linux handoff.
    #[cfg_attr(feature = "host", serde(default))]
    pub print_x86_mtrrs: bool,
    /// Compression to use for raw kernel payload segments stored in FFS.
    #[cfg_attr(feature = "host", serde(default = "default_payload_compression"))]
    pub compression: crate::ffs::Compression,
    /// SBI / ATF firmware blob configuration
    pub firmware: Option<FirmwareConfig>,
    /// Path to a FIT (.itb) image file (relative to board directory).
    ///
    /// Used when `kind` is `FitImage`. The FIT bundles kernel, ramdisk,
    /// and optionally FDT into a single DTB-format blob.
    #[cfg_attr(feature = "host", serde(default))]
    pub fit_file: Option<HString<128>>,
    /// FIT configuration name to use (e.g., "conf-1").
    ///
    /// When `None`, the FIT's `default` configuration is used.
    #[cfg_attr(feature = "host", serde(default))]
    pub fit_config: Option<HString<64>>,
    /// Whether to parse the FIT at buildtime or runtime.
    ///
    /// Defaults to `None` (same as `Buildtime` when `kind` is `FitImage`).
    #[cfg_attr(feature = "host", serde(default))]
    pub fit_parse: Option<FitParseMode>,
}

fn default_payload_compression() -> crate::ffs::Compression {
    crate::ffs::Compression::Lz4
}

/// What kind of payload to boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
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
    /// Bare ELF: its loadable segments loaded with BSS cleared, entered at
    /// `e_entry` with no arguments.
    CustomElf,
    /// UEFI payload via CrabEFI library.
    ///
    /// Links the CrabEFI library and calls `init_platform()` with a
    /// `PlatformConfig` constructed from fstart's drivers.  fstart
    /// handles hardware init (console, PCI BAR allocation, etc.) and
    /// injects drivers as trait objects.
    UefiPayload,
    /// coreboot payload: an ELF entered in 32-bit protected mode with the
    /// coreboot table, plus named files handed over in RAM through it.
    Coreboot,
}

/// When to parse a FIT image.
///
/// Both modes use the same parser code (`fstart-boot::fit`); this controls
/// whether extraction happens at buildtime (fbuild) or runtime (firmware).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
pub enum FitParseMode {
    /// Parse at buildtime: fbuild reads the .itb, extracts kernel/ramdisk/fdt
    /// components, and embeds them as separate FFS entries. The firmware
    /// loads them as individual blobs (like LinuxBoot).
    Buildtime,
    /// Parse at runtime: the whole .itb is embedded in FFS as a single entry.
    /// The firmware parses the FIT in-place (zero-copy on memory-mapped flash)
    /// and copies each component to its load address.
    Runtime,
}

/// Where the Flattened Device Tree comes from.
///
/// A platform always supplies the blob; only where it comes from is a build
/// choice. There is no generator to name, so the variants that promised one
/// are gone rather than left to fail at assembly.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
pub enum FdtSource {
    /// Use the DTB passed by QEMU/firmware at reset
    Platform,
    /// Use a separate DTS file (path relative to board directory)
    Override(HString<128>),
}

/// Configuration for the SBI firmware (RISC-V) or ATF BL31 (AArch64)
/// binary that is loaded before jumping to the OS.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "host", serde(deny_unknown_fields))]
pub struct FirmwareConfig {
    /// Kind of firmware
    pub kind: FirmwareKind,
    /// Path to the firmware binary (relative to board directory)
    pub file: HString<128>,
    /// Address in RAM where the firmware blob is loaded
    pub load_addr: u64,
}

#[cfg(test)]
mod mutable_region_tests {
    use super::*;

    fn region(name: &str, size: u32, alignment: u32) -> MutableFlashRegion {
        MutableFlashRegion {
            name: crate::hstr(name),
            size,
            alignment,
        }
    }

    #[test]
    fn accepts_platform_owned_names_sizes_and_erase_geometry() {
        let regions = [
            region("calibration", 0x1800, 0x800),
            region("variables", 0x2000, 0x800),
        ];
        assert_eq!(mutable_prefix_size(&regions, 0x4000), Ok(0x3800));
        let intel = [
            region("mrc-cache", 0x20000, 0x10000),
            region("console", 0x40000, 0x10000),
        ];
        assert_eq!(mutable_prefix_size(&intel, 0x500000), Ok(0x60000));
        assert_eq!(mutable_prefix_size(&[], u32::MAX), Ok(0));
    }

    #[test]
    fn rejects_misalignment_empty_regions_and_overflow() {
        for bad in [
            region("x", 0, 4096),
            region("x", 4096, 0),
            region("x", 4096, 3),
            region("x", 4097, 4096),
        ] {
            assert!(mutable_prefix_size(&[bad], 0).is_err());
        }
        assert!(mutable_prefix_size(&[region("x", 4096, 4096)], 1).is_err());
        assert!(mutable_prefix_size(&[region("x", 8192, 4096)], 0xfffff000).is_err());
        // A following region must also start on its own declared boundary.
        assert!(
            mutable_prefix_size(&[region("a", 2048, 2048), region("b", 4096, 4096)], 0).is_err()
        );
    }

    #[test]
    fn rejects_duplicate_empty_and_reserved_region_names() {
        for name in ["", "ro", "RO"] {
            assert!(mutable_prefix_size(&[region(name, 4096, 4096)], 0).is_err());
        }
        assert!(
            mutable_prefix_size(&[region("data", 4096, 4096), region("DATA", 4096, 4096)], 0)
                .is_err()
        );
    }
}
