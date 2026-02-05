//! Board configuration — the top-level type deserialized from board.ron.

use heapless::String as HString;
use serde::{Deserialize, Serialize};

use crate::device::DeviceConfig;
use crate::memory::MemoryMap;
use crate::security::SecurityConfig;
use crate::stage::StageLayout;

/// Top-level board configuration, deserialized from a board.ron file.
///
/// This is the single source of truth for a board's hardware description,
/// driver bindings, stage composition, and security settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardConfig {
    /// Human-readable board name (e.g., "qemu-riscv64")
    pub name: HString<64>,
    /// Platform identifier (e.g., "riscv64", "aarch64")
    pub platform: HString<32>,
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
    /// FDT source
    pub fdt: FdtSource,
}

/// What kind of payload to boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PayloadKind {
    /// Boot Linux kernel via platform boot protocol
    LinuxBoot,
    /// Interactive debug shell
    Shell,
    /// Custom ELF payload
    CustomElf,
}

/// Where the Flattened Device Tree comes from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FdtSource {
    /// Generate FDT automatically from this board.ron
    Generated,
    /// Use a separate DTS file (path relative to board directory)
    Override(HString<128>),
    /// Generate from RON but merge in DTS fragments
    GeneratedWithOverride(HString<128>),
}
