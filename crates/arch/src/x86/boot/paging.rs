//! Identity page tables built in Rust: the map every platform installs in
//! its memory phase, before APs start and before the payload.
//!
//! The entry tables (ROM tables or ones the reset path generates) cover too
//! little, and ROM tables cost a flash read per page walk. An application
//! processor started with INIT+SIPI begins with no cache configured,
//! inherits the BSP's CR3, and needs DRAM-backed tables.
//!
//! [`IdentityTables`] always maps the low 4 GiB plus the spans it is given
//! (RAM, PCI memory BARs), with 1 GiB leaves when the CPU has them (PDPE1GB)
//! and 2 MiB leaves otherwise. PTEs request write-back uniformly; MTRRs stay
//! the source of truth for RAM versus MMIO cacheability.

/// The worst case for the low 4 GiB alone: PML4, PDPT and four page
/// directories of 2 MiB leaves. A fixed buffer of this size suffices for
/// [`IdentityTables::build_in`] without extra spans.
pub const TABLE_BYTES: usize = 6 * 4096;

use fstart_core::services::ServiceError;
use x86_64_crate::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageSize, PageTable, PageTableFlags, PhysFrame,
    Size1GiB, Size2MiB, Size4KiB, mapper::MapToError,
};
use x86_64_crate::{PhysAddr, VirtAddr};

/// Pages are present and writable; leaf entries map 1 GiB or 2 MiB directly.
const TABLE_FLAGS: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::WRITABLE);

/// Whether the CPU maps 1 GiB pages (CPUID 8000_0001h EDX bit 26).
pub fn gib_pages() -> bool {
    crate::x86::cpuid(0x8000_0000).0 >= 0x8000_0001
        && crate::x86::cpuid(0x8000_0001).3 & (1 << 26) != 0
}

/// The page-table frames that map the low 4 GiB plus `spans`: the PML4, a
/// PDPT per 512 GiB and, without 1 GiB leaves, a page directory per GiB.
/// Spans that overlap each other above 4 GiB are counted twice.
pub fn frames_needed(gib: bool, spans: &[(u64, u64)]) -> usize {
    let tables = |shift: u32| {
        let low = (u64::from(u32::MAX)) >> shift;
        let above: u64 = spans
            .iter()
            .filter(|(_, size)| *size != 0)
            .map(|&(base, size)| {
                let first = (base >> shift).max(low + 1);
                let last = base.saturating_add(size - 1) >> shift;
                (last + 1).saturating_sub(first)
            })
            .sum();
        low + 1 + above
    };
    let directories = if gib { 0 } else { tables(30) };
    usize::try_from(1 + tables(39) + directories).unwrap_or(usize::MAX)
}

/// An identity map in a run of 4 KiB frames, owned until installation. The
/// first frame is the PML4. Dropping an uninstalled map leaves the CPU's page
/// tables alone.
pub struct IdentityTables {
    base: u64,
    used: usize,
    capacity: usize,
}

// SAFETY: each frame is a distinct, aligned, zeroed PageTable of the run.
// Firmware memory is identity mapped, so its pointer is its physical address.
unsafe impl FrameAllocator<Size4KiB> for IdentityTables {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        if self.used == self.capacity {
            return None;
        }
        let address = self.base + (self.used * 4096) as u64;
        self.used += 1;
        // SAFETY: an unused, aligned page of the run.
        unsafe { (address as *mut PageTable).write(PageTable::new()) };
        PhysFrame::from_start_address(PhysAddr::try_new(address).ok()?).ok()
    }
}

