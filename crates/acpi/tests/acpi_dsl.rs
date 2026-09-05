//! Byte-emitting `acpi_dsl!` integration and ACPICA round-trip tests.

use fstart_acpi::{AmlFragment, FixupKind};
use fstart_acpi_macros::acpi_dsl;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

fn dsdt(aml: &[u8]) -> Vec<u8> {
    let mut table = vec![0u8; 36];
    table[0..4].copy_from_slice(b"DSDT");
    table[4..8].copy_from_slice(&(36u32 + aml.len() as u32).to_le_bytes());
    table[8] = 2;
    table[10..16].copy_from_slice(b"FSTART");
    table[16..24].copy_from_slice(b"DSLTEST_");
    table[24..28].copy_from_slice(&1u32.to_le_bytes());
    table[28..32].copy_from_slice(b"FST0");
    table[32..36].copy_from_slice(&1u32.to_le_bytes());
    table.extend_from_slice(aml);
    table[9] = 0u8.wrapping_sub(table.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)));
    table
}

fn run_iasl(dir: &Path, args: &[&str]) {
    let output = Command::new("iasl")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "iasl {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if args.contains(&"-tc") {
        assert!(
            diagnostics.contains("0 Errors") && !diagnostics.contains("Compilation failed"),
            "iasl reported an error:\n{diagnostics}"
        );
    }
}

fn compile_asl(name: &str, body: &str) -> Vec<u8> {
    let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "fstart-acpi-asl-{name}-{}-{id}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(
        dir.join("reference.dsl"),
        format!(
            "DefinitionBlock (\"\", \"DSDT\", 2, \"FSTART\", \"DSLTEST_\", 1)\n{{\n{body}\n}}\n"
        ),
    )
    .unwrap();
    run_iasl(&dir, &["-tc", "reference.dsl"]);
    let bytes = fs::read(dir.join("reference.aml")).unwrap();
    fs::remove_dir_all(dir).unwrap();
    bytes[36..].to_vec()
}

