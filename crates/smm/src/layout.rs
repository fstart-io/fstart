//! SMRAM placement math for PIC SMM images.
//!
//! The layout follows the same hardware constraints coreboot handles in
//! `smm_module_loader.c`: each CPU enters at `SMBASE + 0x8000`, while its
//! save-state area lives at the top of the 64 KiB SMBASE window and grows
//! downward.  fstart differs by copying one of several precompiled PIC entry
//! stubs per CPU rather than loading one relocatable stub and duplicating it.

/// Architectural SMM entry offset from SMBASE.
pub const SMM_ENTRY_OFFSET: u64 = 0x8000;
/// Architectural default/per-CPU SMM window size.
pub const SMM_CODE_SEGMENT_SIZE: u64 = 0x1_0000;
/// Load alignment for the VMA-zero handler, matching coreboot rmodules.
/// The image builder rejects sections requiring a greater alignment.
pub const SMM_HANDLER_ALIGNMENT: u64 = 4096;

/// Errors from SMRAM layout computation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutError {
    /// `entry_count` is zero.
    NoEntries,
    /// The caller-provided output buffer is too small.
    TooManyEntries,
    /// A size argument is zero or otherwise unusable.
    BadSize,
    /// The entry stub would overlap the architectural entry offset or save state.
    StubDoesNotFit,
    /// The requested regions do not fit inside SMRAM.
    SmramTooSmall,
    /// Address arithmetic overflowed.
    Overflow,
}

/// Inputs for computing the permanent SMRAM layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SmramLayout {
    /// Permanent SMRAM/TSEG base.
    pub smram_base: u64,
    /// Permanent SMRAM/TSEG size.
    pub smram_size: u64,
    /// Number of precompiled entry stubs / CPU slots to place.
    pub entry_count: u16,
    /// Size of each CPU save-state area.
    pub save_state_size: u32,
    /// Per-CPU SMM stack size.
    pub stack_size: u32,
    /// Maximum copied stub size.
    pub entry_stub_size: u32,
    /// Complete handler memory size, including BSS and loader-owned blocks.
    pub handler_mem_size: u32,
    /// Optional page-table bytes below the handler/data region for long mode.
    pub page_table_size: u32,
}

/// Computed per-CPU SMM placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuSmmLayout {
    /// SMBASE written into the CPU save state during relocation.
    pub smbase: u64,
    /// Address where the entry stub is copied (`smbase + 0x8000`).
    pub entry_addr: u64,
    /// Save-state area base.
    pub save_state_base: u64,
    /// Save-state area top (exclusive).
    pub save_state_top: u64,
    /// Stack bottom.
    pub stack_bottom: u64,
    /// Stack top (exclusive), selected by the entry stub for this CPU.
    pub stack_top: u64,
}

/// Round `value` up to a 4 KiB (`SMM_HANDLER_ALIGNMENT`) boundary without
/// 64-bit division.
const fn align_up_4k(value: u64) -> Option<u64> {
    const MASK: u64 = SMM_HANDLER_ALIGNMENT - 1;
    match value.checked_add(MASK) {
        Some(v) => Some(v & !MASK),
        None => None,
    }
}

/// Compute the base address of the copied SMM handler/data region.
pub fn compute_common_base(layout: &SmramLayout) -> Result<u64, LayoutError> {
    if layout.smram_size == 0 {
        return Err(LayoutError::BadSize);
    }
    let smram_top = layout
        .smram_base
        .checked_add(layout.smram_size)
        .ok_or(LayoutError::Overflow)?;
    let handler_aligned =
        align_up_4k(layout.handler_mem_size as u64).ok_or(LayoutError::Overflow)?;
    let pt_aligned = align_up_4k(layout.page_table_size as u64).ok_or(LayoutError::Overflow)?;
    let top_reserved = handler_aligned
        .checked_add(pt_aligned)
        .ok_or(LayoutError::Overflow)?;
    let base = smram_top
        .checked_sub(top_reserved)
        .ok_or(LayoutError::SmramTooSmall)?
        & !(SMM_HANDLER_ALIGNMENT - 1);
    if base < layout.smram_base {
        return Err(LayoutError::SmramTooSmall);
    }
    Ok(base)
}

