use super::*;

fn profile() -> Profile {
    named_profile("riscv64-xip")
}

fn named_profile(name: &str) -> Profile {
    let manifest: toml::Value =
        toml::from_str(include_str!("../../../crates/platform-qemu/Cargo.toml")).unwrap();
    manifest["package"]["metadata"]["fstart"]["layouts"][name]
        .clone()
        .try_into()
        .unwrap()
}

pub(crate) fn resolved(payload: &str) -> ResolvedBuild {
    resolved_for("riscv64-xip", payload)
}

pub(crate) fn resolved_for(profile: &str, payload: &str) -> ResolvedBuild {
    ResolvedBuild::resolve(
        named_profile(profile),
        &Overrides::default(),
        Some(payload),
        "platform profile",
    )
    .unwrap()
}

#[test]
fn all_payloads_project_the_same_geometry_to_linker_runtime_and_assembler() {
    for (profile, payload) in [
        ("riscv64-xip", "halt"),
        ("riscv64-xip", "linux"),
        ("riscv64-xip", "uefi"),
        ("armv7-xip", "halt"),
        ("armv7-xip", "linux"),
        ("aarch64-relocate", "halt"),
        ("aarch64-relocate", "linux"),
        ("aarch64-relocate", "uefi"),
    ] {
        let build = resolved_for(profile, payload);
        let encoded = build.descriptor().unwrap();
        let view = fstart_core::layout::Layout::parse(encoded.as_bytes()).unwrap();
        assert_eq!(
            view.region(RegionKind::Firmware),
            Some(build.firmware.region(RegionKind::Firmware))
        );
        assert_eq!(
            view.region(RegionKind::Stack),
            Some(build.stack.region(RegionKind::Stack))
        );
        assert_eq!(
            build.stack.base + build.stack.size,
            build.writable.base + build.writable.size
        );
        assert_eq!(build.heap.base + build.heap.size, build.stack.base);
        let config = build.assembler_config("test-board").unwrap();
        let StageLayout::Monolithic(stage) = config.stages else {
            panic!("wrong stage")
        };
        assert_eq!(stage.load_addr, build.code_reservation().base);
        assert_eq!(
            view.region(RegionKind::Execution),
            build.execution.map(|r| r.region(RegionKind::Execution))
        );
        assert_eq!(stage.stack_size as u64, build.stack.size);
        assert_eq!(stage.heap_size.map(u64::from), Some(build.heap.size));
        assert!(
            matches!(config.build.firmware_image, FirmwareImagePolicy::MemoryMapped { cpu_base, size }
            if cpu_base == build.firmware.base && size == build.firmware.size)
        );
        let script = crate::linker::resolved_xip(&build).unwrap();
        assert!(script.contains(&format!(
            "ORIGIN = {:#x}, LENGTH = {:#x}",
            build.stack.base, build.stack.size
        )));
        assert_eq!(
            build.json().unwrap(),
            resolved_for(profile, payload).json().unwrap()
        );
    }
}

#[test]
fn relocation_requires_a_disjoint_execution_reservation() {
    for placement in [
        None,
        Some(Span {
            base: 0,
            size: 0x1000,
        }),
        Some(Span {
            base: 0x40800000,
            size: 0x1000,
        }),
        Some(Span {
            base: 0x41000000,
            size: 0x1000,
        }),
    ] {
        let mut p = named_profile("aarch64-relocate");
        p.execution = placement;
        assert!(ResolvedBuild::resolve(p, &Overrides::default(), Some("linux"), "test").is_err());
    }
    let mut p = profile();
    p.execution = Some(Span {
        base: 0x90000000,
        size: 0x1000,
    });
    assert!(ResolvedBuild::resolve(p, &Overrides::default(), Some("halt"), "test").is_err());
    assert!(matches!(
        resolved_for("aarch64-relocate", "linux")
            .assembler_config("test")
            .unwrap()
            .payload
            .unwrap()
            .firmware
            .unwrap()
            .kind,
        FirmwareKind::ArmTrustedFirmware
    ));
}