impl IdentityTables {
    /// Map the low 4 GiB plus `spans` (base, size) with frames from the heap.
    ///
    /// # Safety
    ///
    /// The heap must be identity-mapped DRAM that stays reserved from the
    /// payload until it replaces CR3 (including S3). The spans must be valid
    /// platform physical addresses.
    #[cfg(feature = "alloc")]
    pub unsafe fn build(spans: impl IntoIterator<Item = (u64, u64)>) -> Result<Self, ServiceError> {
        extern crate alloc;
        let spans: alloc::vec::Vec<_> = spans.into_iter().collect();
        let gib = gib_pages();
        let frames = alloc::boxed::Box::<[PageTable]>::new_uninit_slice(frames_needed(gib, &spans));
        let size = core::mem::size_of_val(&*frames);
        // Never freed: the tables outlive the stage, like the heap itself.
        let base = alloc::boxed::Box::leak(frames).as_mut_ptr() as u64;
        Self::map(base, size, gib, spans)
    }

    /// Map the low 4 GiB plus `spans` with frames from the buffer at `base`
    /// of `size` bytes; fails if they run out ([`TABLE_BYTES`] always
    /// suffices without spans).
    ///
    /// # Safety
    ///
    /// The buffer must be 4 KiB aligned, identity mapped, writable, unused,
    /// and stay reserved until the payload replaces CR3.
    pub unsafe fn build_in(
        base: u64,
        size: usize,
        spans: impl IntoIterator<Item = (u64, u64)>,
    ) -> Result<Self, ServiceError> {
        if !base.is_multiple_of(4096) {
            return Err(ServiceError::InvalidParam);
        }
        Self::map(base, size, gib_pages(), spans)
    }

    fn map(
        base: u64,
        size: usize,
        gib: bool,
        spans: impl IntoIterator<Item = (u64, u64)>,
    ) -> Result<Self, ServiceError> {
        let mut tables = Self {
            base,
            used: 0,
            capacity: size / 4096,
        };
        tables.allocate_frame().ok_or(ServiceError::HardwareError)?;
        // SAFETY: the root is a new zeroed page of the run; frame allocation
        // never moves it or aliases its entries.
        let mut mapper =
            unsafe { OffsetPageTable::new(&mut *(base as *mut PageTable), VirtAddr::zero()) };
        for (base, size) in core::iter::once((0, 1u64 << 32)).chain(spans) {
            if gib {
                map_span::<Size1GiB>(&mut mapper, &mut tables, base, size)?;
            } else {
                map_span::<Size2MiB>(&mut mapper, &mut tables, base, size)?;
            }
        }
        Ok(tables)
    }

    /// Load the map into CR3, after writing its frames back to DRAM.
    ///
    /// # Safety
    ///
    /// Called on the BSP before AP startup, with every live instruction,
    /// stack, table and data address identity mapped. The frames must stay
    /// reserved until the payload replaces CR3 (including S3).
    pub unsafe fn install(self) -> u64 {
        // APs walk the tables with caching disabled at first: publish them.
        // SAFETY: the used part of the run.
        unsafe { crate::x86::writeback_cache_range(self.base as *const u8, self.used * 4096) };
        load_cr3(self.base);
        self.base
    }
}

/// Identity map `[base, base + size)` with `S` leaves.
fn map_span<S: PageSize>(
    mapper: &mut OffsetPageTable<'_>,
    frames: &mut IdentityTables,
    base: u64,
    size: u64,
) -> Result<(), ServiceError>
where
    for<'a> OffsetPageTable<'a>: Mapper<S>,
{
    if size == 0 {
        return Ok(());
    }
    let last = base
        .checked_add(size - 1)
        .ok_or(ServiceError::InvalidParam)?;
    let start = PhysFrame::<S>::containing_address(
        PhysAddr::try_new(base).map_err(|_| ServiceError::InvalidParam)?,
    );
    let end = PhysFrame::<S>::containing_address(
        PhysAddr::try_new(last).map_err(|_| ServiceError::InvalidParam)?,
    );
    for frame in PhysFrame::range_inclusive(start, end) {
        let address = VirtAddr::try_new(frame.start_address().as_u64())
            .map_err(|_| ServiceError::InvalidParam)?;
        let page =
            Page::<S>::from_start_address(address).map_err(|_| ServiceError::InvalidParam)?;
        // SAFETY: this disconnected table maps each frame to itself; no
        // active address space or live Rust reference changes.
        match unsafe { mapper.map_to(page, frame, TABLE_FLAGS, frames) } {
            Ok(flush) => flush.ignore(), // This CR3 is not active yet.
            Err(MapToError::PageAlreadyMapped(existing)) if existing == frame => {}
            Err(_) => return Err(ServiceError::HardwareError),
        }
    }
    Ok(())
}

