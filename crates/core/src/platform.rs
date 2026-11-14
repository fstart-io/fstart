//! Target platform / ISA identity.

/// Target platform / ISA.
///
/// This enum is the single source of truth for platform identity.
/// Adding a new platform variant automatically produces compiler errors
/// at every `match` site that needs updating — no stringly-typed
/// matching required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
pub enum Platform {
    /// RISC-V 64-bit (riscv64gc-unknown-none-elf)
    Riscv64,
    /// AArch64 / ARMv8-A (aarch64-unknown-none)
    Aarch64,
    /// ARMv7-A (armv7a-none-eabi)
    Armv7,
    /// ARMv5TE (armv5te-none-eabi): the ARM926EJ-S, and the ARM1176 when
    /// LDREX/STREX are unusable (no exclusive monitor with the MMU off).
    Armv5te,
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
            Platform::Armv5te => "armv5te-none-eabi",
            Platform::X86_64 => "x86_64-unknown-none",
            Platform::X86 => "i686-unknown-none",
        }
    }

    /// Linker `OUTPUT_ARCH(...)` name.
    pub fn linker_arch(&self) -> &'static str {
        match self {
            Platform::Riscv64 => "riscv",
            Platform::Aarch64 => "aarch64",
            Platform::Armv7 | Platform::Armv5te => "arm",
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
            Platform::Armv5te => "armv5te",
            Platform::X86_64 => "x86_64",
            Platform::X86 => "x86",
        }
    }

    /// Either x86 flavour: the same firmware flow, entered in long mode or
    /// staying in 32-bit protected mode.
    pub const fn is_x86(&self) -> bool {
        matches!(self, Platform::X86_64 | Platform::X86)
    }

    /// 32-bit address space and ELF32 images.
    pub const fn is_32bit(&self) -> bool {
        matches!(self, Platform::Armv7 | Platform::Armv5te | Platform::X86)
    }
}

impl core::fmt::Display for Platform {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Kind of runtime firmware loaded before the OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "host", derive(serde::Serialize, serde::Deserialize))]
pub enum FirmwareKind {
    /// OpenSBI or RustSBI using the fw_dynamic protocol.
    /// Entry: a0=hartid, a1=dtb, a2=&fw_dynamic_info.
    OpenSbi,
    /// ARM Trusted Firmware BL31.
    /// Entry: x0=&bl_params.
    ArmTrustedFirmware,
}
