//! Board-owned stage build metadata for fixed handwritten flows.
//!
//! These structs describe how the build tool links and packages each stage.
//! Runtime ordering lives in platform flow code, not in this metadata.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// How stages are laid out for this board.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // no_std: can't Box heapless containers
pub enum StageLayout {
    /// Single binary firmware image.
    Monolithic(MonolithicConfig),
    /// Multiple stage binaries packed into the firmware image.
    MultiStage(heapless::Vec<StageConfig, 8>),
}

/// Configuration for a monolithic (single-stage) build.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonolithicConfig {
    /// Build-time features and packaging inputs this stage needs.
    #[serde(default)]
    pub build: StageBuildConfig,
    /// Load/run address
    pub load_addr: u64,
    /// Stack size in bytes
    pub stack_size: u32,
    /// Heap size in bytes for the bump allocator.
    ///
    /// Required when the stage links code that needs dynamic
    /// allocation. The build emits a sized static
    /// (`_FSTART_HEAP`) and a size constant (`_FSTART_HEAP_SIZE`) that
    /// `fstart-alloc` references via `extern "C"` at link time.
    #[serde(default)]
    pub heap_size: Option<u32>,
    /// Explicit address for data/BSS/stack in RAM (XIP builds only).
    ///
    /// When code runs from ROM (XIP), writable data sections must be
    /// placed in RAM. By default they go at the start of the first RAM
    /// region. Set this to reserve the start of RAM for other uses
    /// (e.g., QEMU places the DTB at the base of RAM on AArch64).
    #[serde(default)]
    pub data_addr: Option<u64>,
    /// Separate low-memory region for page tables and IDT (x86_64).
    ///
    /// On x86_64, page tables and the IDT must be in writable RAM at
    /// addresses below the main data region. When set, the linker
    /// creates a separate LOW memory region at `(addr, size)` for
    /// `.pagetables` and `.idt` sections. When unset, these sections
    /// go in the normal RAM region (or in ROM for platforms that
    /// support ROM-resident page tables).
    #[serde(default)]
    pub page_table_addr: Option<(u64, u64)>,
    /// x86_64 identity-map page size (default: 2 MiB).
    ///
    /// Controls page table layout and total memory used. Use `Size1GiB`
    /// only on CPUs known to support PDPE1GB.
    #[serde(default)]
    pub page_size: PageSize,
}

/// Configuration for one stage in a multi-stage build.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StageConfig {
    /// Stage name (e.g., "bootblock", "main")
    pub name: HString<32>,
    /// Build-time features and packaging inputs this stage needs.
    #[serde(default)]
    pub build: StageBuildConfig,
    /// Where this stage is loaded in memory.
    ///
    /// For the first x86_64 ROM/XIP stage this may be omitted/zero; tooling
    /// then selects the topmost ROM region and the linker/FFS place the
    /// bootblock top-aligned in that region.
    #[serde(default)]
    pub load_addr: u64,
    /// Stack size in bytes
    pub stack_size: u32,
    /// Heap size in bytes for the bump allocator.
    ///
    /// Same semantics as [`MonolithicConfig::heap_size`].
    #[serde(default)]
    pub heap_size: Option<u32>,
    /// Where this stage executes from
    pub runs_from: RunsFrom,
    /// Compression to use when this stage is packaged into FFS.
    ///
    /// The first stage is always stored uncompressed because it executes
    /// directly and contains the patchable FFS anchor. Later stages may use
    /// `Lz4` when loaded by the stage-load helper; stages copied directly by
    /// platform boot-source code must remain `None` because that path copies raw
    /// bytes and jumps.
    #[serde(default = "default_stage_compression")]
    pub compression: crate::ffs::Compression,
    /// Explicit address for data/BSS/stack in RAM (XIP stages only).
    ///
    /// Same semantics as [`MonolithicConfig::data_addr`]: when the stage
    /// runs from ROM (XIP), this offsets writable sections away from the
    /// default RAM base. Needed on AArch64 where QEMU places the DTB at
    /// the base of RAM.
    #[serde(default)]
    pub data_addr: Option<u64>,
    /// Separate low-memory region for page tables and IDT (x86_64).
    ///
    /// Same semantics as [`MonolithicConfig::page_table_addr`].
    #[serde(default)]
    pub page_table_addr: Option<(u64, u64)>,
    /// x86_64 identity-map page size (default: 2 MiB).
    ///
    /// Same semantics as [`MonolithicConfig::page_size`].
    #[serde(default)]
    pub page_size: PageSize,
}

