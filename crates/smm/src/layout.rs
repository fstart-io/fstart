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
    /// A range to map lies above what 4-level paging reaches
    /// ([`MAPPED_LIMIT`]), or its tables exceed [`SMM_MAPPED_TABLES_MAX`].
    Unmappable,
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

/// Compute the base address of the copied SMM handler/data region.
pub fn compute_common_base(layout: &SmramLayout) -> Result<u64, LayoutError> {
    if layout.smram_size == 0 {
        return Err(LayoutError::BadSize);
    }
    let smram_top = layout
        .smram_base
        .checked_add(layout.smram_size)
        .ok_or(LayoutError::Overflow)?;
    let top_reserved = (layout.handler_mem_size as u64)
        .checked_next_multiple_of(SMM_HANDLER_ALIGNMENT)
        .ok_or(LayoutError::Overflow)?
        .checked_add(
            (layout.page_table_size as u64)
                .checked_next_multiple_of(4096)
                .ok_or(LayoutError::Overflow)?,
        )
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
    let per_segment =
        (SMM_CODE_SEGMENT_SIZE - SMM_ENTRY_OFFSET - layout.entry_stub_size as u64) / needed_ss_size;
    if per_segment == 0 {
        return Err(LayoutError::StubDoesNotFit);
    }

    // First segment begins immediately below the top-reserved common/page-table
    // area, then more 64 KiB windows are allocated downward as needed.
    let first_segment_base = common_base
        .checked_sub(SMM_CODE_SEGMENT_SIZE)
        .ok_or(LayoutError::SmramTooSmall)?;

    for (i, slot) in out.iter_mut().take(count).enumerate() {
        let segment = (i as u64) / per_segment;
        let in_segment = (i as u64) % per_segment;
        let smbase = first_segment_base
            .checked_sub(
                segment
                    .checked_mul(SMM_CODE_SEGMENT_SIZE)
                    .ok_or(LayoutError::Overflow)?,
            )
            .and_then(|v| v.checked_sub(in_segment.checked_mul(needed_ss_size)?))
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
        let stack_bottom = layout
            .smram_base
            .checked_add((i as u64) * layout.stack_size as u64)
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
    }

    Ok(&out[..count])
}

/// Return the page-table base reserved immediately above the handler image.
pub fn compute_page_table_base(layout: &SmramLayout) -> Result<u64, LayoutError> {
    compute_common_base(layout)?
        .checked_add(
            (layout.handler_mem_size as u64)
                .checked_next_multiple_of(SMM_HANDLER_ALIGNMENT)
                .ok_or(LayoutError::Overflow)?,
        )
        .ok_or(LayoutError::Overflow)
}

/// Physical base of the identity page tables the default relocation stub loads
/// into CR3.
pub const SMM_RELOCATION_TABLE_OFFSET: u64 = 0x9000;

/// PML4, one PDPT and four page directories mapping the low 4 GiB.
pub const SMM_IDENTITY_TABLE_SIZE: u32 = 6 * 4096;
pub const SMM_RELOCATION_TABLE_SIZE: u64 = SMM_IDENTITY_TABLE_SIZE as u64;

/// A range above 4 GiB the permanent tables map besides the low 4 GiB
/// (MMIO a handler must reach: an ECAM window, a root bridge's windows).
pub type MappedRange = crate::buffer::Region;

/// What 4-level paging (the SMM entry stubs' mode) maps: below 256 TiB.
pub const MAPPED_LIMIT: u64 = 1 << 48;

/// The most bytes of tables [`MappedRange`]s may add to
/// [`SMM_IDENTITY_TABLE_SIZE`]: 64 pages (256 KiB of SMRAM), what a build
/// that checks an image's fit reserves for them.
pub const SMM_MAPPED_TABLES_MAX: u32 = 64 * 4096;

const PAGE: u64 = 4096;
const GIB: u64 = 1 << 30;
const MIB2: u64 = 1 << 21;
/// Bytes one PML4 entry maps.
const SLOT: u64 = 1 << 39;

/// The part of `range` above the low 4 GiB, rounded out to 2 MiB pages:
/// `[base, end)`, `None` if nothing is left. An `Err` for a range above
/// [`MAPPED_LIMIT`] or one that wraps.
fn high_part(range: &MappedRange) -> Result<Option<(u64, u64)>, LayoutError> {
    if range.is_empty() {
        return Ok(None);
    }
    let end = range.end().ok_or(LayoutError::Unmappable)?;
    if end > MAPPED_LIMIT {
        return Err(LayoutError::Unmappable);
    }
    let base = range.base.max(1 << 32) & !(MIB2 - 1);
    let end = end.next_multiple_of(MIB2);
    Ok((base < end).then_some((base, end)))
}

