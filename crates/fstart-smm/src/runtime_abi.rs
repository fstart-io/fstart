/// Maximum number of CPUs represented in the fixed runtime block.
///
/// This mirrors the current `fstart-mp` static mailbox limit.  Boards may
/// request fewer precompiled entry points in RON; they may not exceed this
/// ABI cap without changing the SMM image format version.
pub const MAX_SMM_CPUS: usize = 64;

/// No platform SMI dispatch backend.
pub const SMM_PLATFORM_NONE: u32 = 0;
/// Intel ICH-style PMBASE I/O SMI dispatch backend.
pub const SMM_PLATFORM_INTEL_ICH: u32 = 1;

/// Index of the Intel ICH PMBASE value in [`SmmEntryParams::platform_data`].
pub const SMM_PLATFORM_DATA_ICH_PM_BASE: usize = 0;
/// Index of the Intel ICH GPE0_STS offset in [`SmmEntryParams::platform_data`].
pub const SMM_PLATFORM_DATA_ICH_GPE0_STS_OFFSET: usize = 1;

/// Runtime block consumed by the fstart SMM handler.
///
/// Firmware writes this structure into SMRAM before locking the region.  The
/// SMM handler is PIC and discovers this block from the copied handler blob
/// rather than relying on link-time absolute addresses.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmmRuntime {
    /// Permanent SMRAM/TSEG base.
    pub smram_base: u64,
    /// Permanent SMRAM/TSEG size.
    pub smram_size: u64,
    /// Number of active CPU entries.
    pub num_cpus: u16,
    /// Size of each CPU save-state area.
    pub save_state_size: u16,
    /// Per-CPU SMM stack size.
    pub stack_size: u32,
    /// Image-relative handler/data region offset used to build this runtime.
    pub common_offset: u32,
    /// Image-relative entry descriptor table offset.
    pub entries_offset: u32,
    /// Save-state top address for each CPU, indexed by logical CPU number.
    pub save_state_top: [u64; MAX_SMM_CPUS],
    /// Runtime state flags maintained by the SMM handler.
    pub flags: u32,
    /// Global SMI handler serialization lock.
    pub handler_lock: u32,
    /// Last APMC command observed by the SMM handler.
    pub last_apm_command: u32,
    /// Per-APMC-command dispatch counters.
    pub apm_command_counts: [u32; 256],
    /// Per-logical-CPU SMI entry counters.
    pub cpu_entry_counts: [u32; MAX_SMM_CPUS],
}

impl SmmRuntime {
    /// Construct an empty runtime block for `num_cpus` CPUs.
    pub const fn new(
        smram_base: u64,
        smram_size: u64,
        num_cpus: u16,
        save_state_size: u16,
        stack_size: u32,
        common_offset: u32,
        entries_offset: u32,
    ) -> Self {
        Self {
            smram_base,
            smram_size,
            num_cpus,
            save_state_size,
            stack_size,
            common_offset,
            entries_offset,
            save_state_top: [0; MAX_SMM_CPUS],
            flags: 0,
            handler_lock: 0,
            last_apm_command: 0,
            apm_command_counts: [0; 256],
            cpu_entry_counts: [0; MAX_SMM_CPUS],
        }
    }
}

/// Per-entry PIC parameter block filled by the firmware loader after copying
/// each entry stub to `SMBASE + 0x8000`.
///
/// This block is data, not relocated code.  fstart and coreboot may patch it
/// in SMRAM without violating the image's no-code-relocation contract.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmmEntryParams {
    /// Logical CPU number assigned to this entry slot.
    pub cpu: u32,
    /// Per-CPU SMM stack size.
    pub stack_size: u32,
    /// Top of this CPU's stack.
    pub stack_top: u64,
    /// Absolute address of the copied SMM handler entry.
    pub common_entry: u64,
    /// Absolute address of the copied runtime block, or 0 if absent.
    pub runtime: u64,
    /// Absolute address of this CPU's coreboot-compatible module args block,
    /// or 0 if the image was built without that compatibility feature.
    pub coreboot_module_args: u64,
    /// CR3 to use before entering long mode on x86_64.
    pub cr3: u64,
    /// Absolute address where this entry stub was copied.
    ///
    /// SMM CS has a hidden full SMBASE but only a truncated visible selector on
    /// high TSEG placements, so the 16-bit entry code cannot derive its own
    /// physical base from `cs << 4`.  The loader patches this data field.
    pub entry_base: u64,
    /// Platform dispatch kind consumed by the Rust SMM handler.
    pub platform_kind: u32,
    /// Platform dispatch flags.
    pub platform_flags: u32,
    /// Opaque platform dispatch data.
    pub platform_data: [u64; 4],
}

/// Coreboot-compatible module argument block.
///
/// This intentionally keeps the stable subset needed by coreboot consumers:
/// the logical CPU index and the stack canary pointer passed from the SMM
/// entry stub.  Fields are fixed-width so the generated image is independent
/// of the host tool's pointer width.  Additional coreboot-specific runtime data
/// should be put behind a versioned extension rather than changing this prefix.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorebootModuleArgs {
    /// Logical CPU number.
    pub cpu: u64,
    /// Pointer to the per-CPU stack canary.
    pub canary: u64,
}