/// A PML4, one PDPT and four page directories: the entry tables' shape (the
/// low 4 GiB in 2 MiB or 1 GiB leaves).
#[repr(C, align(4096))]
pub struct EntryTables(pub [[u64; 512]; 6]);

impl EntryTables {
    pub const fn new() -> Self {
        Self([[0; 512]; 6])
    }
}

impl Default for EntryTables {
    fn default() -> Self {
        Self::new()
    }
}

/// Copy the live entry tables (ROM tables or ones the reset path built) into
/// `tables` and return the new CR3, for a stage that must stop page walks
/// into the flash it is about to program. `None`, with CR3 untouched, when
/// the live tables have another shape.
///
/// # Safety
/// `tables` is identity mapped and stays reserved while CR3 points at it.
pub unsafe fn copy_entry_tables(tables: &mut EntryTables) -> Option<u64> {
    const PRESENT: u64 = 1;
    const LEAF: u64 = 1 << 7;
    const ADDRESS: u64 = 0x000f_ffff_ffff_f000;
    let cr3: u64;
    // SAFETY: reading CR3 has no side effects.
    unsafe {
        core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
    }
    let copy = |table: &mut [u64; 512], entry: u64| {
        // SAFETY: a present entry of the live, identity-mapped tables.
        unsafe {
            core::ptr::copy_nonoverlapping((entry & ADDRESS) as *const [u64; 512], table, 1);
        }
        (table.as_ptr() as u64) | (entry & !ADDRESS)
    };
    let [pml4, pdpt, directories @ ..] = &mut tables.0;
    let root = copy(pml4, cr3);
    // Only PML4[0] maps, and only the low 4 GiB below it.
    if pml4[0] & PRESENT == 0 || pml4[1..].iter().any(|entry| entry & PRESENT != 0) {
        return None;
    }
    pml4[0] = copy(pdpt, pml4[0]);
    if pdpt[4..].iter().any(|entry| entry & PRESENT != 0) {
        return None;
    }
    for (entry, directory) in pdpt[..4].iter_mut().zip(directories) {
        if *entry & PRESENT != 0 && *entry & LEAF == 0 {
            *entry = copy(directory, *entry);
        }
    }
    Some(root)
}

