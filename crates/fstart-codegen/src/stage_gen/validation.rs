//! Capability ordering validation and board predicate helpers.
//!
//! Ensures capabilities are declared in a legal order (e.g., ConsoleInit
//! before anything that logs) and provides predicate functions used by the
//! codegen orchestrator to decide which sections to emit.

use fstart_types::{BoardConfig, BootMedium, Capability, FitParseMode, PayloadKind, StageLayout};

/// Validate that capabilities are in a legal order.
///
/// Rules:
/// - Any capability that logs (all of them except ConsoleInit itself) must
///   come after at least one ConsoleInit.
/// - DriverInit must come after ConsoleInit (it logs device init results).
/// - StageLoad / PayloadLoad should be the last capability (nothing runs after
///   a jump). We warn but don't hard-error since the board author may know
///   what they're doing.
pub(super) fn validate_capability_ordering(
    capabilities: &[Capability],
    config: &BoardConfig,
) -> Option<String> {
    let mut console_inited = false;
    let mut boot_media_declared = false;

    // UefiPayload links CrabEFI statically and doesn't use FFS for the
    // payload itself. However, when firmware (BL31) is configured, it IS
    // loaded from FFS, so BootMedia is required in that case.
    let uefi_has_firmware = is_uefi_payload(config)
        && config
            .payload
            .as_ref()
            .and_then(|p| p.firmware.as_ref())
            .is_some();
    let needs_boot_media = !is_uefi_payload(config) || uefi_has_firmware;

    for cap in capabilities {
        match cap {
            Capability::ClockInit { .. } => {
                // ClockInit runs before ConsoleInit (clocks must be up
                // before the UART can work).  No logging requirement.
            }
            Capability::ConsoleInit { .. } => {
                console_inited = true;
            }
            Capability::BootMedia(_) => {
                if !console_inited {
                    return Some(
                        "BootMedia capability requires ConsoleInit to appear earlier \
                         in the capability list (needed for logging)"
                            .to_string(),
                    );
                }
                boot_media_declared = true;
            }
            Capability::MemoryInit if !console_inited => {
                return Some(
                    "MemoryInit capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::DramInit { .. } if !console_inited => {
                return Some(
                    "DramInit capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::DriverInit if !console_inited => {
                return Some(
                    "DriverInit capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::PciInit { .. } if !console_inited => {
                return Some(
                    "PciInit capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::SigVerify if !console_inited => {
                return Some(
                    "SigVerify capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::SigVerify if !boot_media_declared => {
                return Some(
                    "SigVerify capability requires BootMedia to appear earlier \
                     in the capability list"
                        .to_string(),
                );
            }
            Capability::FdtPrepare if !console_inited => {
                return Some(
                    "FdtPrepare capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::PayloadLoad if !console_inited => {
                return Some(
                    "PayloadLoad capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::PayloadLoad if needs_boot_media && !boot_media_declared => {
                return Some(
                    "PayloadLoad capability requires BootMedia to appear earlier \
                     in the capability list (not needed for UefiPayload)"
                        .to_string(),
                );
            }
            Capability::StageLoad { .. } if !console_inited => {
                return Some(
                    "StageLoad capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::StageLoad { .. } if !boot_media_declared => {
                return Some(
                    "StageLoad capability requires BootMedia to appear earlier \
                     in the capability list"
                        .to_string(),
                );
            }
            Capability::ReturnToFel if !console_inited => {
                return Some(
                    "ReturnToFel capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::LoadNextStage { .. } if !console_inited => {
                return Some(
                    "LoadNextStage capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::SmBiosPrepare if !console_inited => {
                return Some(
                    "SmBiosPrepare capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::AcpiLoad { .. } if !console_inited => {
                return Some(
                    "AcpiLoad capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::MemoryDetect { .. } if !console_inited => {
                return Some(
                    "MemoryDetect capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            _ => {}
        }
    }

    None
}

/// Check whether a capability list uses FFS operations (SigVerify, StageLoad, PayloadLoad).
///
/// Used to decide whether the FFS anchor static needs to be emitted.
pub(super) fn needs_ffs(capabilities: &[Capability]) -> bool {
    capabilities.iter().any(|c| {
        matches!(
            c,
            Capability::SigVerify | Capability::StageLoad { .. } | Capability::PayloadLoad
        )
    })
}

/// Check whether this stage should embed a `FSTART_ANCHOR` static.
///
/// Returns `true` for monolithic builds and the first stage in a
/// multi-stage build. Returns `false` for non-first stages — they
/// scan the boot media for the anchor at runtime instead (the
/// bootblock's patched anchor is in the FFS image copy in DRAM).
pub(super) fn needs_embedded_anchor(stages: &StageLayout, stage_name: Option<&str>) -> bool {
    match stages {
        StageLayout::Monolithic(_) => true,
        StageLayout::MultiStage(stages) => {
            // First stage always embeds the anchor (it gets patched by the builder).
            // Non-first stages scan the boot media instead.
            match (stages.first(), stage_name) {
                (Some(first), Some(name)) => first.name.as_str() == name,
                _ => true,
            }
        }
    }
}

/// Find the `BootMedia` capability's medium, if present.
pub(super) fn get_boot_medium(capabilities: &[Capability]) -> Option<&BootMedium> {
    capabilities.iter().find_map(|c| match c {
        Capability::BootMedia(medium) => Some(medium),
        _ => None,
    })
}

/// Check whether this board has a LinuxBoot payload configured.
pub(super) fn is_linux_boot(config: &BoardConfig) -> bool {
    config
        .payload
        .as_ref()
        .is_some_and(|p| p.kind == PayloadKind::LinuxBoot)
}

/// Check whether this board has a FIT image payload configured.
pub(super) fn is_fit_image(config: &BoardConfig) -> bool {
    config
        .payload
        .as_ref()
        .is_some_and(|p| p.kind == PayloadKind::FitImage)
}

/// Check whether a FIT payload should be parsed at runtime.
pub(super) fn is_fit_runtime(config: &BoardConfig) -> bool {
    config.payload.as_ref().is_some_and(|p| {
        p.kind == PayloadKind::FitImage
            && p.fit_parse.unwrap_or(FitParseMode::Buildtime) == FitParseMode::Runtime
    })
}

/// Check whether this board has a UEFI payload via CrabEFI.
pub(super) fn is_uefi_payload(config: &BoardConfig) -> bool {
    config
        .payload
        .as_ref()
        .is_some_and(|p| p.kind == PayloadKind::UefiPayload)
}
