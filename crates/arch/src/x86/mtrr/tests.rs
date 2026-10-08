use super::*;

fn caps(slots: usize) -> Capabilities {
    Capabilities {
        variable_count: slots,
        physical_bits: 36,
        fixed: true,
        write_combining: true,
    }
}

fn effective_type(plan: &Plan, address: u64) -> CacheType {
    if plan.caps.fixed && address < ONE_MIB {
        return if address < 0xa0000 {
            CacheType::WriteBack
        } else {
            CacheType::Uncacheable
        };
    }
    let mut found = None;
    for range in plan
        .ranges
        .iter()
        .filter(|range| range.base <= address && address < range.end)
    {
        found = Some(match found {
            None => range.kind,
            Some(previous) if previous == range.kind => previous,
            Some(CacheType::Uncacheable) => CacheType::Uncacheable,
            Some(_) if range.kind == CacheType::Uncacheable => CacheType::Uncacheable,
            Some(_) => panic!("undefined overlapping cache types"),
        });
    }
    found.unwrap_or(plan.default)
}

fn assert_span(plan: &Plan, base: u64, end: u64, kind: CacheType) {
    // Checking both sides of every transition proves the whole interval, not
    // just a representative address in the last high-RAM chunk.
    for address in core::iter::once(base)
        .chain(core::iter::once(end - 1))
        .chain(plan.ranges.iter().flat_map(|range| [range.base, range.end]))
        .flat_map(|boundary| [boundary.saturating_sub(1), boundary])
        .filter(|address| base <= *address && *address < end)
    {
        assert_eq!(effective_type(plan, address), kind, "address {address:#x}");
    }
}

const X61_RAM: [(u64, u64); 3] = [
    (0, 0xa0000),
    (ONE_MIB, 0xbde00000 - ONE_MIB),
    (0x100000000, 0x3c000000),
];

#[test]
fn x61_covers_all_high_ram_and_keeps_mmio_uc_with_wc_framebuffer() {
    let plan = Plan::build(caps(8), X61_RAM, [(0xd0000000, 0x10000000)]).unwrap();
    assert_eq!(plan.default, CacheType::Uncacheable);
    assert_eq!(plan.ranges.len(), 7);
    assert_span(&plan, ONE_MIB, 0xbde00000, CacheType::WriteBack);
    assert_span(&plan, 0x100000000, 0x13c000000, CacheType::WriteBack);
    assert_eq!(effective_type(&plan, 0x13ba89000), CacheType::WriteBack);
    assert_span(&plan, 0xbde00000, 0xd0000000, CacheType::Uncacheable);
    assert_span(&plan, 0xd0000000, 0xe0000000, CacheType::WriteCombining);
    assert_span(&plan, 0xe0000000, 0x100000000, CacheType::Uncacheable);
    assert_span(&plan, 0x13c000000, 1 << 36, CacheType::Uncacheable);
    assert_span(&plan, 0xa0000, ONE_MIB, CacheType::Uncacheable);
}

#[test]
fn optional_aperture_cannot_displace_required_ram() {
    assert!(Plan::build(caps(6), X61_RAM, [(0xd0000000, 0x10000000)]).is_err());
    let ram_only = Plan::build(caps(6), X61_RAM, []).unwrap();
    assert_eq!(ram_only.ranges.len(), 6);
    assert_span(&ram_only, 0x100000000, 0x13c000000, CacheType::WriteBack);
    assert!(matches!(
        Plan::build(caps(5), X61_RAM, []),
        Err(ServiceError::NotSupported)
    ));
}

#[test]
fn default_wb_wins_when_explicit_uc_holes_use_fewer_slots() {
    let mut limits = caps(2);
    limits.physical_bits = 32;
    let plan = Plan::build(limits, [(0, 0xc0000000), (0xc1000000, 0x3e000000)], []).unwrap();
    assert_eq!(plan.default, CacheType::WriteBack);
    assert_eq!(plan.ranges.len(), 2);
    assert_span(&plan, ONE_MIB, 0xc0000000, CacheType::WriteBack);
    assert_span(&plan, 0xc0000000, 0xc1000000, CacheType::Uncacheable);
    assert_span(&plan, 0xc1000000, 0xff000000, CacheType::WriteBack);
    assert_span(&plan, 0xff000000, 0x100000000, CacheType::Uncacheable);
}

#[test]
fn no_fixed_mtrrs_preserves_legacy_holes_with_variable_ranges() {
    let mut limits = caps(32);
    limits.fixed = false;
    let plan = Plan::build(limits, X61_RAM, []).unwrap();
    assert_span(&plan, 0, 0xa0000, CacheType::WriteBack);
    assert_span(&plan, 0xa0000, ONE_MIB, CacheType::Uncacheable);
    assert_span(&plan, ONE_MIB, 0xbde00000, CacheType::WriteBack);
    assert_span(&plan, 0x100000000, 0x13c000000, CacheType::WriteBack);
}

