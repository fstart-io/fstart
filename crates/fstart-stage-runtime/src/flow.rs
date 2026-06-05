//! Handwritten executor for [`StagePlan`](crate::StagePlan).
//!
//! Flow families are feature-gated so each stage compiles only the operation
//! arms it uses.

#[cfg(any(
    feature = "flow-phases",
    feature = "flow-pci",
    feature = "flow-memory-detect",
    feature = "flow-acpi"
))]
use fstart_services::device::DeviceError;
#[cfg(any(
    feature = "flow-clock-init",
    feature = "flow-console-init",
    feature = "flow-dram-init",
    feature = "flow-driver-init",
    feature = "flow-phases",
    feature = "flow-pci",
    feature = "flow-memory-detect",
    feature = "flow-boot-media",
    feature = "flow-acpi",
    feature = "flow-fel",
))]
use fstart_types::DeviceId;

#[cfg(any(
    feature = "flow-clock-init",
    feature = "flow-console-init",
    feature = "flow-dram-init",
    feature = "flow-driver-init",
    feature = "flow-phases",
    feature = "flow-pci",
    feature = "flow-memory-detect",
    feature = "flow-boot-media",
    feature = "flow-acpi",
    feature = "flow-fel",
))]
use crate::DeviceMask;
#[cfg(any(
    feature = "flow-clock-init",
    feature = "flow-console-init",
    feature = "flow-memory-init",
    feature = "flow-dram-init",
    feature = "flow-driver-init",
    feature = "flow-phases",
    feature = "flow-pci",
    feature = "flow-memory-detect",
    feature = "flow-boot-media",
    feature = "flow-ffs",
    feature = "flow-fdt",
    feature = "flow-mp",
    feature = "flow-acpi",
    feature = "flow-smbios",
    feature = "flow-fel",
))]
use crate::StageOp;
use crate::{Board, StagePlan};

/// Execute a data-only stage plan using handwritten Rust flow.
///
/// This function is compiled only with the `stage-executor` feature. Each flow
/// arm is compiled only when its `flow-*` feature is enabled. Codegen emits
/// feature guard `compile_error!` items when a plan contains an operation whose
/// flow feature is missing.
pub fn run_stage<B: Board>(board: &mut B, plan: &'static StagePlan) -> ! {
    #[cfg(any(
        feature = "flow-clock-init",
        feature = "flow-console-init",
        feature = "flow-dram-init",
        feature = "flow-driver-init",
        feature = "flow-phases",
        feature = "flow-pci",
        feature = "flow-memory-detect",
        feature = "flow-boot-media",
        feature = "flow-acpi",
        feature = "flow-fel",
    ))]
    let mut inited = DeviceMask::from_slice(plan.persistent_inited);

    for op in plan.ops {
        match *op {
            #[cfg(feature = "flow-clock-init")]
            StageOp::ClockInit(id) => init_once(board, &mut inited, id),
            #[cfg(feature = "flow-console-init")]
            StageOp::ConsoleInit(id) => console_init(board, &mut inited, id),
            #[cfg(feature = "flow-memory-init")]
            StageOp::MemoryInit => fstart_capabilities::memory_init(),
            #[cfg(feature = "flow-dram-init")]
            StageOp::DramInit(id) => dram_init(board, &mut inited, id),
            #[cfg(feature = "flow-driver-init")]
            StageOp::DriverInit => driver_init(board, plan, &mut inited),
            #[cfg(feature = "flow-phases")]
            StageOp::PreConsoleInit(ids) => phase(board, &mut inited, ids, Board::pre_console_init),
            #[cfg(feature = "flow-phases")]
            StageOp::EarlyInit(ids) => phase(board, &mut inited, ids, Board::early_init),
            #[cfg(feature = "flow-phases")]
            StageOp::StageLocalInit(ids) => phase(board, &mut inited, ids, Board::stage_local_init),
            #[cfg(feature = "flow-phases")]
            StageOp::PostDramInit(ids) => phase(board, &mut inited, ids, Board::post_dram_init),
            #[cfg(feature = "flow-phases")]
            StageOp::FinalizeInit(ids) => phase(board, &mut inited, ids, Board::finalize_init),
            #[cfg(feature = "flow-pci")]
            StageOp::PciInit(id) => device_op(board, &mut inited, id, Board::pci_init),
            #[cfg(feature = "flow-memory-detect")]
            StageOp::MemoryDetect(id) => device_op(board, &mut inited, id, Board::memory_detect),

            #[cfg(feature = "flow-boot-media")]
            StageOp::BootMediaFirmwareProvider {
                provider,
                temp_ram_buffer,
            } => boot_media_firmware_provider(board, &mut inited, provider, temp_ram_buffer),
            #[cfg(feature = "flow-boot-media")]
            StageOp::BootMediaPlatformFirmwareImage {
                image,
                temp_ram_buffer,
            } => publish_firmware_image_boot_media(board, *image, temp_ram_buffer),
            #[cfg(feature = "flow-boot-media")]
            StageOp::BootMediaPlatformBootSource {
                candidates,
                temp_ram_buffer,
            } => boot_media_platform_boot_source(board, &mut inited, candidates, temp_ram_buffer),

            #[cfg(feature = "flow-ffs")]
            StageOp::SigVerify => sig_verify(board),
            #[cfg(feature = "flow-ffs")]
            StageOp::PayloadLoad => board.payload_load(),
            #[cfg(feature = "flow-ffs")]
            StageOp::StageLoad { next_stage } => board.stage_load(next_stage),

            #[cfg(feature = "flow-fdt")]
            StageOp::FdtPrepare => board.fdt_prepare(),

            #[cfg(feature = "flow-mp")]
            StageOp::MpInit {
                cpu_model,
                num_cpus,
                smm,
            } => {
                if board.mp_init(cpu_model, num_cpus, smm).is_err() {
                    board.halt();
                }
            }

            #[cfg(feature = "flow-acpi")]
            StageOp::AcpiPrepare => board.acpi_prepare(),
            #[cfg(feature = "flow-acpi")]
            StageOp::AcpiLoad(id) => device_op(board, &mut inited, id, Board::acpi_load),

            #[cfg(feature = "flow-smbios")]
            StageOp::SmBiosPrepare => board.smbios_prepare(),

            #[cfg(feature = "flow-fel")]
            StageOp::ReturnToFel => board.return_to_fel(),
            #[cfg(feature = "flow-fel")]
            StageOp::LoadNextStage {
                candidates,
                next_stage,
            } => load_next_stage(board, &mut inited, candidates, next_stage),
        }
    }

    board.halt()
}