/// Build-time feature and packaging requirements for a stage.
///
/// This is deliberately not an execution plan. Fixed platform flows decide
/// runtime ordering; this only tells the build tool which optional code and
/// image entries a board-owned stage needs.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StageBuildConfig {
    /// Whether this stage reads the firmware image/FFS from the board's
    /// configured firmware window.
    #[serde(default)]
    pub firmware_image: Option<FirmwareImageConfig>,
    /// Whether this stage verifies the FFS manifest signature.
    #[serde(default)]
    pub verify_firmware: bool,
    /// Name of the next stage this stage packages/loads from FFS, if any.
    #[serde(default)]
    pub load_next_stage: Option<HString<32>>,
    /// Whether this stage hands off to the selected payload.
    #[serde(default)]
    pub payload: bool,
    /// Whether this stage prepares an FDT.
    #[serde(default)]
    pub fdt: bool,
    /// Whether this stage scans/initializes PCI.
    #[serde(default)]
    pub pci: bool,
    /// Whether this stage emits or loads ACPI tables.
    #[serde(default)]
    pub acpi: bool,
    /// Whether this stage emits SMBIOS tables.
    #[serde(default)]
    pub smbios: bool,
    /// Whether this stage initializes APs/SMM.
    #[serde(default)]
    pub mp: Option<MpBuildConfig>,
}

/// Firmware-image access needed by a stage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FirmwareImageConfig {
    /// Optional temporary RAM arena available to FFS/payload code.
    #[serde(default)]
    pub temp_ram_buffer: Option<TempRamBuffer>,
}

/// MP/SMM build metadata for a stage.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MpBuildConfig {
    /// Maximum logical CPU count to attempt (BSP + APs).
    pub max_cpus: u16,
    /// Enable SMM setup.
    #[serde(default)]
    pub smm: bool,
}

/// Where a stage executes from.
///
/// Describes the **code** location only. The writable landing spot
/// (`.data`, `.bss`, stack) is chosen by the linker from whatever
/// writable memory is actually available at the time the stage runs:
///
/// - On ARM / RISC-V / x86 post-DRAM: the first RAM region.
/// - On x86 pre-DRAM stages (bootblock, romstage) where the board
///   declares [`crate::memory::MemoryMap::car`]: the CAR region.
///
/// The choice is automatic — an XIP stage (`Rom`) on a board with
/// `memory.car` declared lands its writable sections in CAR; on a
/// board without, it lands them in RAM. No separate `Car` variant is
/// required; the distinction is fully expressed by the presence of
/// `memory.car` in the memory map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunsFrom {
    /// Execute in place from ROM (XIP).
    ///
    /// Code runs from flash-mapped ROM. Writable sections go into
    /// `memory.car` if declared (x86 pre-DRAM pattern), else the
    /// first RAM region (everything else).
    Rom,
    /// Execute from RAM after being loaded.
    ///
    /// Load address must lie inside a RAM region. Code is copied from
    /// flash into RAM by a prior stage and jumped to.
    Ram,
}

fn default_stage_compression() -> crate::ffs::Compression {
    crate::ffs::Compression::None
}

/// Resolve the effective load address for a multi-stage entry.
///
/// A `load_addr` of zero is treated as "auto" only for the first x86_64
/// ROM/XIP stage. In that case the address is chosen inside the topmost ROM
/// region; x86 linker and FFS assembly then top-align the actual bootblock
/// bytes within that region.
pub fn effective_stage_load_addr(
    config: &crate::board::BoardConfig,
    stage_index: usize,
    stage: &StageConfig,
) -> u64 {
    if stage.load_addr != 0 {
        return stage.load_addr;
    }

    if config.platform == crate::board::Platform::X86_64
        && stage_index == 0
        && stage.runs_from == RunsFrom::Rom
    {
        return config
            .memory
            .regions
            .iter()
            .filter(|region| region.kind == crate::memory::RegionKind::Rom && region.size > 0)
            .max_by_key(|region| region.base.saturating_add(region.size))
            .map(|region| region.base.saturating_add(region.size).saturating_sub(1))
            .unwrap_or(0);
    }

    stage.load_addr
}

/// x86_64 identity-map page size for page table construction.
///
/// Controls the page table depth and total size:
/// - `Size2MiB`: 4-level (PML4 + PDPT + PD tables), 6 pages for 4 GiB.
///   Compatible with all x86_64 CPUs.
/// - `Size1GiB`: 3-level (PML4 + PDPT with PS=1), 2 pages for 512 GiB.
///   Requires CPUID 0x80000001 EDX bit 26 (PDPE1GB).  Not supported on
///   early Atom, some Xeon E3, and Goldmont-class CPUs.
///
/// Real boards should set this based on the target CPU's known capabilities.
/// QEMU boards can use `Size1GiB` since QEMU always supports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PageSize {
    /// 2 MiB pages — universally supported on all x86_64 CPUs.
    #[default]
    Size2MiB,
    /// 1 GiB pages — requires PDPE1GB (CPUID 0x80000001 EDX[26]).
    Size1GiB,
}

/// Temporary RAM scratch buffer for firmware-image/FFS operations.
///
/// This is a bounded arena, not a firmware-image mapping. Runtime code may use
/// it for short-lived copies needed by non-memory-mapped providers or parsers
/// that need contiguous input. The buffer must be valid writable RAM and must
/// not overlap any source image bytes being copied into it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TempRamBuffer {
    /// Base address of the scratch arena in RAM.
    pub base: u64,
    /// Size of the scratch arena in bytes.
    pub size: u64,
}
