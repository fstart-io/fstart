use fstart_acpi::aml_linker::{AmlPath, AmlWriter, package_length, scope_vec};
use fstart_acpi::{AmlError, AmlFragment, Fixup, FixupKind};
use fstart_acpi_macros::acpi_dsl;

#[test]
fn runtime_conversion_preserves_sign_and_high_bits() {
    for value in [-1i128, i128::from(u64::MAX) + 1] {
        let fragment = acpi_dsl! { Name("TEST", #{qword value}); };
        let mut bytes = vec![0xaa];
        assert_eq!(fragment.emit(&mut bytes), Err(AmlError::InvalidOperand));
        assert_eq!(bytes, [0xaa]);
    }
    let value = u128::from(u64::MAX) + 1;
    assert_eq!(
        acpi_dsl! { Name("TEST", #{qword value}); }.emit(&mut Vec::new()),
        Err(AmlError::InvalidOperand)
    );
    for value in [0i128, 1, i128::from(u64::MAX)] {
        let fragment = acpi_dsl! { Name("TEST", #{qword value}); };
        let mut bytes = Vec::new();
        fragment.emit(&mut bytes).unwrap();
        assert_eq!(&bytes[6..], &u64::try_from(value).unwrap().to_le_bytes());
    }
}

#[test]
fn every_operand_width_is_checked_before_appending() {
    for (kind, maximum) in [
        (FixupKind::Byte, 255),
        (FixupKind::Word, 65535),
        (FixupKind::DWord, u32::MAX as u64),
    ] {
        let fragment = AmlFragment::new([0; 8], [Fixup::raw(0, kind)]);
        let mut bytes = vec![0xcc];
        assert_eq!(
            fragment.emit(&mut bytes, &[maximum + 1]),
            Err(AmlError::InvalidOperand)
        );
        assert_eq!(bytes, [0xcc]);
        fragment.emit(&mut bytes, &[maximum]).unwrap();
    }
}

#[test]
fn resource_ranges_validate_before_any_write() {
    for (min, max) in [(5u64, 4u64), (0, u64::MAX)] {
        let fragment = acpi_dsl! {
            Name("RSC0", ResourceTemplate {
                QWordMemory(NotCacheable, ReadWrite, #{qword min}, #{qword max});
            });
        };
        let mut bytes = vec![0xaa];
        assert_eq!(fragment.emit(&mut bytes), Err(AmlError::InvalidRange));
        assert_eq!(bytes, [0xaa]);
    }
    let max = u32::MAX;
    let fragment = acpi_dsl! {
        Name("RSC0", ResourceTemplate {
            DWordMemory(NotCacheable, ReadWrite, 0u32, #{dword max});
        });
    };
    assert_eq!(fragment.emit(&mut Vec::new()), Err(AmlError::InvalidRange));
    let min = 0x1_0000_0000u64;
    let fragment = acpi_dsl! {
        Name("RSC0", ResourceTemplate {
            QWordMemory(NotCacheable, ReadWrite, #{qword min}, 0x1_0000_0000u64);
        });
    };
    let mut bytes = Vec::new();
    fragment.emit(&mut bytes).unwrap();
    // EndTag's zero checksum means "checksum not used", not stale checksum.
    assert_eq!(&bytes[bytes.len() - 2..], &[0x79, 0]);
    assert_eq!(
        &bytes[bytes.len() - 10..bytes.len() - 2],
        &1u64.to_le_bytes()
    );
}

#[test]
fn paths_are_validated_consistently_and_preserve_prefixes() {
    for path in [
        "", ".", "A.", ".A", "A..B", "lower", "ABCDE", "1ABC", "\\^A", "A.^B", "é",
    ] {
        assert_eq!(AmlPath::new(path), Err(AmlError::InvalidPath), "{path}");
        assert_eq!(scope_vec(path, &[]), Err(AmlError::InvalidPath));
    }
    assert!(AmlPath::new(&vec!["A"; 255].join(".")).is_ok());
    assert!(AmlPath::new(&vec!["A"; 256].join(".")).is_err());
    assert_eq!(scope_vec("\\", &[]).unwrap(), [0x10, 3, b'\\', 0]);
    assert_eq!(scope_vec("^^", &[]).unwrap(), [0x10, 4, b'^', b'^', 0]);
    assert_eq!(&scope_vec("^A.B", &[]).unwrap()[2..], b"^\x2eA___B___");
    assert_eq!(
        &scope_vec("\\A.B.C", &[]).unwrap()[2..],
        b"\\\x2f\x03A___B___C___"
    );
}

#[test]
fn package_lengths_cover_all_width_transitions_and_overflow() {
    for (content, expected) in [
        (62, vec![0x3f]),
        (63, vec![0x41, 0x04]),
        (4093, vec![0x4f, 0xff]),
        (4094, vec![0x81, 0, 1]),
        (0xffffc, vec![0x8f, 0xff, 0xff]),
        (0xffffd, vec![0xc1, 0, 0, 1]),
        (0xffffffb, vec![0xcf, 0xff, 0xff, 0xff]),
    ] {
        let (bytes, len) = package_length(content).unwrap();
        assert_eq!(&bytes[..len], expected);
    }
    for content in [0xffffffc, usize::MAX] {
        assert_eq!(package_length(content), Err(AmlError::LengthOverflow));
    }
}

#[test]
fn scopes_require_temporary_slack_and_rollback_on_failure() {
    // Empty one-segment scope is six final bytes, but initially needs nine.
    for capacity in 0..9 {
        let mut storage = vec![0; capacity];
        let mut writer = AmlWriter::new(&mut storage);
        assert_eq!(writer.scope("TEST", |_| Ok(())), Err(AmlError::Capacity));
        assert_eq!(writer.position(), 0);
        assert_eq!(writer.as_slice(), Err(AmlError::Capacity));
        assert_eq!(writer.byte(1), Err(AmlError::Capacity));
    }
    let mut storage = [0; 9];
    let mut writer = AmlWriter::new(&mut storage);
    writer.scope("TEST", |_| Ok(())).unwrap();
    assert_eq!(
        writer.as_slice().unwrap(),
        &[0x10, 5, b'T', b'E', b'S', b'T']
    );
    for capacity in [17, 18] {
        let mut storage = vec![0; capacity];
        let mut writer = AmlWriter::new(&mut storage);
        let result = writer.scope("PARN", |writer| writer.scope("CHLD", |_| Ok(())));
        assert_eq!(result.is_ok(), capacity == 18);
        assert_eq!(writer.position(), if capacity == 18 { 12 } else { 0 });
    }
}

#[test]
fn failed_tables_cannot_be_finalized_even_if_error_is_ignored() {
    let mut storage = [0; 36];
    let result = AmlWriter::table(
        &mut storage,
        *b"DSDT",
        2,
        *b"FSTART",
        *b"HARDTEST",
        1,
        |writer| {
            let _ = writer.byte(0);
            Ok(())
        },
    );
    assert!(matches!(result, Err(AmlError::Capacity)));
    for negative in [false, true] {
        let mut storage = [0; 128];
        let result = AmlWriter::table(
            &mut storage,
            *b"DSDT",
            2,
            *b"FSTART",
            *b"HARDTEST",
            1,
            |writer| {
                let value = if negative { -1i64 } else { 256 };
                let fragment = acpi_dsl! { Name("TEST", #{byte value}); };
                // Exercise direct fragment-to-writer emission, not just emit_bound.
                let _ = fragment.emit(writer);
                Ok(())
            },
        );
        assert!(matches!(result, Err(AmlError::InvalidOperand)));
    }
    let mut storage = [0; 35];
    assert!(matches!(
        AmlWriter::table(
            &mut storage,
            *b"DSDT",
            2,
            *b"FSTART",
            *b"HARDTEST",
            1,
            |_| Ok(())
        ),
        Err(AmlError::Capacity)
    ));
}

#[test]
fn complete_table_has_final_length_and_checksum() {
    static CHILD: AmlFragment<6, 0> = acpi_dsl! { Name("TEST", 1u32); };
    let mut storage = [0; 128];
    let table = AmlWriter::table(
        &mut storage,
        *b"DSDT",
        2,
        *b"FSTART",
        *b"HARDTEST",
        1,
        |writer| writer.scope("\\_SB_.PCI0", |writer| writer.emit(&CHILD, &[])),
    )
    .unwrap();
    let bytes = table.as_slice().unwrap();
    assert_eq!(&bytes[..4], b"DSDT");
    assert_eq!(
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as usize,
        bytes.len()
    );
    assert_eq!(
        bytes.iter().fold(0u8, |sum, byte| sum.wrapping_add(*byte)),
        0
    );
}

#[test]
fn generated_metadata_rejects_bad_offsets_widths_and_operand_indices() {
    for (bytes, fixup) in [
        ([0; 4], Fixup::raw(usize::MAX, FixupKind::Byte)),
        ([0; 4], Fixup::raw(1, FixupKind::DWord)),
        ([0; 4], Fixup::new(1, FixupKind::Byte)),
        (
            [0; 4],
            Fixup::with_range_length(
                0,
                FixupKind::Byte,
                4,
                fstart_acpi::FixupValue::Literal(0),
                fstart_acpi::FixupValue::Literal(1),
            ),
        ),
        (
            [0; 4],
            Fixup::with_range_length(
                0,
                FixupKind::Byte,
                1,
                fstart_acpi::FixupValue::Operand(1),
                fstart_acpi::FixupValue::Literal(1),
            ),
        ),
    ] {
        assert!(std::panic::catch_unwind(|| AmlFragment::new(bytes, [fixup])).is_err());
    }
}