/// Compute the per-CPU SMM layout.
///
/// `out` must have room for at least `layout.entry_count` entries.  The return
/// value is the populated prefix of `out`.
pub fn compute_cpu_layout<'a>(
    layout: &SmramLayout,
    out: &'a mut [CpuSmmLayout],
) -> Result<&'a [CpuSmmLayout], LayoutError> {
    let count = layout.entry_count as usize;
    if count == 0 {
        return Err(LayoutError::NoEntries);
    }
    if count > out.len() {
        return Err(LayoutError::TooManyEntries);
    }
    if layout.smram_size == 0
        || layout.save_state_size == 0
        || layout.stack_size == 0
        || layout.entry_stub_size == 0
    {
        return Err(LayoutError::BadSize);
    }
    if layout.entry_stub_size as u64 >= SMM_ENTRY_OFFSET {
        return Err(LayoutError::StubDoesNotFit);
    }

    // Common handler and optional page tables are placed at the top of SMRAM.
    let common_base = compute_common_base(layout)?;

    // Stacks grow upward from the beginning of SMRAM as a contiguous region.
    let total_stack = (layout.stack_size as u64)
        .checked_mul(layout.entry_count as u64)
        .ok_or(LayoutError::Overflow)?;
    let stacks_end = layout
        .smram_base
        .checked_add(total_stack)
        .ok_or(LayoutError::Overflow)?;
    if stacks_end > common_base {
        return Err(LayoutError::SmramTooSmall);
    }

    let needed_ss_size =
        core::cmp::max(layout.save_state_size as u64, layout.entry_stub_size as u64);
    let available = SMM_CODE_SEGMENT_SIZE - SMM_ENTRY_OFFSET - layout.entry_stub_size as u64;
    if needed_ss_size > available {
        return Err(LayoutError::StubDoesNotFit);
    }

    // First segment begins immediately below the top-reserved common/page-table
    // area, then more 64 KiB windows are allocated downward as needed.
    let mut segment_base = Some(
        common_base
            .checked_sub(SMM_CODE_SEGMENT_SIZE)
            .ok_or(LayoutError::SmramTooSmall)?,
    );
    let mut in_segment_offset = 0u64;
    let mut stack_bottom = layout.smram_base;

    for slot in out.iter_mut().take(count) {
        let smbase = segment_base
            .and_then(|base| base.checked_sub(in_segment_offset))
            .ok_or(LayoutError::SmramTooSmall)?;
        let entry_addr = smbase
            .checked_add(SMM_ENTRY_OFFSET)
            .ok_or(LayoutError::Overflow)?;
        let save_state_top = smbase
            .checked_add(SMM_CODE_SEGMENT_SIZE)
            .ok_or(LayoutError::Overflow)?;
        let save_state_base = save_state_top
            .checked_sub(layout.save_state_size as u64)
            .ok_or(LayoutError::Overflow)?;
        let stack_top = stack_bottom
            .checked_add(layout.stack_size as u64)
            .ok_or(LayoutError::Overflow)?;

        if smbase < stacks_end || save_state_top > common_base {
            return Err(LayoutError::SmramTooSmall);
        }
        if entry_addr + layout.entry_stub_size as u64 > save_state_base {
            return Err(LayoutError::StubDoesNotFit);
        }

        *slot = CpuSmmLayout {
            smbase,
            entry_addr,
            save_state_base,
            save_state_top,
            stack_bottom,
            stack_top,
        };

        stack_bottom = stack_top;
        // Advance to the next slot within this 64 KiB window; if another slot
        // of size `needed_ss_size` would exceed `available`, move down one full
        // 64 KiB window. Mathematically equivalent to `per_segment = available / needed_ss_size`.
        let next_offset = in_segment_offset + needed_ss_size;
        if next_offset + needed_ss_size <= available {
            in_segment_offset = next_offset;
        } else {
            in_segment_offset = 0;
            segment_base = segment_base.and_then(|base| base.checked_sub(SMM_CODE_SEGMENT_SIZE));
        }
    }

    Ok(&out[..count])
}

