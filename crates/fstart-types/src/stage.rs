//! Stage composition types — capability-based stage definition.
//!
//! Stages are not hand-written code. They are generated from the board RON
//! file: an entry point that calls the declared capabilities in sequence.

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
}

/// Configuration for one stage in a multi-stage build.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageConfig {
    /// Stage name (e.g., "bootblock", "main")
    pub name: HString<32>,
    /// Ordered list of capabilities for this stage
    pub capabilities: heapless::Vec<Capability, 16>,
    /// Where this stage is loaded in memory
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
    /// Explicit address for data/BSS/stack in RAM (XIP stages only).
    ///
    /// Same semantics as [`MonolithicConfig::data_addr`]: when the stage
    /// runs from ROM (XIP), this offsets writable sections away from the
    /// default RAM base. Needed on AArch64 where QEMU places the DTB at
    /// the base of RAM.
    #[serde(default)]
    pub data_addr: Option<u64>,
}

/// Where a stage executes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunsFrom {
    /// Execute in place from ROM (XIP)
    Rom,
    /// Execute from RAM after being loaded
    Ram,
}

/// A capability is a composable unit of firmware functionality.
///
/// The RON file specifies which capabilities run in which stage(s).
/// At build time, the stage binary is generated to call these in order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Capability {
    /// Initialize an early console for debug output.
    ConsoleInit {
        /// Device name from the devices list (e.g., "uart0")
        device: HString<32>,
    },
    /// Declare the boot medium for FFS operations.
    ///
    /// Must appear before any FFS-consuming capability (`SigVerify`,
    /// `StageLoad`, `PayloadLoad`). Generates the `boot_media` variable
    /// used by those capabilities.
    BootMedia(BootMedium),
    /// Verify the firmware filesystem manifest signature.
    SigVerify,
    /// Initialize DRAM (memory training).
    MemoryInit,
    /// Enumerate and initialize all declared devices/drivers.
    DriverInit,
    /// Prepare a Flattened Device Tree for OS handoff.
    FdtPrepare,
    /// Load and jump to the payload (OS kernel, shell, etc.).
    PayloadLoad,
    /// Load the next stage from FFS into RAM and jump to it.
    StageLoad {
        /// Name of the next stage to load
        next_stage: HString<32>,
    },
}

/// Boot medium — how the firmware image is accessed at runtime.
///
/// Specified via the `BootMedia(...)` capability in the board RON.
/// Determines which `BootMedia` trait implementation is constructed
/// in the generated stage code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BootMedium {
    /// Memory-mapped flash.
    ///
    /// The SoC maps the flash chip into the CPU address space starting
    /// at `base`. This is the SoC-specific raw-flash-to-CPU address
    /// translation. Generated code constructs a `MemoryMapped` from
    /// these values — zero-cost, no vtable.
    ///
    /// ```ron
    /// BootMedia(MemoryMapped(base: 0x20000000, size: 0x02000000))
    /// ```
    MemoryMapped {
        /// CPU-visible base address where the flash is mapped.
        base: u64,
        /// Size of the mapped flash region in bytes.
        size: u64,
    },
    /// A named device that implements `BlockDevice`.
    ///
    /// The device must be listed in `devices` and initialized (via
    /// `ConsoleInit`, `DriverInit`, or similar) before the `BootMedia`
    /// capability appears. Generated code wraps the device in a
    /// `BlockDeviceMedia` adapter.
    Device {
        /// Device name from the devices list (e.g., "spi_flash0")
        name: HString<32>,
    },
}