fn round_trip(name: &str, aml: &[u8]) {
    let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("fstart-acpi-{name}-{}-{id}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("input.aml"), dsdt(aml)).unwrap();
    run_iasl(&dir, &["-d", "input.aml"]);
    fs::rename(dir.join("input.aml"), dir.join("original.aml")).unwrap();
    run_iasl(&dir, &["-tc", "input.dsl"]);
    let rebuilt = fs::read(dir.join("input.aml")).unwrap();
    assert_eq!(&rebuilt[36..], aml, "iasl changed macro-generated AML");
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn fragment_is_const_and_runtime_operands_are_fixed_width() {
    const LITERAL: AmlFragment<28, 0> = acpi_dsl! {
        Device("COM0") {
            Name("_HID", "ARMH0011");
            Name("_UID", 0u32);
        }
    };
    assert_eq!(&LITERAL.as_bytes()[..2], &[0x5b, 0x82]);

    const BASE: u32 = 0x1234;
    const CONST_FRAGMENT: AmlFragment<10, 0> = acpi_dsl! {
        Name("CST", #{const dword BASE + 1});
    };
    assert!(
        CONST_FRAGMENT
            .as_bytes()
            .ends_with(&0x1235u32.to_le_bytes())
    );

    let const_fragment = acpi_dsl! {
        Name("CST", #{const 0x1234u16});
        Scope(#{const "^PARN"}) { Name("VAL", 1u8); }
        Method("MCAL", 0, NotSerialized) { HKEY.RHK_(1u8); }
    };
    assert!(
        const_fragment
            .as_bytes()
            .windows(4)
            .any(|bytes| bytes == b"PARN")
    );
    assert!(
        const_fragment
            .as_bytes()
            .windows(4)
            .any(|bytes| bytes == b"RHK_")
    );

    let byte = 0x12u8;
    let word = 0x3456u16;
    let dword = 0x789a_bcdeu32;
    let qword = 0x0123_4567_89ab_cdefu64;
    let fragment = acpi_dsl! {
        Name("BVAL", #{byte byte});
        Name("WVAL", #{word word});
        Name("DVAL", #{dword dword});
        Name("QVAL", #{qword qword});
    };
    assert_eq!(
        fragment.fragment().fixups().map(|fixup| fixup.kind()),
        [
            FixupKind::Byte,
            FixupKind::Word,
            FixupKind::DWord,
            FixupKind::QWord,
        ]
    );
    let mut bytes = Vec::new();
    fragment.emit(&mut bytes).unwrap();
    for (fixup, expected) in fragment.fragment().fixups().iter().zip([
        &[0x12][..],
        &[0x56, 0x34][..],
        &[0xde, 0xbc, 0x9a, 0x78][..],
        &[0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01][..],
    ]) {
        assert_eq!(
            &bytes[fixup.offset()..fixup.offset() + expected.len()],
            expected
        );
    }
    round_trip("operands", &bytes);
}

#[test]
fn runtime_expressions_are_evaluated_once_in_source_order() {
    let values = [0x12u64, 0x3456, 0x789a_bcde, 0x0123_4567_89ab_cdef];
    let mut index = 0;
    let fragment = acpi_dsl! {
        Name("BVAL", #{byte { let value = values[index]; index += 1; value }});
        Name("WVAL", #{word { let value = values[index]; index += 1; value }});
        Name("DVAL", #{dword { let value = values[index]; index += 1; value }});
        Name("QVAL", #{qword { let value = values[index]; index += 1; value }});
    };
    assert_eq!(index, values.len());
    assert_eq!(fragment.operands().unwrap(), &values);
}

#[test]
fn runtime_operand_overflow_is_rejected() {
    let too_large = 0x100u16;
    let fragment = acpi_dsl! { Name("OVFL", #{byte too_large}); };
    assert_eq!(
        fragment.emit(&mut Vec::new()),
        Err(fstart_acpi::AmlError::InvalidOperand)
    );
}

#[test]
fn remaining_operation_and_resource_families_encode() {
    let fragment = acpi_dsl! {
        Device("DEV0") { Name("_HID", "FST0003"); }
        Name("BUF0", Buffer(0u8, 1u8, 2u8, 3u8));
        Name("RSC0", ResourceTemplate {
            IRQ(Edge, ActiveHigh, Exclusive, 7u8);
            DWordIO(0x1000u32, 0x1fffu32);
            WordBusNumber(0u16, 0xffu16);
            DWordMemory(Cacheable, ReadWrite, 0x80000000u32, 0x8fffffffu32);
            QWordMemory(Prefetchable, ReadWrite, 0x100000000u64, 0x10fffffffu64);
        });
        Method("OPS0", 2, NotSerialized) {
            CreateDwordField(BUF0, 0u8, "FLD0");
            Store(Arg0, Local0);
            ShiftLeft(Local1, Local0, 1u8);
            Subtract(Local2, Local1, Arg1);
            Add(Local3, Local2, 1u8);
            Local4 = DeRefOf(Index(BUF0, 0u8));
            If (CondRefOf(FLD0, Local5)) { Increment(Local5); }
            Notify(DEV0, 0x80u8);
            Return(SizeOf(BUF0));
        }
    };
    for opcode in [0x8a, 0x70, 0x79, 0x74, 0x72, 0x83, 0x88, 0x86, 0x87] {
        assert!(
            fragment.as_bytes().contains(&opcode),
            "missing opcode {opcode:#x}"
        );
    }
    for descriptor in [0x23, 0x87, 0x88, 0x8a] {
        assert!(
            fragment.as_bytes().contains(&descriptor),
            "missing descriptor {descriptor:#x}"
        );
    }
    round_trip("operations", fragment.as_bytes());
}

#[test]
fn runtime_resource_ranges_and_buffer_operands_are_linked() {
    let buffer_byte = 0x12u8;
    let buffer_word = 0x3456u16;
    let buffer_dword = 0x789a_bcdeu32;
    let buffer_qword = 0x0123_4567_89ab_cdefu64;
    let dword_min = 0x8000_0000u32;
    let dword_max = 0x8fff_ffffu32;
    let qword_min = 0x1_0000_0000u64;
    let qword_max = 0x1_0fff_ffffu64;
    let fragment = acpi_dsl! {
        Name("BUF0", Buffer(
            #{byte buffer_byte}, #{word buffer_word},
            #{dword buffer_dword}, #{qword buffer_qword}
        ));
        Name("RSC0", ResourceTemplate {
            DWordMemory(NotCacheable, ReadWrite, #{dword dword_min}, #{dword dword_max});
            QWordMemory(Prefetchable, ReadWrite, #{qword qword_min}, #{qword qword_max});
        });
    };
    let mut bytes = Vec::new();
    fragment.emit(&mut bytes).unwrap();
    assert!(
        bytes
            .windows(4)
            .any(|window| window == 0x1000_0000u32.to_le_bytes())
    );
    assert!(
        bytes
            .windows(8)
            .any(|window| window == 0x1000_0000u64.to_le_bytes())
    );

    let reference = compile_asl(
        "runtime-resources",
        r#"
        Name (BUF0, Buffer (0x0F) {
            0x12, 0x56, 0x34, 0xDE, 0xBC, 0x9A, 0x78,
            0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01
        })
        Name (RSC0, ResourceTemplate () {
            DWordMemory (ResourceProducer, PosDecode, MinFixed, MaxFixed,
                NonCacheable, ReadWrite, 0, 0x80000000, 0x8FFFFFFF, 0,
                0x10000000)
            QWordMemory (ResourceProducer, PosDecode, MinFixed, MaxFixed,
                Prefetchable, ReadWrite, 0, 0x100000000, 0x10FFFFFFF, 0,
                0x10000000)
        })
        "#,
    );
    assert_eq!(
        bytes, reference,
        "resource descriptors differ from reference ASL"
    );
    round_trip("runtime-resources", &bytes);
}

#[test]
fn device_resources_region_field_and_methods_round_trip() {
    let aml = acpi_dsl! {
    Device("DEV0") {
        Name("_HID", EisaId("PNP0501"));
            Name("_UID", 1u32);
            Name("PKG", Package(0u8, 1u8, 0x1234u16, "text"));
            Name("BUF", Buffer(0x10u8, 0x20u8, 0x30u8));
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadWrite, 0x60000000u32, 0x1000u32);
                Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, 33u32);
                IRQNoFlags(5u8);
                IO(0x03f8u16, 0x03f8u16, 1u8, 8u8);
            });
            OperationRegion("REGN", SystemMemory, 0x60000000u32, 0x100u32);
            Field("REGN", DWordAcc, NoLock, Preserve) {
                Offset(0x04),
                FLD0, 8,
                , 8,
                FLD1, 16,
            }
            Method("TEST", 1, NotSerialized) {
                Local0 = Arg0 + 1u32;
                If (Local0 > 2u32) { Local1 = Local0; }
                Else { Local1 = Zero; }
                While (Local1 > Zero) { Local1--; Break; }
                Return(Local1);
            }
        }
    };
    round_trip("device", aml.as_bytes());
}

#[test]
fn containers_synchronization_and_relative_names_round_trip() {
    let fragment = acpi_dsl! {
        Scope("\\") { Name("ROOT", 9u8); }
        Mutex("MTX0", 0u8);
        ThermalZone("THM0") {
                Name("_TZP", 100u32);
                Method("_TMP", 0, Serialized) {
                    Acquire(MTX0, 0xffffu16);
                    Sleep(1u32);
                    Stall(1u32);
                    Release(MTX0);
                    Return(3000u32);
                }
            }
            PowerResource("PWR0", 0u8, 0u16) {
                Method("_ON", 0, NotSerialized) { Return(Zero); }
                Method("_OFF", 0, NotSerialized) { Return(Zero); }
            }
        Device("PCI0") {
                Name("_HID", EisaId("PNP0A08"));
                Device("PARN") {
                    Name("_HID", "FST0001");
                    Name("FOO", 42u8);
                }
                Scope("PARN") { Name("BAR", 7u8); }
                Device("CHLD") {
                    Name("_HID", "FST0002");
                    Method("CALL", 0, NotSerialized) { Return(^^PARN.FOO); }
                }
            }
    };
    round_trip("containers", fragment.as_bytes());
}
