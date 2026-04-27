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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageSize {
    /// 2 MiB pages — universally supported on all x86_64 CPUs.
    Size2MiB,
    /// 1 GiB pages — requires PDPE1GB (CPUID 0x80000001 EDX[26]).
    Size1GiB,
}

impl Default for PageSize {
    fn default() -> Self {
        Self::Size2MiB
    }
}

/// A capability is a composable unit of firmware functionality.
///
/// The RON file specifies which capabilities run in which stage(s).
/// At build time, the stage binary is generated to call these in order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Capability {
    /// Initialize the clock tree / PLL configuration.
    ///
    /// Must appear before `ConsoleInit` when the UART clock gate needs
    /// to be opened, and before `DramInit` when the DRAM PLL must be
    /// programmed. The referenced device implements `ClockController`.
    ///
    /// On Allwinner SoCs this programs PLL1 (CPU), PLL6 (peripherals),
    /// opens the UART clock gate, and muxes UART GPIO pins.
    ClockInit {
        /// Device name from the devices list (e.g., "ccu0")
        device: HString<32>,
    },
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
    /// Initialize DRAM (memory training) — stub, no device reference.
    ///
    /// Used on platforms where DRAM is already available or QEMU-style
    /// virtual boards. For real hardware, use `DramInit` instead.
    MemoryInit,
    /// Initialize DRAM via a specific memory controller driver.
    ///
    /// The referenced device implements `MemoryController`. Its `init()`
    /// method performs the full DRAM initialization sequence (PLL setup,
    /// PHY training, size detection). After this capability completes,
    /// the DRAM region declared in the memory map is usable.
    ///
    /// Replaces `MemoryInit` for boards with real DRAM controllers.
    DramInit {
        /// Device name from the devices list (e.g., "dramc0")
        device: HString<32>,
    },
    /// Early chipset initialization (x86 northbridge + southbridge).
    ///
    /// Calls `PciHost::early_init()` on the northbridge and
    /// `Southbridge::early_init()` on the southbridge. Used by the
    /// x86 romstage before `DramInit` — the NB must be programmed to
    /// expose its registers (MCHBAR, DMIBAR) and the SB must unlock
    /// LPC decode ranges / BIOS shadow before DRAM training.
    ///
    /// This is an x86-specific capability; other platforms use
    /// `DriverInit` + per-driver `Device::init()` instead.
    ///
    /// Should follow `ChipsetPreConsole` when the console device
    /// sits behind the southbridge (SuperIO on LPC).
    ChipsetInit {
        /// Northbridge device name (implements `PciHost`).
        northbridge: HString<32>,
        /// Southbridge device name (implements `Southbridge`).
        southbridge: HString<32>,
    },
    /// Pre-console chipset initialization — the absolute minimum to
    /// make the console device reachable.
    ///
    /// Calls `PciHost::pre_console_init()` (enable ECAM) and
    /// `Southbridge::pre_console_init()` (open LPC decode). Must
    /// appear before `ConsoleInit` when the console device sits on
    /// a bus that needs chipset decode windows opened (e.g., SuperIO
    /// behind ICH7 LPC).
    ///
    /// Full chipset init (`ChipsetInit`) follows after the console
    /// is available, so diagnostics are visible during the heavier
    /// BAR programming, GPIO, CIR, etc.
    ChipsetPreConsole {
        /// Northbridge device name (implements `PciHost`).
        northbridge: HString<32>,
        /// Southbridge device name (implements `Southbridge`).
        southbridge: HString<32>,
    },
    /// Initialize all logical CPUs (BSP + APs).
    ///
    /// Brings up application processors via INIT+SIPI, runs per-CPU
    /// MSR configuration (C-states, SpeedStep, thermals), optionally
    /// performs SMM relocation, and parks APs for later work dispatch.
    ///
    /// Must appear after `DramInit` — APs need stacks in DRAM.
    MpInit {
        /// CPU model identifier (selects which `CpuOps` to use).
        cpu_model: HString<32>,
        /// Expected logical CPU count (BSP + APs).
        num_cpus: u16,
        /// Enable SMM setup.  When true, the northbridge driver must
        /// implement `SmmOps`.  Default: false.
        #[serde(default)]
        smm: bool,
    },
    /// Enumerate and initialize all declared devices/drivers.
    DriverInit,
    /// Enumerate a PCI root bus, allocate BAR resources, and enable devices.
    ///
    /// The referenced device implements `PciRootBus`.  Its `init()` method
    /// walks the bus tree, sizes BARs, allocates from the MMIO/IO windows
    /// in the driver config, programs hardware, and enables memory decode.
    ///
    /// Must appear after `ConsoleInit` (so enumeration can be logged).
    PciInit {
        /// Device name from the devices list (e.g., "pci0")
        device: HString<32>,
    },
    /// Prepare a Flattened Device Tree for OS handoff.
    FdtPrepare,
    /// Load and jump to the payload (OS kernel, shell, etc.).
    PayloadLoad,
    /// Load the next stage from FFS into RAM and jump to it.
    StageLoad {
        /// Name of the next stage to load
        next_stage: HString<32>,
    },
    /// Device lockdown and security hardening — post-boot.
    ///
    /// Called after all payload/OS handoff preparation is complete but
    /// before the final jump. Used to:
    /// - Write-protect flash regions
    /// - Lock fuses / OTP
    /// - Disable debug ports (JTAG, UART if desired)
    /// - Revoke temporary credentials
    ///
    /// Devices that need lockdown should implement a `lockdown()` method
    /// (future trait extension). For now, this is a capability placeholder
    /// that logs its execution.
    LateDriverInit,
    /// Generate ACPI tables and write them to the configured address.
    ///
    /// Iterates devices with `AcpiDevice` impls and ACPI-only extra
    /// devices from the board RON to collect DSDT entries and standalone
    /// tables, then assembles the full table set (RSDP, XSDT, FADT,
    /// MADT, GTDT, device tables, DSDT). Requires a heap (`heap_size`
    /// must be set) and the board's `acpi` config section.
    AcpiPrepare,
    /// Generate SMBIOS tables and write them to the configured address.
    ///
    /// Writes SMBIOS 3.0 entry point and structure tables (Type 0/1/2/3/
    /// 4/16/17/19/32/127) from the board RON's `smbios` config section.
    /// Does not require a heap — writes directly to the target address.
    SmBiosPrepare,
    /// Load ACPI tables from an external provider device.
    ///
    /// Calls the referenced device's `AcpiTableProvider` implementation
    /// to load pre-built ACPI tables into memory. Used when the platform
    /// provides ACPI tables externally (e.g., QEMU's fw_cfg device)
    /// rather than generating them from the board RON.
    ///
    /// The loaded RSDP address is stored for the payload boot protocol
    /// (e.g., x86 zero page, or UEFI system table).
    ///
    /// For platforms that generate their own ACPI tables, use
    /// `AcpiPrepare` instead.
    AcpiLoad {
        /// Device name implementing `AcpiTableProvider` (e.g., "fw_cfg0")
        device: HString<32>,
    },
    /// Detect system memory layout at runtime.
    ///
    /// Calls the referenced device's `MemoryDetector` implementation
    /// to discover the memory map. On QEMU this reads e820 entries from
    /// the fw_cfg device. On real hardware, a future implementation
    /// might read SPD data and perform memory training.
    ///
    /// Results are stored for later use by the boot protocol (e.g.,
    /// x86 zero page e820 table, or FDT `/memory` node updates).
    MemoryDetect {
        /// Device name implementing `MemoryDetector` (e.g., "fw_cfg0")
        device: HString<32>,
    },
    /// Return to the BROM's FEL (USB recovery) mode.
    ///
    /// Restores the saved BROM state (SP, LR, CPSR, SCTLR, VBAR) from
    /// the `fel_stash` written by `save_boot_params` at reset, then
    /// returns via the saved LR. This function never returns.
    ///
    /// Useful for debugging: boot from SD card, run clock/UART init,
    /// then return to FEL so the host can poke registers via `sunxi-fel`.
    ///
    /// Currently supported on `armv7` (Allwinner sunxi) only.
    ReturnToFel,
    /// Load the next stage directly from a block device into its load
    /// address and jump to it.
    ///
    /// The stage's offset and size on the block device are read at
    /// runtime from the eGON header (patched by the FFS assembler).
    /// The absolute byte offset on the device is `base_offset` +
    /// `next_stage_offset` (from the header).
    ///
    /// When multiple devices are specified, the boot device is
    /// auto-detected at runtime via `fstart_soc_sunxi::boot_device()`
    /// (reads the BROM-written `boot_media` field from the eGON header).
    ///
    /// Used by bootblocks that are too small to contain the FFS reader
    /// (e.g., Allwinner A20 with 24K SRAM). The bootblock reads just
    /// the next stage binary and jumps — no FFS parsing, no intermediate
    /// DRAM buffer.
    LoadNextStage {
        /// Boot device candidates. When multiple are specified, the
        /// active boot device is auto-detected from the eGON header.
        devices: heapless::Vec<LoadDevice, 4>,
        /// Name of the next stage to jump to after loading.
        next_stage: HString<32>,
    },
}

