//! Synthetic namespace/behavior checks with ACPICA's simulated SystemMemory
//! handler. These do not validate real EC hardware, SMM or suspend semantics.
use fstart_acpi::aml_linker::AmlWriter;
use fstart_acpi_macros::acpi_dsl;
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
    assert!(output.status.success(), "{tool} failed: {text}");
    assert!(
        !text.contains("AE_")
            && !text.contains("ACPI Error")
            && !text.contains("Compilation failed"),
        "{text}"
    );
    text
}

#[test]
fn complete_table_set_resolves_external_calls_and_reads_live_region_state() {
    let dir = std::env::temp_dir().join(format!("fstart-acpi-behavior-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    // This fragment requires parent \\_SB_, exports EC00.TEMP (arity 0),
    // SETT (arity 1) and _LID (arity 0); all field references are internal.
    let fragment = acpi_dsl! {
        Device("EC00") {
            Name("_HID", "FST0001");
            Name("EVCT", 0u8);
            Name("_CRS", ResourceTemplate { Memory32Fixed(ReadWrite, 0x1000u32, 0x100u32); });
            OperationRegion("REGN", SystemMemory, 0x1000u32, 0x100u32);
            Field("REGN", ByteAcc, NoLock, Preserve) { TRAW, 16, LIDS, 8, }
            Method("SETT", 1, Serialized) { Store(Arg0, TRAW); }
            Method("SLID", 1, Serialized) { Store(Arg0, LIDS); }
            Method("TEMP", 0, Serialized) { Return(TRAW + 2732u32); }
            Method("_LID", 0, Serialized) { Return(LIDS); }
            Method("_Q42", 0, Serialized) { Increment(EVCT); Notify(EC00, 0x80u8); }
        }
    };
    let mut storage = [0; 1024];
    let table = AmlWriter::table(
        &mut storage,
        *b"DSDT",
        2,
        *b"FSTART",
        *b"BEHAVIOR",
        1,
        |writer| writer.scope("\\_SB_", |writer| writer.emit(&fragment, &[])),
    )
    .unwrap();
    fs::write(dir.join("dsdt.aml"), table.as_slice().unwrap()).unwrap();
    // Independent ASL supplies a cross-table consumer with a rooted reference
    // and a zero-argument call. No namespace search redirects this call.
    fs::write(
        dir.join("ssdt.dsl"),
        r#"
DefinitionBlock ("", "SSDT", 2, "FSTART", "EXTTEST_", 1) {
    External (\_SB.EC00.TEMP, MethodObj)
    Scope (\_SB) {
        ThermalZone (THM0) {
            Method (_TMP, 0, Serialized) { Return (\_SB.EC00.TEMP ()) }
        }
    }
}
"#,
    )
    .unwrap();
    let compiled = run(&dir, "iasl", &["-tc", "ssdt.dsl"]);
    assert!(compiled.contains("0 Errors"), "{compiled}");
    // Disassemble the set together, then compile each disassembly. Compiler
    // header normalization and noncanonical integer widths need not be equal.
    run(&dir, "iasl", &["-e", "ssdt.aml", "-d", "dsdt.aml"]);
    run(&dir, "iasl", &["-e", "dsdt.aml", "-d", "ssdt.aml"]);
    for file in ["dsdt.dsl", "ssdt.dsl"] {
        let compiled = run(&dir, "iasl", &["-tc", file]);
        assert!(compiled.contains("0 Errors"), "{compiled}");
    }
    // Use the original linker output for execution, not the recompiled DSDT.
    fs::write(dir.join("dsdt.aml"), table.as_slice().unwrap()).unwrap();
    // Disable ACPICA's own allocation-tracking diagnostics: this checks AML
    // behavior, not the interpreter's cache accounting at process shutdown.
    let output = run(
        &dir,
        "acpiexec",
        &[
            "-dt",
            "-fv",
            "0",
            "-b",
            "execute \\_SB.EC00.SETT 0x12C;execute \\_SB.THM0._TMP;execute \\_SB.EC00.SLID 1;execute \\_SB.EC00._LID;execute \\_SB.EC00.SLID 0;execute \\_SB.EC00._LID;execute \\_SB.EC00._Q42;execute \\_SB.EC00.EVCT;execute \\_SB.EC00._CRS",
            "dsdt.aml",
            "ssdt.aml",
        ],
    );
    assert!(
        output.contains("Received a Device Notify") && output.contains("Value 0x80"),
        "notification missing: {output}"
    );
    assert!(
        output.contains("0000000000000BD8"),
        "thermal return missing: {output}"
    ); // 300 + 2732
    assert!(
        output.matches("[Integer] = 0000000000000001").count() >= 2,
        "lid/event return missing: {output}"
    );
    assert!(
        output.contains("[Integer] = 0000000000000000"),
        "live lid change missing: {output}"
    );
    assert!(
        output.contains("86 09 00 01 00 10 00 00 00 01 00 00 79 00"),
        "resource buffer missing: {output}"
    );
    fs::remove_dir_all(dir).unwrap();
}
