use super::*;
use crate::{layout::EncodedLayout, linker::Xip};
use fstart_core::{Platform, layout::RegionKind};

fn xip(payload: Span) -> Xip {
    Xip {
        platform: Platform::Armv7,
        boot_hart_id: 0,
        image: Span {
            base: 0,
            size: 0x1000,
        },
        execution: None,
        writable: Span {
            base: 0x10000,
            size: 0x4000,
        },
        heap: Span {
            base: 0x12000,
            size: 0x1000,
        },
        stack: Span {
            base: 0x13000,
            size: 0x1000,
        },
        descriptor: EncodedLayout::encode(0, &[payload.region(RegionKind::Payload)]).unwrap(),
    }
}

#[test]
fn reset_without_descriptor_still_checks_permissions_entry_and_symbols() {
    // Minimal host-only ELF fixture: one HLT byte, no sections/descriptor. Never run.
    let base = 0x02030000u64;
    let mut bytes = vec![0; 121];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    for (offset, value) in [(16, 2u16), (18, 62), (52, 64), (54, 56), (56, 1), (58, 64)] {
        bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }
    for (offset, value) in [(20, 1u32), (64, 1), (68, 5)] {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    for (offset, value) in [
        (24, base),
        (32, 64),
        (72, 120),
        (80, base),
        (88, base),
        (96, 1),
        (104, 1),
        (112, 1),
    ] {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    bytes[120] = 0xf4;
    let span = Span { base, size: 4096 };
    let mut expected = Expectations {
        architecture: Architecture::X86_64,
        elf64: true,
        little_endian: true,
        stored: vec![span],
        runtime: vec![span],
        identity_mapping: true,
        entry: Some(base),
        copy: None,
        descriptor: None,
        stored_flags: Some(5),
        symbols: BTreeMap::new(),
    };
    validate(&bytes, &expected).unwrap();
    for offset in [121u64, 122, u64::MAX] {
        let mut bad = bytes.clone();
        bad[72..80].copy_from_slice(&offset.to_le_bytes());
        assert!(
            validate(&bad, &expected)
                .unwrap_err()
                .contains("file extent")
        );
    }
    // Start inside the file, but the declared tail is truncated.
    let mut truncated = bytes.clone();
    truncated[96..104].copy_from_slice(&2u64.to_le_bytes());
    truncated[104..112].copy_from_slice(&2u64.to_le_bytes());
    assert!(
        validate(&truncated, &expected)
            .unwrap_err()
            .contains("file extent")
    );
    // A backed entry does not excuse a different truncated LOAD.
    let mut two_loads = bytes.clone();
    two_loads.resize(177, 0);
    two_loads[56..58].copy_from_slice(&2u16.to_le_bytes());
    two_loads[120..176].copy_from_slice(&bytes[64..120]);
    two_loads[72..80].copy_from_slice(&176u64.to_le_bytes());
    two_loads[128..136].copy_from_slice(&177u64.to_le_bytes());
    two_loads[136..144].copy_from_slice(&(base + 16).to_le_bytes());
    two_loads[144..152].copy_from_slice(&(base + 16).to_le_bytes());
    two_loads[176] = 0xf4;
    assert!(
        validate(&two_loads, &expected)
            .unwrap_err()
            .contains("file extent")
    );
    for flags in [6u32, 7] {
        let mut bad = bytes.clone();
        bad[68..72].copy_from_slice(&flags.to_le_bytes());
        assert!(validate(&bad, &expected).is_err());
    }
    let mut bad = bytes.clone();
    bad[24..32].copy_from_slice(&(base + 1).to_le_bytes());
    expected.entry = Some(base + 1);
    assert!(
        validate(&bad, &expected)
            .unwrap_err()
            .contains("file-backed reset-image")
    );
    expected.entry = Some(base);
    expected.symbols.insert("required".into(), base);
    assert!(
        validate(&bytes, &expected)
            .unwrap_err()
            .contains("symbol required")
    );
}

#[test]
fn elf_width_distinguishes_exclusive_range_ends_from_addresses() {
    const LIMIT: u64 = 1u64 << 32;
    for (base, size) in [(0, LIMIT), (LIMIT - 16, 16), (LIMIT - 1, 0)] {
        address_extent(false, base, size).unwrap();
    }
    for (base, size) in [(LIMIT, 0), (LIMIT, 16), (LIMIT - 16, 17)] {
        assert!(
            address_extent(false, base, size)
                .unwrap_err()
                .contains("32-bit")
        );
        address_extent(true, base, size).unwrap();
    }
    assert!(address_extent(true, u64::MAX, 1).is_err());
    let mut layout = xip(Span {
        base: LIMIT - 16,
        size: 16,
    });
    layout.validate().unwrap(); // Descriptor-only payload ending at 2^32 is valid.
    layout.elf_expectations().unwrap().validate().unwrap();
    layout.descriptor = EncodedLayout::encode(
        0,
        &[Span {
            base: LIMIT,
            size: 16,
        }
        .region(RegionKind::Payload)],
    )
    .unwrap();
    assert!(layout.validate().unwrap_err().contains("32-bit"));
    layout.platform = Platform::Aarch64;
    layout.validate().unwrap();
}

#[test]
fn xip_and_elf_expectations_reject_unrepresentable_ranges_entries_and_symbols() {
    const LIMIT: u64 = 1u64 << 32;
    let payload = Span {
        base: 0x20000,
        size: 0x1000,
    };
    let valid = xip(payload).elf_expectations().unwrap();
    let mutations: [fn(&mut Expectations); 6] = [
        |e: &mut Expectations| e.stored[0].base = LIMIT,
        |e: &mut Expectations| e.runtime[0].size = LIMIT + 1,
        |e: &mut Expectations| e.entry = Some(LIMIT),
        |e: &mut Expectations| {
            e.symbols.insert("_end".into(), LIMIT);
        },
        |e: &mut Expectations| e.descriptor.as_mut().unwrap().reservation.base = LIMIT,
        |e: &mut Expectations| {
            e.copy = Some(CopyMapping {
                storage: Span {
                    base: LIMIT,
                    size: 16,
                },
                execution: Span {
                    base: 0x1000,
                    size: 16,
                },
                end_symbol: "_end".into(),
            })
        },
    ];
    for mutate in mutations {
        let mut expected = valid.clone();
        mutate(&mut expected);
        assert!(expected.validate().unwrap_err().contains("32-bit"));
    }
    let mut layout = xip(payload);
    layout.writable.base = LIMIT;
    assert!(layout.validate().unwrap_err().contains("32-bit"));
    let mut layout = xip(payload);
    // All spans fit, but the emitted _stack_top symbol itself would not.
    layout.writable.base = LIMIT - 0x4000;
    layout.heap.base = LIMIT - 0x2000;
    layout.stack.base = LIMIT - 0x1000;
    assert!(layout.validate().unwrap_err().contains("32-bit"));
}