#[test]
fn fixed_policy_handles_sub_page_ebda_reservation() {
    let plan = Plan::build(caps(8), [(0, 0x9fc00), (ONE_MIB, 0x1ff00000)], []).unwrap();
    assert_span(&plan, 0, 0xa0000, CacheType::WriteBack);
    assert_span(&plan, 0xa0000, ONE_MIB, CacheType::Uncacheable);
    assert_span(&plan, ONE_MIB, 0x20000000, CacheType::WriteBack);
}

#[test]
fn publication_rejects_overflow_without_losing_the_previous_physical_map() {
    let physical = [(0, 0x9fc00), (ONE_MIB, 0x1ff00000)];
    set_ram_wb_ranges(&physical).unwrap();
    assert_eq!(
        ram_ranges().collect::<Vec<_, MAX_RAM_RANGES>>().as_slice(),
        &physical
    );
    assert!(
        set_ram_wb_ranges_from(
            (0..MAX_RAM_RANGES + 1).map(|index| (ONE_MIB + index as u64 * 0x2000, 0x1000))
        )
        .is_err()
    );
    assert!(set_ram_wb_ranges(&[(ONE_MIB, u64::MAX)]).is_err());
    assert_eq!(
        ram_ranges().collect::<Vec<_, MAX_RAM_RANGES>>().as_slice(),
        &physical
    );
}

#[test]
fn sparse_and_adjacent_ram_preserves_every_hole() {
    let ram = [
        (0, 0x80000000),
        (0x80000000, 0x40000000),
        (0x100000000, 0x10000000),
        (0x180000000, 0x20000000),
    ];
    let plan = Plan::build(caps(8), ram, []).unwrap();
    assert_span(&plan, ONE_MIB, 0xc0000000, CacheType::WriteBack);
    assert_span(&plan, 0xc0000000, 0x100000000, CacheType::Uncacheable);
    assert_span(&plan, 0x100000000, 0x110000000, CacheType::WriteBack);
    assert_span(&plan, 0x110000000, 0x180000000, CacheType::Uncacheable);
    assert_span(&plan, 0x180000000, 0x1a0000000, CacheType::WriteBack);
    assert_span(&plan, 0x1a0000000, 1 << 36, CacheType::Uncacheable);
}

#[test]
fn invalid_or_unrepresentable_input_is_not_truncated() {
    for ram in [[(1, 0x100000)], [(0, u64::MAX)], [(1 << 36, 0x1000)]] {
        assert!(matches!(
            Plan::build(caps(8), ram, []),
            Err(ServiceError::InvalidParam)
        ));
    }
    assert!(matches!(
        Plan::build(caps(8), [(0, 0x80000000), (0x40000000, 0x80000000)], []),
        Err(ServiceError::InvalidParam)
    ));
    assert!(matches!(
        Plan::build(caps(8), X61_RAM, [(0x100000000, 0x10000000)]),
        Err(ServiceError::InvalidParam)
    ));
    assert!(matches!(
        Plan::build(
            caps(32),
            (0..65).map(|index| (ONE_MIB + index * 0x2000, 0x1000)),
            []
        ),
        Err(ServiceError::NotSupported)
    ));
    let mut no_wc = caps(8);
    no_wc.write_combining = false;
    assert!(matches!(
        Plan::build(no_wc, X61_RAM, [(0xd0000000, 0x10000000)]),
        Err(ServiceError::NotSupported)
    ));
}

#[test]
fn sipi_snapshot_copies_every_fixed_variable_and_pat_msr_with_enable_last() {
    let entries = snapshot_msrs(caps(8), true, |index| {
        (u64::from(index) << 32) | !u64::from(index) & 0xffffffff
    })
    .unwrap();
    assert_eq!(core::mem::size_of::<MsrEntry>(), 12);
    assert_eq!(entries.len(), 29);
    assert_eq!(
        entries[..11]
            .iter()
            .map(|entry| entry.index)
            .collect::<Vec<_, 11>>()
            .as_slice(),
        &FIXED_MSRS
    );
    for (ordinal, entry) in entries[11..27].iter().enumerate() {
        assert_eq!(entry.index, IA32_MTRR_PHYSBASE0 + ordinal as u32);
    }
    assert_eq!(entries[27].index, 0x277);
    assert_eq!(entries[28].index, IA32_MTRR_DEF_TYPE);
    for entry in &entries {
        assert_eq!(entry.high, entry.index);
        assert_eq!(entry.low, !entry.index);
    }
    let no_fixed = snapshot_msrs(
        Capabilities {
            fixed: false,
            ..caps(8)
        },
        false,
        |_| 0,
    )
    .unwrap();
    assert_eq!(no_fixed.len(), 17);
    assert_eq!(no_fixed.last().unwrap().index, IA32_MTRR_DEF_TYPE);
    assert!(snapshot_msrs(caps(MAX_MTRRS + 1), true, |_| 0).is_err());
}