/// Bytes of the permanent identity tables for the low 4 GiB and `mapped`:
/// [`SMM_IDENTITY_TABLE_SIZE`] and, per range, a PDPT for each 512 GiB it
/// touches above the first and a page directory for each GiB it covers
/// only in part (whole GiBs take 1 GiB pages). An upper bound: ranges that
/// share tables are counted each.
///
/// An error for a range above [`MAPPED_LIMIT`], or tables beyond
/// [`SMM_MAPPED_TABLES_MAX`].
pub fn identity_table_size(mapped: &[MappedRange]) -> Result<u32, LayoutError> {
    let mut pages = 0u64;
    for range in mapped {
        let Some((base, end)) = high_part(range)? else {
            continue;
        };
        let (first, last) = (base / SLOT, (end - 1) / SLOT);
        pages += last - first + 1 - u64::from(first == 0);
        let (head, tail) = (base / GIB, (end - 1) / GIB);
        let partial_head = !base.is_multiple_of(GIB) || (head == tail && !end.is_multiple_of(GIB));
        let partial_tail = head != tail && !end.is_multiple_of(GIB);
        pages += u64::from(partial_head) + u64::from(partial_tail);
    }
    let extra = pages.checked_mul(PAGE).ok_or(LayoutError::Unmappable)?;
    if extra > u64::from(SMM_MAPPED_TABLES_MAX) {
        return Err(LayoutError::Unmappable);
    }
    Ok(SMM_IDENTITY_TABLE_SIZE + extra as u32)
}

const PTE_PRESENT: u64 = 1 << 0;
const PTE_WRITABLE: u64 = 1 << 1;
/// PWT and PCD: uncached by the default PAT, whatever the MTRRs say.
const PTE_UNCACHED: u64 = 1 << 3 | 1 << 4;
const PTE_PAGE_SIZE: u64 = 1 << 7;
const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
const ENTRIES: u64 = 512;

/// Build a 4 GiB identity map with 2 MiB pages at an explicitly reserved base.
///
/// # Safety
///
/// `base..base + SMM_IDENTITY_TABLE_SIZE` must be writable and exclusively
/// owned by the caller.
pub unsafe fn build_identity_tables(base: u64) -> u64 {
    // SAFETY: the caller's contract; no ranges need no more than
    // SMM_IDENTITY_TABLE_SIZE.
    match unsafe { build_identity_tables_with(base, &[]) } {
        Ok(cr3) => cr3,
        Err(_) => base,
    }
}

