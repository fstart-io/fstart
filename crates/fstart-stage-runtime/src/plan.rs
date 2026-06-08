//! Data-only stage plans for the handwritten executor.
//!
//! Codegen emits these values for every `fstart-stage` build. The plan records
//! resolved stage facts only: operation order, device IDs, static candidate
//! tables, and small descriptor values. Runtime behavior belongs to handwritten
//! flow modules.

use crate::BootMediaCandidate;
#[cfg(feature = "flow-boot-media")]
use fstart_services::FirmwareImage;
use fstart_types::DeviceId;
#[cfg(feature = "flow-boot-media")]
use fstart_types::TempRamBuffer;

/// A compiled, data-only plan for one firmware stage.
#[derive(Debug, Clone, Copy)]
pub struct StagePlan {
    /// Stage name for diagnostics. Empty for monolithic stages.
    pub stage_name: &'static str,
    /// Capabilities lowered to resolved operation facts.
    pub ops: &'static [StageOp],
    /// Devices whose hardware state is known to persist from a previous stage.
    pub persistent_inited: &'static [DeviceId],
    /// Root-first construction chains for runtime devices.
    pub device_init: &'static [DeviceInitPlan],
    /// Enabled runtime devices for `DriverInit`, in root-first order.
    pub all_devices: &'static [DeviceId],
    /// Devices that may fail `DriverInit` without halting the stage.
    pub optional_devices: &'static [DeviceId],
    /// DriverInit devices gated by boot-source matching.
    pub boot_media_gated: &'static [BootMediaCandidate],
}

/// Root-first construction chain for one target device.
#[derive(Debug, Clone, Copy)]
pub struct DeviceInitPlan {
    /// Device whose ready state this chain establishes.
    pub device: DeviceId,
    /// Runtime device IDs to construct/init before `device` is considered ready.
    pub chain: &'static [DeviceId],
}

impl StagePlan {
    /// Empty plan for tests and safe defaults.
    pub const EMPTY: Self = Self {
        stage_name: "",
        ops: &[],
        persistent_inited: &[],
        device_init: &[],
        all_devices: &[],
        optional_devices: &[],
        boot_media_gated: &[],
    };
}

/// A single operation fact in stage order.
///
/// Variants for optional flow families are Cargo-feature gated. A stage build
/// enables only the flow families its generated plan uses.
#[derive(Debug, Clone, Copy)]
pub enum StageOp {
    /// Initialize one clock controller device.
    #[cfg(feature = "flow-clock-init")]
    ClockInit(DeviceId),
    /// Initialize one console device and install logging.
    #[cfg(feature = "flow-console-init")]
    ConsoleInit(DeviceId),
    /// Mark memory initialized on platforms where DRAM already exists.
    #[cfg(feature = "flow-memory-init")]
    MemoryInit,
    /// Run DRAM training on one memory-controller device.
    #[cfg(feature = "flow-dram-init")]
    DramInit(DeviceId),
    /// Initialize all remaining runtime devices.
    #[cfg(feature = "flow-driver-init")]
    DriverInit,
    /// Run the pre-console phase over the listed devices.
    #[cfg(feature = "flow-phases")]
    PreConsoleInit(&'static [DeviceId]),
    /// Run the early-init phase over the listed devices.
    #[cfg(feature = "flow-phases")]
    EarlyInit(&'static [DeviceId]),
    /// Rebuild per-stage software bindings.
    #[cfg(feature = "flow-phases")]
    StageLocalInit(&'static [DeviceId]),
    /// Run post-DRAM initialization.
    #[cfg(feature = "flow-phases")]
    PostDramInit(&'static [DeviceId]),
    /// Run final lockdown.
    #[cfg(feature = "flow-phases")]
    FinalizeInit(&'static [DeviceId]),
    /// Enumerate a PCI root bus.
    #[cfg(feature = "flow-pci")]
    PciInit(DeviceId),
    /// Detect memory through a runtime provider.
    #[cfg(feature = "flow-memory-detect")]
    MemoryDetect(DeviceId),

    /// Select a firmware image from a provider device.
    #[cfg(feature = "flow-boot-media")]
    BootMediaFirmwareProvider {
        /// Provider device ID.
        provider: DeviceId,
        /// Optional scratch arena for temporary FFS/payload buffers.
        temp_ram_buffer: Option<TempRamBuffer>,
    },
    /// Select a fixed Rust-platform firmware image mapping.
    #[cfg(feature = "flow-boot-media")]
    BootMediaPlatformFirmwareImage {
        /// Platform-provided image windows.
        image: &'static FirmwareImage,
        /// Optional scratch arena for temporary FFS/payload buffers.
        temp_ram_buffer: Option<TempRamBuffer>,
    },
    /// Select a firmware image by matching hardware boot-source candidates.
    #[cfg(feature = "flow-boot-media")]
    BootMediaPlatformBootSource {
        /// Candidate table.
        candidates: &'static [BootMediaCandidate],
        /// Optional scratch arena for temporary FFS/payload buffers.
        temp_ram_buffer: Option<TempRamBuffer>,
    },

    /// Verify the firmware filesystem manifest.
    #[cfg(feature = "flow-ffs")]
    SigVerify,
    /// Load and jump to the payload.
    #[cfg(feature = "flow-ffs")]
    PayloadLoad,
    /// Load and jump to another named stage from FFS.
    #[cfg(feature = "flow-ffs")]
    StageLoad { next_stage: &'static str },

    /// Prepare an FDT for OS handoff.
    #[cfg(feature = "flow-fdt")]
    FdtPrepare,

    /// Initialize CPUs and optional SMM.
    #[cfg(feature = "flow-mp")]
    MpInit {
        /// Expected logical CPU count.
        num_cpus: u16,
        /// Whether SMM setup is enabled.
        smm: bool,
    },

    /// Generate ACPI tables.
    #[cfg(feature = "flow-acpi")]
    AcpiPrepare,
    /// Load ACPI tables from a provider device.
    #[cfg(feature = "flow-acpi")]
    AcpiLoad(DeviceId),

    /// Generate SMBIOS tables.
    #[cfg(feature = "flow-smbios")]
    SmBiosPrepare,

    /// Return to sunxi FEL recovery.
    #[cfg(feature = "flow-fel")]
    ReturnToFel,
    /// Load a next stage using platform boot-source metadata.
    #[cfg(feature = "flow-fel")]
    LoadNextStage {
        /// Candidate table.
        candidates: &'static [BootMediaCandidate],
        /// Next stage name.
        next_stage: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_plan_is_empty() {
        assert!(StagePlan::EMPTY.ops.is_empty());
        assert!(StagePlan::EMPTY.all_devices.is_empty());
    }

    #[test]
    fn stage_op_stays_small() {
        let size = core::mem::size_of::<StageOp>();
        assert!(
            size <= 48,
            "StageOp is {size} bytes; put large operands behind &'static refs",
        );
    }
}
