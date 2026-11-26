use core::sync::atomic::AtomicU64;

/// Bytes reserved in every image for the concrete handler's configuration
/// (SP7's carries its root bridges' PCI windows, which bound what its
/// handler reaches for the OS).
pub const HANDLER_CONFIG_CAPACITY: usize = 1024;
/// Alignment of the handler configuration block.
pub const HANDLER_CONFIG_ALIGNMENT: usize = 16;

/// Runtime block shared by every CPU's permanent SMM entry.
///
/// The installer writes it into SMRAM before lock. It holds no pointers; the
/// handler configuration is addressed relative to the start of this block.
#[repr(C)]
#[derive(Debug)]
pub struct SmmRuntime {
    /// Permanent SMRAM/TSEG base.
    pub smram_base: u64,
    /// Permanent SMRAM/TSEG size.
    pub smram_size: u64,
    /// Number of CPUs whose SMBASE points at this image.
    pub num_cpus: u16,
    /// Reserved; zero.
    pub reserved: u16,
    /// Per-CPU SMM stack size.
    pub stack_size: u32,
    /// Offset of the handler configuration from the start of this block.
    pub handler_config_offset: u32,
    /// Size of the handler configuration actually written by the installer.
    pub handler_config_size: u32,
    /// The rendezvous: the generation of the current or the last owner in
    /// bits 63:32, its logical CPU index + 1 in bits 31:0 (0: free). Each
    /// owner takes the next generation.
    pub rendezvous: AtomicU64,
    /// The CPUs parked for a generation's owner: the generation in bits
    /// 63:32, how many acknowledged it in bits 31:0.
    pub parked: AtomicU64,
}

impl SmmRuntime {
    /// Construct the runtime block written by the installer.
    pub const fn new(
        smram_base: u64,
        smram_size: u64,
        num_cpus: u16,
        stack_size: u32,
        handler_config_offset: u32,
        handler_config_size: u32,
    ) -> Self {
        Self {
            smram_base,
            smram_size,
            num_cpus,
            reserved: 0,
            stack_size,
            handler_config_offset,
            handler_config_size,
            rendezvous: AtomicU64::new(0),
            parked: AtomicU64::new(0),
        }
    }
}

/// Per-entry parameter block inside each copied entry stub.
///
/// This block is data, not code: the loader patches it after copying the stub
/// to `SMBASE + 0x8000` without relocating any instruction.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmmEntryParams {
    /// Logical CPU number assigned to this entry slot.
    pub cpu: u32,
    /// Per-CPU SMM stack size.
    pub stack_size: u32,
    /// Top of this CPU's stack.
    pub stack_top: u64,
    /// Absolute address of the function the stub calls.
    pub common_entry: u64,
    /// Absolute address of the [`SmmRuntime`]. The temporary relocation stub
    /// uses this field as its callback argument instead.
    pub runtime: u64,
    /// Absolute address of this CPU's coreboot module-args block, or 0.
    pub coreboot_module_args: u64,
    /// CR3 loaded before entering long mode.
    pub cr3: u64,
    /// Absolute address where this stub was copied. SMM CS has the full
    /// SMBASE in its hidden base but only a truncated selector, so the 16-bit
    /// entry code cannot derive its own address from `cs << 4`.
    pub entry_base: u64,
}

/// Coreboot-compatible module argument block.
///
/// The stable subset coreboot consumers need: the logical CPU index and the
/// stack canary pointer. Fields are fixed-width so the image does not depend
/// on the host tool's pointer width.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorebootModuleArgs {
    /// Logical CPU number.
    pub cpu: u64,
    /// Pointer to the per-CPU stack canary.
    pub canary: u64,
}