#[cfg(feature = "flow-clock-init")]
fn init_once<B: Board>(board: &mut B, inited: &mut DeviceMask, id: DeviceId) {
    if inited.contains(id) {
        return;
    }
    if board.init_device(id).is_err() {
        board.halt();
    }
    inited.set(id);
}

#[cfg(feature = "flow-console-init")]
fn console_init<B: Board>(board: &mut B, inited: &mut DeviceMask, id: DeviceId) {
    if board.init_device(id).is_err() {
        board.halt();
    }
    // SAFETY: codegen validation guarantees this operation names a Console
    // provider, and init_device has just constructed it.
    unsafe {
        board.install_logger(id);
    }
    inited.set(id);
}

#[cfg(feature = "flow-dram-init")]
fn dram_init<B: Board>(board: &mut B, inited: &mut DeviceMask, id: DeviceId) {
    if board.init_device(id).is_err() {
        board.halt();
    }
    if board.dram_init(id).is_err() {
        board.halt();
    }
    inited.set(id);
}

#[cfg(feature = "flow-phases")]
fn phase<B: Board>(
    board: &mut B,
    inited: &mut DeviceMask,
    ids: &'static [DeviceId],
    run: fn(&mut B, &[DeviceId]) -> Result<(), DeviceError>,
) {
    for id in ids {
        if board.init_device(*id).is_err() {
            board.halt();
        }
    }
    if run(board, ids).is_err() {
        board.halt();
    }
    for id in ids {
        inited.set(*id);
    }
}

#[cfg(any(
    feature = "flow-pci",
    feature = "flow-memory-detect",
    feature = "flow-acpi"
))]
fn device_op<B: Board>(
    board: &mut B,
    inited: &mut DeviceMask,
    id: DeviceId,
    run: fn(&mut B, DeviceId) -> Result<(), DeviceError>,
) {
    if board.init_device(id).is_err() {
        board.halt();
    }
    if run(board, id).is_err() {
        board.halt();
    }
    inited.set(id);
}

#[cfg(feature = "flow-driver-init")]
fn driver_init<B: Board>(board: &mut B, plan: &StagePlan, inited: &mut DeviceMask) {
    let active_gated =
        select_boot_media_candidate(board, plan.boot_media_gated).map(|candidate| candidate.device);

    for id in plan.all_devices {
        if inited.contains(*id) {
            continue;
        }
        if driver_init_is_gated(plan, *id) && active_gated != Some(*id) {
            fstart_log::info!(
                "skipping driver init (boot-media gated, not active): id {}",
                *id,
            );
            continue;
        }
        match board.init_device(*id) {
            Ok(()) => inited.set(*id),
            Err(_) if driver_init_is_optional(plan, *id) => {
                fstart_log::warn!("driver init failed (optional), continuing: id {}", *id);
            }
            Err(_) => {
                fstart_log::error!("FATAL: driver init failed for id {}", *id);
                board.halt();
            }
        }
    }
}

