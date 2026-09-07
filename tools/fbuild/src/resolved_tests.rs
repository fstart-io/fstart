use super::*;

fn profile() -> Profile {
    let manifest: toml::Value =
        toml::from_str(include_str!("../../../crates/platform-qemu/Cargo.toml")).unwrap();
    manifest["package"]["metadata"]["fstart"]["layouts"]["riscv64-xip"]
        .clone()
        .try_into()
        .unwrap()
}

pub(crate) fn resolved(payload: &str) -> ResolvedBuild {
    ResolvedBuild::resolve(
        profile(),
        &Overrides::default(),
        Some(payload),
        "platform profile",
    )
    .unwrap()
}

#[test]
fn all_payloads_project_the_same_geometry_to_linker_runtime_and_assembler() {
    for payload in ["halt", "linux", "uefi"] {
        let build = resolved(payload);
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
        assert_eq!(stage.load_addr, build.image.base);
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
        assert_eq!(build.json().unwrap(), resolved(payload).json().unwrap());
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
