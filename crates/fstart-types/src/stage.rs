//! Stage composition types for transitional capability/profile metadata.
//!
//! Rust-authored boards provide these values directly. The long-term fixed
//! stage flow consumes coarse build profiles rather than board-authored device
//! routing or generated flow code.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

/// How stages are laid out for this board.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)] // no_std: can't Box heapless containers
pub enum StageLayout {
    /// Single binary with all capabilities linked in.
    Monolithic(MonolithicConfig),
    /// Multiple stage binaries, each with a subset of capabilities.
    /// Each stage is generated separately and packed into the FFS.
    MultiStage(heapless::Vec<StageConfig, 8>),
}

/// Configuration for a monolithic (single-stage) build.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonolithicConfig {
    /// Ordered list of capabilities to execute
    pub capabilities: heapless::Vec<Capability, 16>,
    /// Load/run address
    pub load_addr: u64,
    /// Stack size in bytes
    pub stack_size: u32,
    /// Heap size in bytes for the bump allocator.
    ///
    /// Required when the stage uses capabilities that need dynamic
    /// allocation (e.g., `FdtPrepare`). Codegen emits a sized static
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
    /// Ordered list of capabilities for this stage
    pub capabilities: heapless::Vec<Capability, 16>,
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
    /// `Lz4` when loaded via `StageLoad`; stages loaded by `LoadNextStage`
    /// must remain `None` because that path copies raw bytes and jumps.
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
    /// flash into RAM by a prior stage's `StageLoad` capability and
    /// jumped to.
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

/// A semantic firmware capability/flow marker.
///
/// Transitional code still uses this enum to select stage feature families and
/// to emit legacy `StagePlan` facts, but board metadata must not route runtime
/// work by device name. Device participation is selected from service metadata
/// and platform/driver policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum Capability {
    /// Initialize the clock tree / PLL configuration.
    ClockInit,
    /// Initialize an early console for debug output.
    ConsoleInit,
    /// Declare the boot medium for FFS operations.
    BootMedia(BootMedium),
    /// Verify the firmware filesystem manifest signature.
    SigVerify,
    /// Mark DRAM as available without a memory-controller service.
    MemoryInit,
    /// Initialize DRAM through the stage-selected memory controller service.
    DramInit,
    /// Initialize all logical CPUs (BSP + APs).
    MpInit {
        /// Maximum logical CPU count to attempt (BSP + APs).
        max_cpus: u16,
        /// Enable SMM setup. When true, exactly one compiled runtime device must
        /// provide `SmmOps`; selecting among multiple providers belongs in typed
        /// board/build policy, not a capability string.
        #[serde(default)]
        smm: bool,
    },
    /// Enumerate and initialize all declared devices/drivers.
    DriverInit,
    /// Enumerate a PCI root bus, allocate BAR resources, and enable devices.
    PciInit,
    /// Prepare a Flattened Device Tree for OS handoff.
    FdtPrepare,
    /// Load and jump to the payload (OS kernel, shell, etc.).
    PayloadLoad,
    /// Load the next stage from FFS into RAM and jump to it.
    StageLoad {
        /// Name of the next stage to load.
        next_stage: HString<32>,
    },
    /// Generate ACPI tables and write them to the configured address.
    AcpiPrepare,
    /// Generate SMBIOS tables and write them to the configured address.
    SmBiosPrepare,
    /// Load ACPI tables from the selected external provider service.
    AcpiLoad,
    /// Detect system memory layout through the selected memory detector service.
    MemoryDetect,
    /// Return to the BROM's FEL (USB recovery) mode.
    ReturnToFel,
    /// Load the next stage directly from platform boot-source metadata.
    LoadNextStage {
        /// Name of the next stage to jump to after loading.
        next_stage: HString<32>,
    },
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

/// Boot medium — how the firmware image is accessed at runtime.
///
/// Declared via the `BootMedia(...)` capability for transitional stage metadata.
/// Firmware image mapping and boot-source candidate tables are supplied by
/// Rust platform/chipset/provider code, not by board-local device strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum BootMedium {
    /// Firmware image exposed by the selected provider service or by platform
    /// firmware-image/boot-source metadata. The capability does not name a
    /// provider device.
    FirmwareImage {
        /// Optional scratch RAM arena available to FFS/payload code.
        #[serde(default)]
        temp_ram_buffer: Option<TempRamBuffer>,
    },
}