#[test]
fn armv7_requires_representable_addresses_and_direct_linux_without_firmware() {
    let linux = resolved_for("armv7-xip", "linux");
    let config = linux.assembler_config("qemu-armv7").unwrap();
    assert_eq!(config.platform, Platform::Armv7);
    assert!(config.payload.unwrap().firmware.is_none());
    assert!(
        ResolvedBuild::resolve(
            named_profile("armv7-xip"),
            &Overrides::default(),
            Some("uefi"),
            "test"
        )
        .is_err()
    );
    for payload_range in [false, true] {
        let mut p = named_profile("armv7-xip");
        if payload_range {
            p.payloads
                .get_mut("linux")
                .unwrap()
                .kernel
                .as_mut()
                .unwrap()
                .base = 0x1_0000_0000;
        } else {
            p.writable.base = 0x1_0000_0000;
        }
        assert!(
            ResolvedBuild::resolve(p, &Overrides::default(), Some("linux"), "test")
                .unwrap_err()
                .contains("32-bit")
        );
    }
}

#[test]
fn geometry_override_changes_every_projection_and_the_linker_path() {
    let before = resolved("halt");
    let overrides = Overrides {
        heap: Some(before.heap.size * 2),
        firmware_offset: Some(0x0120_0000),
        firmware_capacity: Some(0x00e0_0000),
        ..Default::default()
    };
    let after =
        ResolvedBuild::resolve(profile(), &overrides, Some("halt"), "platform profile").unwrap();
    assert_ne!(
        before.descriptor().unwrap().as_bytes(),
        after.descriptor().unwrap().as_bytes()
    );
    assert_ne!(
        crate::linker::resolved_xip(&before).unwrap(),
        crate::linker::resolved_xip(&after).unwrap()
    );
    assert_ne!(
        before
            .artifact_dir(Path::new("/build"), "test", true)
            .unwrap(),
        after
            .artifact_dir(Path::new("/build"), "test", true)
            .unwrap()
    );
    assert_eq!(after.heap.size, before.heap.size * 2);
    assert_eq!(after.firmware.base, after.flash.base + 0x0120_0000);
    assert!(matches!(
        after.assembler_config("test").unwrap().build.firmware_image,
        FirmwareImagePolicy::MemoryMapped {
            cpu_base: 0x2120_0000,
            size: 0x00e0_0000
        }
    ));
    assert_eq!(
        after.origins["heap"],
        "board:package.metadata.fstart.layout"
    );
}

#[test]
fn rejects_overlaps_overflow_exhausted_budgets_and_unsupported_selections() {
    for (overrides, diagnostic) in [
        (
            Overrides {
                image_capacity: Some(0x0100_0010),
                ..Default::default()
            },
            "partitions overlap",
        ),
        (
            Overrides {
                firmware_offset: Some(u64::MAX),
                ..Default::default()
            },
            "offset overflow",
        ),
        (
            Overrides {
                stack: Some(u64::MAX - 15),
                ..Default::default()
            },
            "exceed writable capacity",
        ),
        (
            Overrides {
                writable_capacity: Some(0x0014_0000),
                ..Default::default()
            },
            "no data/BSS space",
        ),
        (
            Overrides {
                heap: Some(1),
                ..Default::default()
            },
            "16-byte aligned",
        ),
        (
            Overrides {
                firmware_capacity: Some(0),
                ..Default::default()
            },
            "empty layout range",
        ),
    ] {
        let error =
            ResolvedBuild::resolve(profile(), &overrides, Some("halt"), "test").unwrap_err();
        assert!(error.contains(diagnostic), "{error}");
    }
    assert!(ResolvedBuild::resolve(profile(), &Overrides::default(), Some("fit"), "test").is_err());
    let mut p = profile();
    p.entry = "guess-from-address".into();
    assert!(ResolvedBuild::resolve(p, &Overrides::default(), None, "test").is_err());
    let mut p = profile();
    p.payloads.get_mut("linux").unwrap().kernel = Some(p.writable);
    assert!(
        ResolvedBuild::resolve(p, &Overrides::default(), None, "test")
            .unwrap_err()
            .contains("overlap")
    );
}

#[test]
fn feature_references_must_be_actual_direct_dependency_features() {
    let board = serde_json::json!({"id":"board", "features":{"stage":[]}});
    let metadata = serde_json::json!({
        "packages":[{"id":"platform", "features":{"riscv64":[]}}],
        "resolve":{"nodes":[{"id":"board", "deps":[{"name":"renamed_platform", "pkg":"platform"}]}]}
    });
    assert!(
        validate_features(
            &["stage".into(), "renamed-platform/riscv64".into()],
            &board,
            &metadata
        )
        .is_ok()
    );
    for feature in [
        "missing",
        "transitive-driver/riscv64",
        "renamed-platform/missing",
    ] {
        assert!(validate_features(&[feature.into()], &board, &metadata).is_err());
    }
}