/// Return the page-table base reserved immediately above the handler image.
pub fn compute_page_table_base(layout: &SmramLayout) -> Result<u64, LayoutError> {
    compute_common_base(layout)?
        .checked_add(align_up_4k(layout.handler_mem_size as u64).ok_or(LayoutError::Overflow)?)
        .ok_or(LayoutError::Overflow)
}

/// Physical base of the identity page tables the default relocation stub loads
/// into CR3.
pub const SMM_RELOCATION_TABLE_OFFSET: u64 = 0x9000;

/// PML4, one PDPT and four page directories mapping the low 4 GiB.
pub const SMM_IDENTITY_TABLE_SIZE: u32 = 6 * 4096;
pub const SMM_RELOCATION_TABLE_SIZE: u64 = SMM_IDENTITY_TABLE_SIZE as u64;

/// Build a 4 GiB identity map with 2 MiB pages into `tables`, encoding
/// hardware page-table pointers relative to `phys_base`.
///
/// `tables` must be at least [`SMM_IDENTITY_TABLE_SIZE`] bytes long.
pub(crate) fn build_identity_tables_in_slice(
    tables: &mut [core::mem::MaybeUninit<u8>],
    phys_base: u64,
) -> u64 {
    const PTE_PRESENT: u64 = 1 << 0;
    const PTE_WRITABLE: u64 = 1 << 1;
    const PTE_PAGE_SIZE: u64 = 1 << 7;
    const ENTRIES: usize = 512;

    let tables = &mut tables[..SMM_IDENTITY_TABLE_SIZE as usize];
    // Zero every unused PML4/PDPT entry so stale SMRAM contents cannot create
    // unintended translations.
    tables.fill(core::mem::MaybeUninit::new(0));
    // SAFETY: every byte in `tables` was just initialized to 0 above.
    let tables =
        unsafe { core::slice::from_raw_parts_mut(tables.as_mut_ptr().cast::<u8>(), tables.len()) };

    let pdpt_phys = phys_base.wrapping_add(4096);
    let pds_phys = phys_base.wrapping_add(2 * 4096);

    tables[0..8].copy_from_slice(&(pdpt_phys | PTE_PRESENT | PTE_WRITABLE).to_le_bytes());
    for i in 0..4usize {
        let off = 4096 + i * 8;
        let entry = pds_phys.wrapping_add((i as u64) * 4096) | PTE_PRESENT | PTE_WRITABLE;
        tables[off..off + 8].copy_from_slice(&entry.to_le_bytes());
    }
    for pd in 0..4usize {
        for entry_idx in 0..ENTRIES {
            let slot = pd * ENTRIES + entry_idx;
            let off = 2 * 4096 + slot * 8;
            let phys = (slot as u64) << 21;
            let entry = phys | PTE_PRESENT | PTE_WRITABLE | PTE_PAGE_SIZE;
            tables[off..off + 8].copy_from_slice(&entry.to_le_bytes());
        }
    }
    phys_base
}

/// Build a 4 GiB identity map with 2 MiB pages at an explicitly reserved base.
///
/// # Safety
///
/// `base..base + SMM_IDENTITY_TABLE_SIZE` must be writable and exclusively
/// owned by the caller.
pub unsafe fn build_identity_tables(base: u64) -> u64 {
    // SAFETY: caller guarantees the region is writable and exclusively owned;
    // `MaybeUninit<u8>` requires no prior initialization of SMRAM bytes.
    let slice = unsafe {
        core::slice::from_raw_parts_mut(
            base as *mut core::mem::MaybeUninit<u8>,
            SMM_IDENTITY_TABLE_SIZE as usize,
        )
    };
    build_identity_tables_in_slice(slice, base)
}

