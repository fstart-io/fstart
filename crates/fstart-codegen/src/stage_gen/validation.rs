//! Capability ordering validation and board predicate helpers.
//!
//! Ensures capabilities are declared in a legal order (e.g., ConsoleInit
//! before anything that logs) and provides predicate functions used by the
//! codegen orchestrator to decide which sections to emit.

use fstart_device_registry::{DriverInstance, Service, ServiceSet};

use fstart_types::{
    BoardConfig, BootMedium, Capability, FitParseMode, PayloadKind, Platform, StageLayout,
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
            Capability::MemoryInit => {
                memory_ready = true;
            }
            Capability::DramInit { .. } if !console_inited => {
                return Some(
                    "DramInit capability requires ConsoleInit to appear earlier \
                     in the capability list (needed for logging)"
                        .to_string(),
                );
            }
            Capability::DramInit { .. } => {
                memory_ready = true;
            }
            Capability::PreConsoleInit { .. } => {
                // Pre-console phases must be log-free and may run before the
                // logger exists.
            }
            Capability::EarlyInit { .. }
            | Capability::StageLocalInit { .. }
            | Capability::PostDramInit { .. }
            | Capability::FinalizeInit { .. }
                if !console_inited =>
            {
                return Some(
                    "EarlyInit/StageLocalInit/PostDramInit/FinalizeInit require ConsoleInit to \
                     appear earlier in the capability list (needed for logging)"
                        .to_string(),
                );
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
            Capability::MpInit {
                smm: true,
                smm_provider: Some(provider),
                ..
            } if !config
                .devices
                .iter()
                .zip(device_services.iter())
                .any(|(dev, services)| {
                    dev.name.as_str() == provider.as_str() && services.contains(Service::SmmOps)
                }) =>
            {
                return Some(format!(
                    "MpInit(smm: true, smm_provider: {:?}) requires that device to provide \
                     SmmOps",
                    provider.as_str()
                ));
            }
            Capability::MpInit {
                smm: true,
                smm_provider: None,
                ..
            } if device_services
                .iter()
                .filter(|services| services.contains(Service::SmmOps))
                .count()
                != 1 =>
            {
                return Some(
                    "MpInit(smm: true) without smm_provider requires exactly one device \
                     that provides SmmOps"
                        .to_string(),
                );
            }
            Capability::MpInit {
                smm: true,
                num_cpus,
                ..
            } if config
                .smm
                .and_then(|s| s.entry_points)
                .is_some_and(|entries| entries < *num_cpus) =>
            {
                return Some(
                    "board.smm.entry_points must be greater than or equal to MpInit.num_cpus"
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
            Capability::MpInit { .. } => {}
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

fn validate_capability_service(
    cap: &Capability,
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_services: &[ServiceSet],
) -> Result<(), String> {
    match cap {
        Capability::ClockInit { device } => require_device_service(
            config,
            device_services,
            device.as_str(),
            Service::ClockController,
            "ClockInit",
        ),
        Capability::ConsoleInit { device } => require_device_service(
            config,
            device_services,
            device.as_str(),
            Service::Console,
            "ConsoleInit",
        ),
        Capability::BootMedia(BootMedium::FirmwareImage { provider, .. }) => {
            validate_firmware_image_provider(
                provider.as_ref().map(|p| p.as_str()),
                config,
                instances,
                device_services,
            )
        }
        Capability::DramInit { device } => require_device_service(
            config,
            device_services,
            device.as_str(),
            Service::MemoryController,
            "DramInit",
        ),
        Capability::PreConsoleInit { devices } => require_devices_service(
            config,
            device_services,
            devices,
            Service::PreConsoleInit,
            "PreConsoleInit",
        ),
        Capability::EarlyInit { devices } => require_devices_service(
            config,
            device_services,
            devices,
            Service::EarlyInit,
            "EarlyInit",
        ),
        Capability::StageLocalInit { devices } => require_devices_service(
            config,
            device_services,
            devices,
            Service::StageLocalInit,
            "StageLocalInit",
        ),
        Capability::PostDramInit { devices } => require_devices_service(
            config,
            device_services,
            devices,
            Service::PostDramInit,
            "PostDramInit",
        ),
        Capability::FinalizeInit { devices } => require_devices_service(
            config,
            device_services,
            devices,
            Service::FinalizeInit,
            "FinalizeInit",
        ),
        Capability::PciInit { device } => require_device_service(
            config,
            device_services,
            device.as_str(),
            Service::PciRootBus,
            "PciInit",
        ),
        Capability::AcpiLoad { device } => require_device_service(
            config,
            device_services,
            device.as_str(),
            Service::AcpiTableProvider,
            "AcpiLoad",
        ),
        Capability::MemoryDetect { device } => require_device_service(
            config,
            device_services,
            device.as_str(),
            Service::MemoryDetector,
            "MemoryDetect",
        ),
        Capability::LoadNextStage { devices, .. } => {
            for device in devices {
                require_device_service(
                    config,
                    device_services,
                    device.name.as_str(),
                    Service::BlockDevice,
                    "LoadNextStage",
                )?;
                if crate::stage_gen::capabilities::boot_media_values_for_device(
                    device.name.as_str(),
                    &config.devices,
                    instances,
                )
                .is_empty()
                {
                    return Err(format!(
                        "LoadNextStage device '{}' has no boot-source mapping",
                        device.name.as_str()
                    ));
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn require_devices_service(
    config: &BoardConfig,
    device_services: &[ServiceSet],
    devices: &[heapless::String<32>],
    service: Service,
    capability: &str,
) -> Result<(), String> {
    for device in devices {
        require_device_service(
            config,
            device_services,
            device.as_str(),
            service,
            capability,
        )?;
    }
    Ok(())
}

fn validate_firmware_image_provider(
    provider: Option<&str>,
    config: &BoardConfig,
    instances: &[DriverInstance],
    device_services: &[ServiceSet],
) -> Result<(), String> {
    if let Some(provider) = provider {
        return require_device_service(
            config,
            device_services,
            provider,
            Service::FirmwareImageProvider,
            "BootMedia(FirmwareImage)",
        );
    }

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
        let candidates = fstart_device_registry::platform_boot_media_candidates(
            config.name.as_str(),
            config.platform,
        );
        if !candidates.is_empty() {
            for candidate in candidates {
                require_device_service(
                    config,
                    device_services,
                    candidate.device,
                    Service::BlockDevice,
                    "BootMedia(FirmwareImage)",
                )?;
                if crate::stage_gen::capabilities::boot_media_values_for_device(
                    candidate.device,
                    &config.devices,
                    instances,
                )
                .is_empty()
                {
                    return Err(format!(
                        "BootMedia(FirmwareImage) platform candidate '{}' has no boot-source mapping",
                        candidate.device
                    ));
                }
            }
            return Ok(());
        }
        return Err(
            "BootMedia(FirmwareImage) requires a device that provides FirmwareImageProvider, Rust platform firmware-image support, or Rust platform boot-source candidates"
                .to_string(),
        );
    };
    if let Some(second) = providers.next() {
        return Err(format!(
            "BootMedia(FirmwareImage) has multiple providers ('{first}', '{second}', ...); set provider"
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
            "{capability} references unknown device '{device_name}'"
        ));
    };
    if device_services
        .get(idx)
        .is_some_and(|services| services.contains(service))
    {
        return Ok(());
    }

    Err(format!(
        "{capability} references device '{device_name}', but that device does not provide {}",
        service.as_str()
    ))
}