/// Load CR3.
#[inline]
pub fn load_cr3(value: u64) {
    // SAFETY: writing CR3 is side-effect free apart from switching the address
    // space and flushing non-global TLB entries; callers guarantee the new tables
    // map the currently executing code.
    unsafe {
        core::arch::asm!("mov cr3, {0}", in(reg) value, options(nostack, preserves_flags));
    }
}

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::*;
    use x86_64_crate::structures::paging::Translate;
    extern crate std;

    /// A map in leaked host frames, sized like [`IdentityTables::build`].
    fn heap(gib: bool, spans: &[(u64, u64)]) -> Result<IdentityTables, ServiceError> {
        let frames = std::vec![PageTable::new(); frames_needed(gib, spans)].leak();
        let size = core::mem::size_of_val(frames);
        IdentityTables::map(frames.as_mut_ptr() as u64, size, gib, spans.iter().copied())
    }

    fn translate(tables: &mut IdentityTables, address: u64) -> Option<u64> {
        // SAFETY: host allocations have pointer-as-physical addressing for
        // the software walk; these tables are never installed.
        let mapper = unsafe {
            OffsetPageTable::new(&mut *(tables.base as *mut PageTable), VirtAddr::zero())
        };
        mapper
            .translate_addr(VirtAddr::new(address))
            .map(|a| a.as_u64())
    }

    #[test]
    fn final_map_covers_remapped_ram_and_high_pci_bars() {
        for gib in [false, true] {
            let mut tables = heap(
                gib,
                &[
                    (0x100_000000, 0x3c00_0000),
                    (0x200_000000, 0x1000_0000),
                    (0x100_000001, 0x1000), // Overlapping spans are harmless.
                ],
            )
            .unwrap();
            for address in [0, 0xfff_fffff, 0x13bc_00000, 0x13bff_ffff, 0x200_000000] {
                assert_eq!(translate(&mut tables, address), Some(address), "{gib}");
            }
            assert_eq!(translate(&mut tables, 0x240_000000), None);
        }
        // 2 MiB leaves stop at the span; a 1 GiB leaf covers its gigabyte.
        assert_eq!(
            translate(
                &mut heap(false, &[(0x100_000000, 0x3c00_0000)]).unwrap(),
                0x13c00_0000
            ),
            None
        );
        assert_eq!(
            translate(
                &mut heap(true, &[(0x100_000000, 0x3c00_0000)]).unwrap(),
                0x13c00_0000
            ),
            Some(0x13c00_0000)
        );
    }

    #[test]
    fn low_4gib_uses_the_requested_leaf_size() {
        let tables = heap(true, &[]).unwrap();
        assert_eq!(tables.used, 2, "PML4 and one PDPT of 1 GiB leaves");
        let tables = heap(false, &[]).unwrap();
        assert_eq!(tables.used, 6, "PML4, PDPT and four 2 MiB directories");
        assert_eq!(tables.used * 4096, TABLE_BYTES);
        // Leaves map their own address, write-back (no PWT/PCD).
        let pdpt = unsafe { &*((tables.base + 4096) as *const PageTable) };
        let pd = unsafe { &*(pdpt[3].addr().as_u64() as *const PageTable) };
        assert_eq!(pd[511].addr().as_u64(), 0xffe0_0000);
        assert!(pd[511].flags().contains(PageTableFlags::HUGE_PAGE));
        assert!(
            !pd[511]
                .flags()
                .intersects(PageTableFlags::WRITE_THROUGH | PageTableFlags::NO_CACHE)
        );
    }

    #[test]
    fn heap_frames_match_what_disjoint_spans_need() {
        // Below 4 GiB, across a GiB boundary, and past the first 512 GiB.
        let spans = [
            (0x8000_0000, 0x1000),
            (0x1_3fe0_0000, 0x40_0000),
            (1 << 40, 1),
        ];
        for gib in [false, true] {
            let tables = heap(gib, &spans).unwrap();
            assert_eq!(tables.used, frames_needed(gib, &spans), "{gib}");
        }
    }

    #[test]
    fn fixed_buffer_fits_the_low_4gib_and_fails_cleanly_when_full() {
        #[repr(align(4096))]
        struct Storage([u8; TABLE_BYTES]);
        let mut storage = Storage([0xa5; TABLE_BYTES]);
        let base = storage.0.as_mut_ptr() as u64;
        for gib in [false, true] {
            let mut tables = IdentityTables::map(base, TABLE_BYTES, gib, []).unwrap();
            assert_eq!(tables.base, base);
            assert_eq!(translate(&mut tables, 0xffff_ffff), Some(0xffff_ffff));
        }
        // A span above 512 GiB needs another PDPT (and directory).
        assert!(IdentityTables::map(base, TABLE_BYTES, false, [(1 << 40, 0x1000)]).is_err());
    }

    #[test]
    fn final_map_rejects_overflowing_or_noncanonical_spans() {
        for span in [(u64::MAX, 2), (1 << 47, 1)] {
            assert!(heap(false, &[span]).is_err());
        }
    }
}