/// Build the temporary default-SMBASE identity tables.
///
/// # Safety
///
/// The default SMBASE window must be writable and reserved for relocation.
pub unsafe fn build_relocation_identity_tables(default_smbase: u64) -> u64 {
    unsafe { build_identity_tables(default_smbase + SMM_RELOCATION_TABLE_OFFSET) }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;

    const ZERO_CPU: CpuSmmLayout = CpuSmmLayout {
        smbase: 0,
        entry_addr: 0,
        save_state_base: 0,
        save_state_top: 0,
        stack_bottom: 0,
        stack_top: 0,
    };

    fn overlaps(a: (u64, u64), b: (u64, u64)) -> bool {
        a.0 < b.1 && b.0 < a.1
    }

    fn assert_complete_layout(layout: &SmramLayout, cpus: &[CpuSmmLayout]) {
        let smram = (layout.smram_base, layout.smram_base + layout.smram_size);
        let common_base = compute_common_base(layout).unwrap();
        let page_table_base = compute_page_table_base(layout).unwrap();
        assert_eq!(common_base % SMM_HANDLER_ALIGNMENT, 0);
        assert_eq!(page_table_base % 4096, 0);
        let common = (
            common_base,
            common_base + u64::from(layout.handler_mem_size),
        );
        let page_tables = (
            page_table_base,
            page_table_base + u64::from(layout.page_table_size),
        );
        assert!(!overlaps(common, page_tables));
        assert!(common.0 >= smram.0 && common.1 <= smram.1);
        assert!(page_tables.0 >= smram.0 && page_tables.1 <= smram.1);

        for (i, cpu) in cpus.iter().enumerate() {
            let stack = (cpu.stack_bottom, cpu.stack_top);
            let stub = (
                cpu.entry_addr,
                cpu.entry_addr + u64::from(layout.entry_stub_size),
            );
            let save_state = (cpu.save_state_base, cpu.save_state_top);
            assert_eq!(cpu.entry_addr, cpu.smbase + SMM_ENTRY_OFFSET);
            for range in [stack, stub, save_state] {
                assert!(range.0 >= smram.0 && range.1 <= smram.1);
                assert!(!overlaps(range, common));
                assert!(!overlaps(range, page_tables));
            }
            assert!(!overlaps(stack, stub));
            assert!(!overlaps(stack, save_state));
            assert!(!overlaps(stub, save_state));

            for other in &cpus[..i] {
                assert!(!overlaps(stack, (other.stack_bottom, other.stack_top)));
                assert!(!overlaps(
                    stub,
                    (
                        other.entry_addr,
                        other.entry_addr + u64::from(layout.entry_stub_size),
                    ),
                ));
                assert!(!overlaps(
                    save_state,
                    (other.save_state_base, other.save_state_top),
                ));
            }
        }
    }

    #[test]
    fn lays_out_and_contains_four_q35_entries() {
        let layout = SmramLayout {
            smram_base: 0x7f00_0000,
            smram_size: 0x80_0000,
            entry_count: 4,
            save_state_size: 0x400,
            stack_size: 0x400,
            entry_stub_size: 0x600,
            handler_mem_size: 0x4000,
            page_table_size: SMM_IDENTITY_TABLE_SIZE,
        };
        let mut cpus = [ZERO_CPU; 4];
        let out = compute_cpu_layout(&layout, &mut cpus).unwrap();
        assert_complete_layout(&layout, out);
    }

    #[test]
    fn preserves_handler_alignment_with_unaligned_memory_extent() {
        let layout = SmramLayout {
            smram_base: 0x0100_0000,
            smram_size: 0x0100_0000,
            entry_count: 4,
            save_state_size: 0x400,
            stack_size: 0x400,
            entry_stub_size: 0x600,
            handler_mem_size: 432,
            page_table_size: SMM_IDENTITY_TABLE_SIZE,
        };
        let mut cpus = [ZERO_CPU; 4];
        let out = compute_cpu_layout(&layout, &mut cpus).unwrap();
        assert_complete_layout(&layout, out);
        assert_eq!((compute_common_base(&layout).unwrap() + 0x40) % 64, 0);
    }

    #[test]
    fn lays_out_multiple_smbase_segments_without_region_overlap() {
        let layout = SmramLayout {
            smram_base: 0x7e00_0000,
            smram_size: 0x100_0000,
            entry_count: 70,
            save_state_size: 0x400,
            stack_size: 0x800,
            entry_stub_size: 0x600,
            handler_mem_size: 0x8000,
            page_table_size: SMM_IDENTITY_TABLE_SIZE,
        };
        let mut cpus = std::vec![ZERO_CPU; layout.entry_count as usize];
        let out = compute_cpu_layout(&layout, &mut cpus).unwrap();
        assert_complete_layout(&layout, out);
        assert!(out.iter().any(|cpu| cpu.smbase < out[0].smbase - 0x8000));
    }

    #[test]
    fn rejects_layout_when_stacks_collide_with_smm_windows() {
        let layout = SmramLayout {
            smram_base: 0x100000,
            smram_size: 0x2_0000,
            entry_count: 4,
            save_state_size: 0x400,
            stack_size: 0x4000,
            entry_stub_size: 0x600,
            handler_mem_size: 0x4000,
            page_table_size: SMM_IDENTITY_TABLE_SIZE,
        };
        let mut cpus = [ZERO_CPU; 4];
        assert_eq!(
            compute_cpu_layout(&layout, &mut cpus),
            Err(LayoutError::SmramTooSmall)
        );
    }

    #[test]
    fn identity_tables_clear_unused_entries() {
        let mut tables =
            std::vec![core::mem::MaybeUninit::new(0xffu8); SMM_IDENTITY_TABLE_SIZE as usize];
        build_identity_tables_in_slice(&mut tables, 0x7f00_0000);
        // SAFETY: `build_identity_tables_in_slice` initializes every byte.
        let bytes =
            unsafe { core::slice::from_raw_parts(tables.as_ptr().cast::<u8>(), tables.len()) };

        assert!(bytes[8..4096].iter().all(|&byte| byte == 0));
        assert!(bytes[4096 + 4 * 8..8192].iter().all(|&byte| byte == 0));
    }

    #[test]
    fn rejects_oversized_stub() {
        let layout = SmramLayout {
            smram_base: 0,
            smram_size: 0x1_0000,
            entry_count: 1,
            save_state_size: 0x400,
            stack_size: 0x400,
            entry_stub_size: 0x8000,
            handler_mem_size: 0,
            page_table_size: 0,
        };
        let mut cpus = [CpuSmmLayout {
            smbase: 0,
            entry_addr: 0,
            save_state_base: 0,
            save_state_top: 0,
            stack_bottom: 0,
            stack_top: 0,
        }; 1];
        assert_eq!(
            compute_cpu_layout(&layout, &mut cpus).unwrap_err(),
            LayoutError::StubDoesNotFit
        );
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    const ZERO_CPU: CpuSmmLayout = CpuSmmLayout {
        smbase: 0,
        entry_addr: 0,
        save_state_base: 0,
        save_state_top: 0,
        stack_bottom: 0,
        stack_top: 0,
    };

    fn overlaps_u128(a: (u128, u128), b: (u128, u128)) -> bool {
        a.0 < a.1 && b.0 < b.1 && a.0 < b.1 && b.0 < a.1
    }

    #[kani::proof]
    fn verify_common_and_page_table_layout_inside_smram() {
        let layout = SmramLayout {
            smram_base: kani::any(),
            smram_size: kani::any(),
            entry_count: kani::any(),
            save_state_size: kani::any(),
            stack_size: kani::any(),
            entry_stub_size: kani::any(),
            handler_mem_size: kani::any(),
            page_table_size: kani::any(),
        };

        let Ok(common_base) = compute_common_base(&layout) else {
            return;
        };
        let page_table_base = compute_page_table_base(&layout).unwrap();

        let smram_start = u128::from(layout.smram_base);
        let smram_end = smram_start + u128::from(layout.smram_size);
        assert!(smram_end <= u128::from(u64::MAX));

        let common = (
            u128::from(common_base),
            u128::from(common_base) + u128::from(layout.handler_mem_size),
        );
        let page_tables = (
            u128::from(page_table_base),
            u128::from(page_table_base) + u128::from(layout.page_table_size),
        );

        assert_eq!(common_base % SMM_HANDLER_ALIGNMENT, 0);
        assert_eq!(page_table_base % 4096, 0);
        assert!(common.0 >= smram_start && common.1 <= smram_end);
        assert!(page_tables.0 >= smram_start && page_tables.1 <= smram_end);
        assert!(!overlaps_u128(common, page_tables));
    }

    fn overlaps_u64(a: (u64, u64), b: (u64, u64)) -> bool {
        a.0 < a.1 && b.0 < b.1 && a.0 < b.1 && b.0 < a.1
    }

    /// Proves that for up to 3 CPUs (covering single-CPU, intra-segment packing,
    /// and multi-segment transitions across 64 KiB SMBASE windows) with arbitrary
    /// SMRAM bases/sizes and stub/save-state/stack sizes, every returned stack,
    /// stub, and save-state area is non-empty, stays inside SMRAM, does not
    /// overlap the common handler or page tables, and is pairwise disjoint across
    /// all CPUs.
    #[kani::proof]
    #[kani::unwind(5)]
    fn verify_cpu_layout_containment_and_pairwise_disjointness() {
        let entry_count: u16 = kani::any();
        kani::assume(entry_count <= 3);

        let layout = SmramLayout {
            smram_base: kani::any(),
            smram_size: kani::any(),
            entry_count,
            save_state_size: kani::any(),
            stack_size: kani::any(),
            entry_stub_size: kani::any(),
            handler_mem_size: kani::any(),
            page_table_size: kani::any(),
        };

        let mut cpus = [ZERO_CPU; 3];
        let Ok(out) = compute_cpu_layout(&layout, &mut cpus) else {
            return;
        };

        let common_base = compute_common_base(&layout).unwrap();
        let page_table_base = compute_page_table_base(&layout).unwrap();
        let smram_start = layout.smram_base;
        let smram_end = layout.smram_base.checked_add(layout.smram_size).unwrap();
        let common = (
            common_base,
            common_base
                .checked_add(u64::from(layout.handler_mem_size))
                .unwrap(),
        );
        let page_tables = (
            page_table_base,
            page_table_base
                .checked_add(u64::from(layout.page_table_size))
                .unwrap(),
        );

        let count = out.len();
        assert_eq!(count, usize::from(entry_count));
        for i in 0..3 {
            if i >= count {
                break;
            }
            let cpu = out[i];
            let stack = (cpu.stack_bottom, cpu.stack_top);
            let stub = (
                cpu.entry_addr,
                cpu.entry_addr
                    .checked_add(u64::from(layout.entry_stub_size))
                    .unwrap(),
            );
            let save_state = (cpu.save_state_base, cpu.save_state_top);
            assert_eq!(
                cpu.entry_addr,
                cpu.smbase.checked_add(SMM_ENTRY_OFFSET).unwrap()
            );
            for range in [stack, stub, save_state] {
                assert!(range.0 < range.1);
                assert!(range.0 >= smram_start && range.1 <= smram_end);
                assert!(!overlaps_u64(range, common));
                assert!(!overlaps_u64(range, page_tables));
            }
            assert!(!overlaps_u64(stack, stub));
            assert!(!overlaps_u64(stack, save_state));
            assert!(!overlaps_u64(stub, save_state));

            for j in 0..i {
                let other = out[j];
                let o_stack = (other.stack_bottom, other.stack_top);
                let o_stub = (
                    other.entry_addr,
                    other
                        .entry_addr
                        .checked_add(u64::from(layout.entry_stub_size))
                        .unwrap(),
                );
                let o_save = (other.save_state_base, other.save_state_top);
                assert!(!overlaps_u64(stack, o_stack));
                assert!(!overlaps_u64(stack, o_stub));
                assert!(!overlaps_u64(stack, o_save));
                assert!(!overlaps_u64(stub, o_stack));
                assert!(!overlaps_u64(stub, o_stub));
                assert!(!overlaps_u64(stub, o_save));
                assert!(!overlaps_u64(save_state, o_stack));
                assert!(!overlaps_u64(save_state, o_stub));
                assert!(!overlaps_u64(save_state, o_save));
            }
        }
        kani::cover!(entry_count >= 2, "multi-CPU layout succeeded");
    }
}
