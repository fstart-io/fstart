//! Target platform / ISA identity.

use serde::{Deserialize, Serialize};

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
    /// x86-64 / AMD64 (x86_64-unknown-none)
    X86_64,
    /// 32-bit x86 in protected mode, for CPUs without long mode
    /// (i686-unknown-none, a JSON target in `tools/fbuild/targets/`)
    X86,
}

impl Platform {
    /// Rust target triple for cross-compilation.
    pub fn target_triple(&self) -> &'static str {
        match self {
            Platform::Riscv64 => "riscv64gc-unknown-none-elf",
            Platform::Aarch64 => "aarch64-unknown-none",
            Platform::Armv7 => "armv7a-none-eabi",
            Platform::X86_64 => "x86_64-unknown-none",
            Platform::X86 => "i686-unknown-none",
        }
    }

    /// Linker `OUTPUT_ARCH(...)` name.
    pub fn linker_arch(&self) -> &'static str {
        match self {
            Platform::Riscv64 => "riscv",
            Platform::Aarch64 => "aarch64",
            Platform::Armv7 => "arm",
            Platform::X86_64 => "i386:x86-64",
            Platform::X86 => "i386",
        }
    }

    /// Short string identifier (used for cargo feature names, log messages).
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::Riscv64 => "riscv64",
            Platform::Aarch64 => "aarch64",
            Platform::Armv7 => "armv7",
            Platform::X86_64 => "x86_64",
            Platform::X86 => "x86",
        }
    }

    /// Either x86 flavour: the same firmware flow, entered in long mode or
    /// staying in 32-bit protected mode.
    pub const fn is_x86(&self) -> bool {
        matches!(self, Platform::X86_64 | Platform::X86)
    }
}

impl core::fmt::Display for Platform {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// QEMU machine selected by a board's host build policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QemuMachine {
    /// SiFive FU740-compatible `sifive_u` machine, booted through `-bios`.
    SifiveU,
    /// Allwinner H3 `orangepi-pc` machine, booted from SD eGON at 8 KiB.
    OrangePiPc,
    /// Arm SBSA reference platform, booted TF-A-first from two pflash banks.
    SbsaRef,
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
