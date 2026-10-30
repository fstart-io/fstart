//! Execute the generated video extension with the real ACPICA interpreter.
//! No physical memory handler or graphics hardware is needed: ROM bytes belong
//! to AML, not a firmware pointer that an OS could reclaim.
use fstart_acpi::{acpi_dsl, aml_linker::AmlWriter, video::rom_aml};
use std::{fs, path::Path, process::Command};

fn run(dir: &Path, tool: &str, args: &[&str]) -> String {
    let output = Command::new(tool)
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.status.success() && !text.contains("AE_") && !text.contains("ACPI Error"),
        "{tool}: {text}"
    );
    text
}

#[test]
fn rom_reads_are_bounded_padded_repeatable_and_per_device() {
    let dir = std::env::temp_dir().join(format!("fstart-video-rom-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let mut storage = [0u8; 32768];
    let lengths = [37usize, 8205]; // Small, unaligned images; neither needs 4-KiB padding.
    let table = AmlWriter::table(
        &mut storage,
        *b"DSDT",
        2,
        *b"FSTART",
        *b"VIDEOROM",
        1,
        |writer| {
            writer.scope("\\_SB_", |writer| {
                writer.device("PCI0", |writer| {
                    writer.emit(&acpi_dsl! { Name("_HID", EisaId("PNP0A08")); }, &[])?;
                    for (gpu, &length) in lengths.iter().enumerate() {
                        writer.device(&format!("GFX{gpu}"), |writer| {
                            let adr = ((gpu as u32) + 1) << 16;
                            acpi_dsl! { Name("_ADR", #{dword adr}); }.emit(writer)?;
                            let rom: Vec<u8> = (0..length)
                                .map(|i| ((i + gpu * 37) % 251 + 1) as u8)
                                .collect();
                            writer.raw(&rom_aml(&rom)?) // The source is dropped before the table is executed.
                        })?;
                    }
                    Ok(())
                })
            })
        },
    )
    .unwrap();
    let original = table.as_slice().unwrap().to_vec();
    fs::write(dir.join("dsdt.aml"), &original).unwrap();
    let mut asl =
        String::from("DefinitionBlock (\"\", \"SSDT\", 2, \"FSTART\", \"ROMTEST_\", 1) {\n");
    let mut commands = Vec::new();
    for (gpu, &length) in lengths.iter().enumerate() {
        asl += &format!("External (\\_SB.PCI0.GFX{gpu}._ROM, MethodObj)\n");
        let cases = [
            (0, 16),
            (11, 17),
            (4090, 16),
            (length as u64 - 3, 16),
            (length as u64, 16),
            (length as u64 + 123, 16),
            (u64::MAX, 16),
            (0, 0),
            (0, 4097),
            (0, u64::MAX),
            (length as u64 - 1, 1),
            (length as u64 - 1, 2),
            (length as u64 - 7, 7),
        ];
        for (case, (offset, request)) in cases.into_iter().enumerate() {
            let name = format!("T{gpu}{case:02X}");
            commands.push(format!("execute {name}"));
            let expected_length = request.min(4096);
            // A wholly out-of-range offset must not wrap when adding the index.
            let expected = if offset >= length as u64 {
                String::new()
            } else {
                format!(
                    "If (Local2 < {length}) {{ Local3 = Mod (Local2 + {}, 251) + 1 }}",
                    gpu * 37
                )
            };
            asl += &format!(
                r#"
Method ({name}, 0, Serialized) {{
    Store (\_SB.PCI0.GFX{gpu}._ROM (0x{offset:X}, 0x{request:X}), Local0)
    If (SizeOf (Local0) != {expected_length}) {{ Return (Zero) }}
    Local1 = Zero
    While (Local1 < {expected_length}) {{
        Local2 = 0x{offset:X} + Local1
        Local3 = Zero
        {expected}
        If (DerefOf (Index (Local0, Local1)) != Local3) {{ Return (Zero) }}
        Increment (Local1)
    }}
    Return (One)
}}
"#
            );
        }
        // Mutating a returned buffer must neither change VDAT nor poison the
        // next call's output. The last call requested a short (integer-sized)
        // field; the following one requests a real buffer again.
        let name = format!("MUT{gpu}");
        commands.push(format!("execute {name}"));
        let first = gpu * 37 + 1;
        asl += &format!(
            r#"
Method ({name}, 0, Serialized) {{
    Local0 = \_SB.PCI0.GFX{gpu}._ROM (0, 16)
    Store (Zero, Index (Local0, Zero))
    Local1 = \_SB.PCI0.GFX{gpu}._ROM (0, 16)
    If (DerefOf (Index (Local1, Zero)) == {first}) {{ Return (One) }}
    Return (Zero)
}}
"#
        );
    }
    asl += "}\n";
    fs::write(dir.join("tests.dsl"), asl).unwrap();
    let compiled = run(&dir, "iasl", &["-tc", "tests.dsl"]);
    assert!(compiled.contains("0 Errors"), "{compiled}");
    // Also independently disassemble and compile the generated implementation.
    run(&dir, "iasl", &["-d", "dsdt.aml"]);
    let compiled = run(&dir, "iasl", &["-tc", "dsdt.dsl"]);
    assert!(compiled.contains("0 Errors"), "{compiled}");
    fs::write(dir.join("dsdt.aml"), original).unwrap();
    let output = run(
        &dir,
        "acpiexec",
        &["-dt", "-b", &commands.join(";"), "dsdt.aml", "tests.aml"],
    );
    assert_eq!(
        output.matches("[Integer] = 0000000000000001").count(),
        commands.len(),
        "{output}"
    );
    fs::remove_dir_all(dir).unwrap();
}
