//! Capability ordering validation and board predicate helpers.
//!
//! Ensures capabilities are declared in a legal order (e.g., ConsoleInit
//! before anything that logs) and provides predicate functions used by the
//! codegen orchestrator to decide which sections to emit.

use fstart_device_registry::{DriverInstance, Service, ServiceSet};

use fstart_types::{
    BoardConfig, BootMedium, Capability, FdtSource, FitParseMode, PayloadKind, Platform,
    StageLayout,
};

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
    device_services: &[ServiceSet],
    stage_runs_from_ram: bool,
) -> Option<String> {
    let mut console_inited = false;
    let mut boot_media_declared = false;
    let mut memory_ready = stage_runs_from_ram;
    let mut mp_inited = false;

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
            Capability::ClockInit => {
                // ClockInit runs before ConsoleInit (clocks must be up
                // before the UART can work).  No logging requirement.
            }
            Capability::ConsoleInit => {
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
            Capability::MemoryInit => {
                memory_ready = true;
            }
            Capability::DramInit if !console_inited => {
                return Some(
                    "DramInit capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::DramInit => {
                memory_ready = true;
            }
            Capability::MpInit { smm, .. } if !console_inited => {
                return Some(
                    "MpInit capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::MpInit { smm: true, .. } if config.platform != Platform::X86_64 => {
                return Some("MpInit(smm: true) is currently supported only on X86_64".to_string());
            }
            Capability::MpInit { smm: true, .. } if config.smm.is_none() => {
                return Some(
                    "MpInit(smm: true) requires a top-level board.smm configuration block"
                        .to_string(),
                );
            }
            Capability::MpInit { smm: true, .. }
                if device_services
                    .iter()
                    .filter(|services| services.contains(Service::SmmOps))
                    .count()
                    != 1 =>
            {
                return Some(
                    "MpInit(smm: true) requires exactly one device \
                     that provides SmmOps"
                        .to_string(),
                );
            }
            Capability::MpInit {
                smm: true,
                max_cpus,
                ..
            } if config
                .smm
                .and_then(|s| s.entry_points)
                .is_some_and(|entries| entries < *max_cpus) =>
            {
                return Some(
                    "board.smm.entry_points must be greater than or equal to MpInit.max_cpus"
                        .to_string(),
                );
            }
            Capability::MpInit { smm: true, .. } if !memory_ready => {
                return Some(
                    "MpInit(smm: true) must appear after MemoryInit or DramInit so SMRAM \
                     installation runs from a DRAM-backed stage"
                        .to_string(),
                );
            }
            Capability::MpInit { .. } => {
                mp_inited = true;
            }
            Capability::AcpiPrepare if x86_acpi_requires_mp(config) && !mp_inited => {
                return Some(
                    "AcpiPrepare with x86 platform ACPI requires MpInit to appear earlier"
                        .to_string(),
                );
            }
            Capability::AcpiPrepare
                if x86_acpi_requires_mp(config)
                    && device_services
                        .iter()
                        .filter(|services| services.contains(Service::X86AcpiPlatformProvider))
                        .count()
                        != 1 =>
            {
                return Some(
                    "AcpiPrepare with x86 platform ACPI requires exactly one device that provides X86AcpiPlatformProvider"
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
            Capability::PciInit if !console_inited => {
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
            Capability::AcpiLoad if !console_inited => {
                return Some(
                    "AcpiLoad capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::MemoryDetect if !console_inited => {
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

/// Validate that named capability devices provide the services required by
/// their roles.
pub(super) fn validate_capability_services(
    capabilities: &[Capability],
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_services: &[ServiceSet],
) -> Option<String> {
    for cap in capabilities {
        if let Err(err) = validate_capability_service(cap, config, instances, device_services) {
            return Some(err);
        }
    }

    None
}

/// Validate stage-wide configuration requirements that are not tied to a
/// specific service provider device.
pub(super) fn validate_stage_scope_requirements(
    capabilities: &[Capability],
    config: &BoardConfig,
) -> Option<String> {
    let has = |needle: fn(&Capability) -> bool| capabilities.iter().any(needle);

    if has(|cap| matches!(cap, Capability::AcpiPrepare)) && config.acpi.is_none() {
        return Some("AcpiPrepare capability requires top-level board.acpi config".to_string());
    }

    if has(|cap| matches!(cap, Capability::SmBiosPrepare)) && config.smbios.is_none() {
        return Some("SmBiosPrepare capability requires top-level board.smbios config".to_string());
    }

    if has(|cap| matches!(cap, Capability::FdtPrepare)) {
        let Some(payload) = config.payload.as_ref() else {
            return Some("FdtPrepare requires a payload with an FDT source".to_string());
        };

        match payload.fdt {
            FdtSource::Platform => {}
            FdtSource::Override(_) => {
                if !needs_ffs(capabilities) {
                    return Some(
                        "FdtPrepare with an override DTB requires an FFS-using stage".to_string(),
                    );
                }
            }
            FdtSource::Generated | FdtSource::GeneratedWithOverride(_) => {
                return Some(
                    "FdtPrepare supports only Platform or Override FDT sources".to_string(),
                );
            }
        }
    }

    None
}

fn validate_capability_service(
    cap: &Capability,
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_services: &[ServiceSet],
) -> Result<(), String> {
    match cap {
        Capability::ClockInit => require_unique_service(
            config,
            device_services,
            Service::ClockController,
            "ClockInit",
        ),
        Capability::ConsoleInit => {
            require_unique_service(config, device_services, Service::Console, "ConsoleInit")
        }
        Capability::BootMedia(BootMedium::FirmwareImage { .. }) => {
            validate_firmware_image_provider(config, instances, device_services)
        }
        Capability::DramInit => require_unique_service(
            config,
            device_services,
            Service::MemoryController,
            "DramInit",
        ),
        Capability::PciInit => {
            require_unique_service(config, device_services, Service::PciRootBus, "PciInit")
        }
        Capability::AcpiLoad => require_unique_service(
            config,
            device_services,
            Service::AcpiTableProvider,
            "AcpiLoad",
        ),
        Capability::MemoryDetect => require_unique_service(
            config,
            device_services,
            Service::MemoryDetector,
            "MemoryDetect",
        ),
        Capability::LoadNextStage { .. } => validate_platform_boot_source_candidates(
            "LoadNextStage",
            config,
            instances,
            device_services,
        ),
        _ => Ok(()),
    }
}

fn validate_firmware_image_provider(
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_services: &[ServiceSet],
) -> Result<(), String> {
    let mut providers = config
        .devices
        .iter()
        .zip(device_services.iter())
        .filter(|(device, services)| {
            device.enabled && services.contains(Service::FirmwareImageProvider)
        })
        .map(|(device, _)| device.name.as_str());
    let Some(first) = providers.next() else {
        if fstart_device_registry::platform_firmware_image(config.name.as_str(), config.platform)
            .is_some()
        {
            return Ok(());
        }
        return validate_platform_boot_source_candidates(
            "BootMedia(FirmwareImage)",
            config,
            instances,
            device_services,
        );
    };
    if let Some(second) = providers.next() {
        return Err(format!(
            "BootMedia(FirmwareImage) has multiple providers ('{first}', '{second}', ...); provider choice must live in typed board/build policy, not Capability"
        ));
    }
    Ok(())
}

fn validate_platform_boot_source_candidates(
    capability: &str,
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_services: &[ServiceSet],
) -> Result<(), String> {
    let candidates = fstart_device_registry::platform_boot_media_candidates(
        config.name.as_str(),
        config.platform,
    );
    if candidates.is_empty() {
        return Err(format!(
            "{capability} requires Rust platform boot-source candidates; board capabilities may not name boot devices"
        ));
    }
    for candidate in candidates {
        require_device_service(
            config,
            device_services,
            candidate.device,
            Service::BlockDevice,
            capability,
        )?;
        if crate::stage_gen::capabilities::boot_media_values_for_device(
            candidate.device,
            &config.devices,
            instances,
        )
        .is_empty()
        {
            return Err(format!(
                "{capability} platform candidate '{}' has no boot-source mapping",
                candidate.device
            ));
        }
    }
    Ok(())
}

fn require_unique_service(
    config: &BoardConfig,
    device_services: &[ServiceSet],
    service: Service,
    capability: &str,
) -> Result<(), String> {
    let mut matches = config
        .devices
        .iter()
        .zip(device_services.iter())
        .filter(|(device, services)| device.enabled && services.contains(service))
        .map(|(device, _)| device.name.as_str());
    let Some(first) = matches.next() else {
        return Err(format!(
            "{capability} requires exactly one enabled {} provider, found none",
            service.as_str()
        ));
    };
    if let Some(second) = matches.next() {
        return Err(format!(
            "{capability} requires exactly one enabled {} provider, found '{first}' and '{second}'",
            service.as_str()
        ));
    }
    Ok(())
}

fn require_device_service(
    config: &BoardConfig,
    device_services: &[ServiceSet],
    device_name: &str,
    service: Service,
    capability: &str,
) -> Result<(), String> {
    let Some(idx) = config
        .devices
        .iter()
        .position(|dev| dev.name.as_str() == device_name)
    else {
        return Err(format!(
            "{capability} references unknown platform candidate '{device_name}'"
        ));
    };
    if device_services
        .get(idx)
        .is_some_and(|services| services.contains(service))
    {
        return Ok(());
    }

    Err(format!(
        "{capability} platform candidate '{device_name}' does not provide {}",
        service.as_str()
    ))
}

fn x86_acpi_requires_mp(config: &BoardConfig) -> bool {
    matches!(
        config.acpi.as_ref().map(|acpi| &acpi.platform),
        Some(fstart_types::acpi::AcpiPlatform::X86)
    )
}
