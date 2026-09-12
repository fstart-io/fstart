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
        |e: &mut Expectations| e.descriptor.reservation.base = LIMIT,
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
