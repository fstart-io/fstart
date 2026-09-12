//! Host-clean board facts for the Allwinner sunxi family.
//!
//! Only facts that genuinely vary per board belong here: the SoC variant,
//! the SRAM geometry the BROM loads, the populated DRAM size, and the Linux
//! payload files/addresses. Machine invariants (DRAM base, mainstage and
//! handoff addresses, stacks, stage structure) are platform constants owned
//! by the host resolver. No stage budgets, linker addresses or Cargo relays.

use fstart_core::{FirmwareKind, QemuMachine};

/// Real sunxi SoC differences; common SRAM/DRAM placement belongs to the family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SunxiSoc {
    A20,
    H3,
    H5,
    D1,
}

/// Optional trusted-firmware blob packaged with the Linux payload.
#[derive(Debug, Clone, Copy)]
pub struct SunxiFirmware {
    pub kind: FirmwareKind,
    pub file: &'static str,
    pub load_addr: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct BoardFacts {
    pub soc: SunxiSoc,
    /// SRAM base the BROM loads the eGON image to (0x0 on A20/H3).
    pub sram_base: u64,
    pub sram_size: u64,
    /// Populated DRAM size from `0x4000_0000`; the controller detects it live.
    pub dram_size: u64,
    pub kernel_load_addr: u64,
    /// DTB file resolved from the board directory, if present.
    pub dtb: &'static str,
    pub dtb_addr: u64,
    /// Kernel command line patched into the FDT chosen node for Linux boot.
    pub bootargs: &'static str,
    pub firmware: Option<SunxiFirmware>,
    /// Emulator equivalent used by host tooling, if any.
    pub qemu_machine: Option<QemuMachine>,
}

pub trait SunxiBoardFacts {
    const FACTS: BoardFacts;
}
