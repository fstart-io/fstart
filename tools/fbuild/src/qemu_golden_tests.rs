//! Output snapshots captured from the pre-removal metadata resolver, with the
//! intentional phase2a RISC-V change to one packed 32-MiB bank applied explicitly.
//! ARM retains its original two-bank geometry.
//! Linker text uses a digest to avoid repeating hundreds of descriptor BYTE lines;
//! the wire bytes, ELF expectations and assembly remain directly inspectable.
use fstart_image_build::{build_plan::UnitOutput, plan::BuildSelection};
use fstart_platform_qemu::facts::VirtMachine;
use sha2::{Digest, Sha256};

#[test]
fn typed_qemu_presets_match_captured_xip_outputs() {
    for (machine, payload, fixture) in [
        (
            VirtMachine::Riscv64,
            "halt",
            include_str!("fixtures/qemu-xip/riscv64-xip-halt.json"),
        ),
        (
            VirtMachine::Riscv64,
            "linux",
            include_str!("fixtures/qemu-xip/riscv64-xip-linux.json"),
        ),
        (
            VirtMachine::Riscv64,
            "uefi",
            include_str!("fixtures/qemu-xip/riscv64-xip-uefi.json"),
        ),
        (
            VirtMachine::Armv7,
            "halt",
            include_str!("fixtures/qemu-xip/armv7-xip-halt.json"),
        ),
        (
            VirtMachine::Armv7,
            "linux",
            include_str!("fixtures/qemu-xip/armv7-xip-linux.json"),
        ),
    ] {
        let expected: serde_json::Value = serde_json::from_str(fixture).unwrap();
        let plan = fstart_platform_qemu::host::resolve(
            machine,
            BuildSelection {
                payload: Some(payload.into()),
            },
        )
        .unwrap();
        let unit = &plan.units[0];
        let UnitOutput::Executable { expectations, .. } = &unit.output else {
            panic!("not executable")
        };
        let mut actual = serde_json::json!({
            "descriptor_hex": expectations.descriptor.bytes.iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "linker_sha256": format!("{:x}", Sha256::digest(unit.linker_script.as_ref().unwrap().as_bytes())),
            "elf": expectations,
            "assembly": plan.assembly.config("fixture").unwrap(),
            "target": unit.target, "entry": unit.entry, "environment": unit.environment,
        });
        // The exact wire bytes are recorded once, in descriptor_hex.
        actual["elf"]["descriptor"]
            .as_object_mut()
            .unwrap()
            .remove("bytes");
        assert_eq!(actual, expected, "{machine:?}/{payload}");
    }
}