#[cfg(feature = "flow-driver-init")]
fn driver_init_is_gated(plan: &StagePlan, id: DeviceId) -> bool {
    plan.boot_media_gated
        .iter()
        .any(|candidate| candidate.device == id)
}

#[cfg(feature = "flow-driver-init")]
fn driver_init_is_optional(plan: &StagePlan, id: DeviceId) -> bool {
    plan.optional_devices.iter().any(|optional| *optional == id)
}

#[cfg(feature = "flow-ffs")]
fn sig_verify<B: Board>(board: &B) {
    let Some(anchor) = board.ffs_anchor() else {
        return;
    };
    board.with_boot_media("sig_verify", (), |media, _scratch| {
        fstart_capabilities::sig_verify(anchor, media);
    });
}

#[cfg(feature = "flow-boot-media")]
fn boot_media_firmware_provider<B: Board>(
    board: &mut B,
    inited: &mut DeviceMask,
    provider: DeviceId,
    temp_ram_buffer: Option<fstart_types::TempRamBuffer>,
) {
    if board.init_device(provider).is_err() {
        board.halt();
    }
    inited.set(provider);
    let Ok(image) = board.firmware_image(provider) else {
        board.halt();
    };
    publish_firmware_image_boot_media(board, image, temp_ram_buffer);
}

#[cfg(feature = "flow-boot-media")]
fn publish_firmware_image_boot_media<B: Board>(
    board: &mut B,
    image: fstart_services::FirmwareImage,
    temp_ram_buffer: Option<fstart_types::TempRamBuffer>,
) {
    if image.validate().is_err() {
        board.halt();
    }
    if let Some(window) = image.contiguous_window() {
        if window.cpu_base != 0 {
            if let Some(anchor) = board.ffs_anchor() {
                fstart_services::ffs_context::set_memory_mapped(
                    anchor,
                    window.cpu_base,
                    window.size,
                );
            }
        }
    }
    board.set_boot_media_state(crate::BootMediaState::from_firmware_image(
        image,
        temp_ram_buffer,
    ));
}

#[cfg(feature = "flow-boot-media")]
fn boot_media_platform_boot_source<B: Board>(
    board: &mut B,
    inited: &mut DeviceMask,
    candidates: &'static [crate::BootMediaCandidate],
    temp_ram_buffer: Option<fstart_types::TempRamBuffer>,
) {
    let Some(candidate) = select_boot_media_candidate(board, candidates) else {
        board.halt();
    };
    let id = candidate.device;
    if board.init_device(id).is_err() {
        board.halt();
    }
    inited.set(id);
    publish_block_boot_media(
        board,
        candidate.device,
        candidate.offset,
        candidate.size,
        temp_ram_buffer,
    );
}

#[cfg(any(feature = "flow-boot-media", feature = "flow-fel"))]
fn publish_block_boot_media<B: Board>(
    board: &mut B,
    device: DeviceId,
    offset: u64,
    size: u64,
    temp_ram_buffer: Option<fstart_types::TempRamBuffer>,
) {
    board.set_boot_media_state(crate::BootMediaState::from_block_firmware_image(
        device,
        offset,
        size,
        temp_ram_buffer,
    ));
}

#[cfg(feature = "flow-fel")]
fn load_next_stage<B: Board>(
    board: &mut B,
    inited: &mut DeviceMask,
    candidates: &'static [crate::BootMediaCandidate],
    next_stage: &'static str,
) {
    let Some(candidate) = select_boot_media_candidate(board, candidates) else {
        board.halt();
    };
    let id = candidate.device;
    if board.init_device(id).is_err() {
        board.halt();
    }
    inited.set(id);
    publish_block_boot_media(
        board,
        candidate.device,
        candidate.offset,
        candidate.size,
        None,
    );
    board.load_next_stage(next_stage);
}

#[cfg(any(
    feature = "flow-driver-init",
    feature = "flow-boot-media",
    feature = "flow-fel"
))]
fn select_boot_media_candidate<B: Board>(
    board: &B,
    candidates: &'static [crate::BootMediaCandidate],
) -> Option<&'static crate::BootMediaCandidate> {
    if candidates.is_empty() {
        return None;
    }
    let boot_media = board.soc_boot_media()?;
    fstart_log::info!("boot media detect: {:#x}", boot_media);
    let selected = candidates
        .iter()
        .find(|candidate| candidate.media_ids.iter().any(|&id| id == boot_media));
    if selected.is_none() {
        fstart_log::error!(
            "boot media select: no candidate matched boot media {:#x}",
            boot_media,
        );
    }
    selected
}