/// A boot device candidate for `LoadNextStage`.
///
/// Each entry maps a block device name to its firmware image offset
/// on the medium. The codegen derives the eGON `boot_media` match
/// value from the device's driver type at build time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadDevice {
    /// Device name from the devices list (e.g., "mmc0", "spi0").
    pub name: HString<32>,
    /// Byte offset on the device where the firmware image starts.
    ///
    /// For SD card on sunxi: `0x2000` (sector 16, where BROM looks).
    /// For SPI NOR flash: `0` (image starts at the beginning of flash).
    pub base_offset: u64,
}

/// A boot device candidate for `BootMedium::AutoDevice`.
///
/// Similar to [`LoadDevice`] but also carries the FFS region size
/// needed by the `BootMedia` capability.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutoBootDevice {
    /// Device name from the devices list (e.g., "mmc0", "spi0").
    pub name: HString<32>,
    /// Byte offset on the device where the FFS image starts.
    pub offset: u64,
    /// Size of the FFS image region in bytes.
    pub size: u64,
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
        /// Optional RAM address to copy FFS data before accessing it.
        ///
        /// On x86_64 with code-model=large, FFS operations (postcard
        /// deserialization, LZ4 decompression) are significantly faster
        /// when operating on RAM rather than flash-mapped MMIO. When
        /// set, generated code copies `size` bytes from `base` to this
        /// address before constructing the `MemoryMapped` accessor.
        ///
        /// On platforms with true XIP (ARM, RISC-V), this should be
        /// `None` — the flash is accessed directly.
        #[serde(default)]
        ram_copy_addr: Option<u64>,
    },
    /// A named device that implements `BlockDevice`.
    ///
    /// The device must be listed in `devices` and initialized (via
    /// `ConsoleInit`, `DriverInit`, or similar) before the `BootMedia`
    /// capability appears. Generated code wraps the device in a
    /// `BlockDeviceMedia` adapter with the given base offset and size.
    ///
    /// ```ron
    /// BootMedia(Device(name: "mmc0", offset: 0x2000, size: 0x400000))
    /// ```
    Device {
        /// Device name from the devices list (e.g., "mmc0")
        name: HString<32>,
        /// Byte offset on the device where the FFS image starts.
        ///
        /// For Allwinner SD card boot, this is 8192 (sector 16) where
        /// the BROM loads from.
        offset: u64,
        /// Size of the FFS image region in bytes.
        size: u64,
    },
    /// Runtime boot device auto-detection.
    ///
    /// On sunxi, the BROM writes the boot source into the eGON header.
    /// The generated code reads `boot_device()` and selects the matching
    /// candidate device. A small `BlockDevice` dispatch enum is generated
    /// to unify the different device types behind a single variable.
    ///
    /// ```ron
    /// BootMedia(AutoDevice(devices: [
    ///     (name: "mmc0", offset: 0x2000, size: 0x800000),
    ///     (name: "spi0", offset: 0,      size: 0x400000),
    /// ]))
    /// ```
    AutoDevice {
        /// Boot device candidates — auto-selected at runtime.
        devices: heapless::Vec<AutoBootDevice, 4>,
    },
}