/// Build the identity map of the low 4 GiB (2 MiB pages, cached by the
/// MTRRs) and of the ranges `mapped` above it (1 GiB pages where a range
/// covers a whole GiB, 2 MiB pages, rounded out, elsewhere; uncached:
/// they are MMIO) at an explicitly reserved base. Returns the CR3.
///
/// # Safety
///
/// `base..base + identity_table_size(mapped)` must be writable and
/// exclusively owned by the caller, `base` page aligned; the CPUs that
/// load the tables support 1 GiB pages if a range covers a whole GiB.
pub unsafe fn build_identity_tables_with(
    base: u64,
    mapped: &[MappedRange],
) -> Result<u64, LayoutError> {
    let size = u64::from(identity_table_size(mapped)?);
    let limit = base.checked_add(size).ok_or(LayoutError::Overflow)?;
    let pml4 = base;
    let pdpt = base + PAGE;
    let pds = base + 2 * PAGE;
    let entry = |table: u64, index: u64| (table + 8 * index) as *mut u64;

    // SAFETY: caller guarantees the region is writable; all offsets stay within
    // `size`. Zero every unused entry so stale SMRAM contents cannot create
    // unintended translations.
    unsafe {
        core::ptr::write_bytes(base as *mut u8, 0, size as usize);
        entry(pml4, 0).write(pdpt | PTE_PRESENT | PTE_WRITABLE);
        for i in 0..4 {
            entry(pdpt, i).write((pds + i * PAGE) | PTE_PRESENT | PTE_WRITABLE);
        }
        for i in 0..4 * ENTRIES {
            entry(pds, i).write(i << 21 | PTE_PRESENT | PTE_WRITABLE | PTE_PAGE_SIZE);
        }
    }

    let mut next = base + u64::from(SMM_IDENTITY_TABLE_SIZE);
    // The table an entry points to, a new zeroed one if it is empty.
    let mut table_of = |at: *mut u64| -> Result<u64, LayoutError> {
        // SAFETY: `at` is an entry of a table inside the region.
        let value = unsafe { at.read() };
        if value & PTE_PRESENT != 0 {
            return Ok(value & ADDRESS);
        }
        if next >= limit {
            return Err(LayoutError::SmramTooSmall);
        }
        let table = next;
        next += PAGE;
        // SAFETY: a fresh (zeroed) page of the region.
        unsafe { at.write(table | PTE_PRESENT | PTE_WRITABLE) };
        Ok(table)
    };
    let leaf = PTE_PRESENT | PTE_WRITABLE | PTE_UNCACHED | PTE_PAGE_SIZE;
    for range in mapped {
        let Some((from, to)) = high_part(range)? else {
            continue;
        };
        for gib in from / GIB..=(to - 1) / GIB {
            let table = table_of(entry(pml4, gib / ENTRIES))?;
            let at = entry(table, gib % ENTRIES);
            let (start, stop) = (gib * GIB, (gib + 1) * GIB);
            // SAFETY: an entry of a PDPT inside the region.
            let value = unsafe { at.read() };
            if value & PTE_PAGE_SIZE != 0 {
                // Already a 1 GiB page.
                continue;
            }
            if value & PTE_PRESENT == 0 && from <= start && to >= stop {
                // SAFETY: as above.
                unsafe { at.write(start | leaf) };
                continue;
            }
            let pd = table_of(at)?;
            for page in from.max(start) / MIB2..to.min(stop) / MIB2 {
                // SAFETY: an entry of a page directory inside the region.
                unsafe { entry(pd, page % ENTRIES).write((page * MIB2) | leaf) };
            }
        }
    }
    Ok(base)
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
        let mut tables = std::vec![0xffu8; SMM_IDENTITY_TABLE_SIZE as usize];
        let base = tables.as_mut_ptr() as u64;
        unsafe { build_identity_tables(base) };

        assert!(tables[8..4096].iter().all(|&byte| byte == 0));
        assert!(tables[4096 + 4 * 8..8192].iter().all(|&byte| byte == 0));
    }

    /// Walk the tables at `cr3` (host memory) for `address`: the leaf's
    /// physical address and flags, `None` if unmapped.
    fn walk(cr3: u64, address: u64) -> Option<(u64, u64)> {
        let read = |table: u64, index: u64| unsafe { *((table + 8 * index) as *const u64) };
        let pml4e = read(cr3, address >> 39 & 511);
        if pml4e & PTE_PRESENT == 0 {
            return None;
        }
        let pdpte = read(pml4e & ADDRESS, address >> 30 & 511);
        if pdpte & PTE_PRESENT == 0 {
            return None;
        }
        if pdpte & PTE_PAGE_SIZE != 0 {
            return Some((
                (pdpte & ADDRESS & !(GIB - 1)) | (address & (GIB - 1)),
                pdpte,
            ));
        }
        let pde = read(pdpte & ADDRESS, address >> 21 & 511);
        if pde & PTE_PRESENT == 0 {
            return None;
        }
        Some(((pde & ADDRESS & !(MIB2 - 1)) | (address & (MIB2 - 1)), pde))
    }

    fn build(mapped: &[MappedRange]) -> (std::vec::Vec<u64>, u64) {
        let size = identity_table_size(mapped).unwrap() as usize;
        // Page aligned: one spare page to align in.
        let mut tables = std::vec![u64::MAX; size / 8 + 512];
        let base = (tables.as_mut_ptr() as u64).next_multiple_of(PAGE);
        let cr3 = unsafe { build_identity_tables_with(base, mapped) }.unwrap();
        (tables, cr3)
    }

    /// The MD25-CG0's ECAM (256 MiB at 0x2FFC_8000_0000, in PML4 slot 95):
    /// a PDPT and a page directory more, 2 MiB uncached pages, the 4 GiB
    /// below as before, nothing else above.
    #[test]
    fn a_high_ecam_window_is_mapped_uncached() {
        let ecam = MappedRange::new(0x2ffc_8000_0000, 256 << 20);
        assert_eq!(
            identity_table_size(&[ecam]),
            Ok(SMM_IDENTITY_TABLE_SIZE + 2 * 4096)
        );
        let (_tables, cr3) = build(&[ecam]);
        for address in [ecam.base, ecam.base + 0x123_4567, ecam.end().unwrap() - 4] {
            let (phys, flags) = walk(cr3, address).unwrap();
            assert_eq!(phys, address);
            assert_eq!(flags & PTE_UNCACHED, PTE_UNCACHED);
            assert_ne!(flags & PTE_WRITABLE, 0);
        }
        assert_eq!(walk(cr3, ecam.end().unwrap()), None);
        assert_eq!(walk(cr3, ecam.base - 4), None);
        assert_eq!(walk(cr3, 1 << 32), None);
        let (phys, flags) = walk(cr3, 0xfed8_0000).unwrap();
        assert_eq!(phys, 0xfed8_0000);
        assert_eq!(flags & PTE_UNCACHED, 0, "the low 4 GiB as before");
    }

    /// Whole GiBs take 1 GiB pages; ranges sharing tables share them; an
    /// 8-segment ECAM (2 GiB, aligned) costs a PDPT only.
    #[test]
    fn whole_gibs_take_1gib_pages_and_tables_are_shared() {
        let ecam = MappedRange::new(0x1000_0000_0000, 8 << 28);
        assert_eq!(
            identity_table_size(&[ecam]),
            Ok(SMM_IDENTITY_TABLE_SIZE + 4096)
        );
        // A window from 4 GiB + 3 MiB over 2.5 GiB: slot 0, two partial GiBs.
        let window = MappedRange::new((1 << 32) + (3 << 20), 5 << 29);
        // Overlapping, in the same GiBs.
        let inner = MappedRange::new((1 << 32) + (1 << 30), 1 << 20);
        let mapped = [ecam, window, inner];
        assert_eq!(
            identity_table_size(&mapped),
            Ok(SMM_IDENTITY_TABLE_SIZE + (1 + 2 + 1) * 4096)
        );
        let (_tables, cr3) = build(&mapped);
        let (_, flags) = walk(cr3, ecam.base + (3 << 30) / 2).unwrap();
        assert_ne!(flags & PTE_PAGE_SIZE, 0);
        // The window's head is 2 MiB granular, rounded out.
        assert_eq!(walk(cr3, 1 << 32), None);
        assert!(walk(cr3, (1 << 32) + (2 << 20)).is_some());
        // Its middle GiB was whole: a 1 GiB page, the inner range in it.
        let (phys, _) = walk(cr3, (1 << 32) + (1 << 30) + 0x1234).unwrap();
        assert_eq!(phys, (1 << 32) + (1 << 30) + 0x1234);
        let end = window.end().unwrap();
        assert!(walk(cr3, end - 1).is_some());
        assert_eq!(walk(cr3, end.next_multiple_of(MIB2)), None);
    }

    /// Nothing at or above 256 TiB, nothing that wraps, no more tables than
    /// the reserve; the low 4 GiB need nothing more.
    #[test]
    fn unmappable_ranges_are_refused() {
        assert_eq!(
            identity_table_size(&[MappedRange::new(MAPPED_LIMIT - (1 << 28), 1 << 28)]),
            Ok(SMM_IDENTITY_TABLE_SIZE + 2 * 4096)
        );
        assert_eq!(
            identity_table_size(&[MappedRange::new(MAPPED_LIMIT, 1 << 28)]),
            Err(LayoutError::Unmappable)
        );
        assert_eq!(
            identity_table_size(&[MappedRange::new(u64::MAX - 4, 8)]),
            Err(LayoutError::Unmappable)
        );
        assert_eq!(
            identity_table_size(&[MappedRange::new(0xe000_0000, 256 << 20)]),
            Ok(SMM_IDENTITY_TABLE_SIZE)
        );
        // 65 partial GiBs, each a page directory.
        let many: std::vec::Vec<_> = (0..65u64)
            .map(|i| MappedRange::new((8 + i) << 30, 1 << 21))
            .collect();
        assert_eq!(identity_table_size(&many), Err(LayoutError::Unmappable));
        assert_eq!(
            identity_table_size(&many[..64]),
            Ok(SMM_IDENTITY_TABLE_SIZE + SMM_MAPPED_TABLES_MAX)
        );
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
