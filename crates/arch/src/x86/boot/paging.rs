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

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
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

/// Where the table frames come from.
enum Frames {
    /// Heap pages; the platform keeps the heap reserved through handoff.
    Heap(Vec<Box<PageTable>>),
    /// A fixed, identity-mapped, reserved buffer.
    Fixed {
        base: u64,
        used: usize,
        capacity: usize,
    },
}

/// An identity map, owned until installation. Dropping an uninstalled map
/// leaves the CPU's page tables alone (and frees heap frames).
pub struct IdentityTables {
    frames: Frames,
    root: u64,
}

// SAFETY: each frame is a distinct, aligned, zeroed PageTable retained by
// `frames`. Firmware memory is identity mapped, so its pointer is its
// physical address.
unsafe impl FrameAllocator<Size4KiB> for IdentityTables {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let address = match &mut self.frames {
            Frames::Heap(pages) => {
                let mut page = Box::new(PageTable::new());
                let address = (&mut *page as *mut PageTable) as u64;
                pages.push(page);
                address
            }
            Frames::Fixed {
                base,
                used,
                capacity,
            } => {
                if *used == *capacity {
                    return None;
                }
                let address = *base + (*used * 4096) as u64;
                *used += 1;
                // SAFETY: an unused, aligned page of the caller's buffer.
                unsafe { (address as *mut PageTable).write(PageTable::new()) };
                address
            }
        };
        PhysFrame::from_start_address(PhysAddr::try_new(address).ok()?).ok()
    }
}

impl IdentityTables {
    /// Map the low 4 GiB plus `spans` (base, size) with heap frames.
    ///
    /// # Safety
    ///
    /// The heap must be identity-mapped DRAM that stays reserved from the
    /// payload until it replaces CR3 (including S3). The spans must be valid
    /// platform physical addresses.
    pub unsafe fn build(spans: impl IntoIterator<Item = (u64, u64)>) -> Result<Self, ServiceError> {
        Self::map(Frames::Heap(Vec::new()), gib_pages(), spans)
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
        if base % 4096 != 0 {
            return Err(ServiceError::InvalidParam);
        }
        let frames = Frames::Fixed {
            base,
            used: 0,
            capacity: size / 4096,
        };
        Self::map(frames, gib_pages(), spans)
    }

    fn map(
        frames: Frames,
        gib: bool,
        spans: impl IntoIterator<Item = (u64, u64)>,
    ) -> Result<Self, ServiceError> {
        let mut tables = Self { frames, root: 0 };
        let root = tables.allocate_frame().ok_or(ServiceError::HardwareError)?;
        tables.root = root.start_address().as_u64();
        // SAFETY: the root is a new zeroed page owned by `tables`; frame
        // allocation never moves it or aliases its entries.
        let mut mapper = unsafe {
            OffsetPageTable::new(&mut *(tables.root as *mut PageTable), VirtAddr::zero())
        };
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
        match &self.frames {
            Frames::Heap(pages) => {
                for page in pages {
                    // SAFETY: each page is a live, aligned allocation.
                    unsafe {
                        crate::x86::writeback_cache_range(
                            (&**page as *const PageTable).cast(),
                            core::mem::size_of::<PageTable>(),
                        );
                    }
                }
            }
            Frames::Fixed { base, used, .. } => {
                // SAFETY: the used part of the caller's buffer.
                unsafe { crate::x86::writeback_cache_range(*base as *const u8, *used * 4096) };
            }
        }
        load_cr3(self.root);
        let root = self.root;
        core::mem::forget(self);
        root
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

    fn heap(gib: bool, spans: &[(u64, u64)]) -> Result<IdentityTables, ServiceError> {
        IdentityTables::map(Frames::Heap(Vec::new()), gib, spans.iter().copied())
    }

    fn translate(tables: &mut IdentityTables, address: u64) -> Option<u64> {
        // SAFETY: host allocations have pointer-as-physical addressing for
        // the software walk; these tables are never installed.
        let mapper = unsafe {
            OffsetPageTable::new(&mut *(tables.root as *mut PageTable), VirtAddr::zero())
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
        let Frames::Heap(pages) = &tables.frames else {
            unreachable!()
        };
        assert_eq!(pages.len(), 2, "PML4 and one PDPT of 1 GiB leaves");
        let tables = heap(false, &[]).unwrap();
        let Frames::Heap(pages) = &tables.frames else {
            unreachable!()
        };
        assert_eq!(pages.len(), 6, "PML4, PDPT and four 2 MiB directories");
        // Leaves map their own address, write-back (no PWT/PCD).
        let pdpt = &*pages[1];
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
    fn fixed_buffer_fits_the_low_4gib_and_fails_cleanly_when_full() {
        #[repr(align(4096))]
        struct Storage([u8; TABLE_BYTES]);
        let mut storage = Storage([0xa5; TABLE_BYTES]);
        let base = storage.0.as_mut_ptr() as u64;
        for gib in [false, true] {
            let frames = Frames::Fixed {
                base,
                used: 0,
                capacity: TABLE_BYTES / 4096,
            };
            let mut tables = IdentityTables::map(frames, gib, []).unwrap();
            assert_eq!(tables.root, base);
            assert_eq!(translate(&mut tables, 0xffff_ffff), Some(0xffff_ffff));
        }
        let frames = Frames::Fixed {
            base,
            used: 0,
            capacity: TABLE_BYTES / 4096,
        };
        // A span above 512 GiB needs another PDPT (and directory).
        assert!(IdentityTables::map(frames, false, [(1 << 40, 0x1000)]).is_err());
    }

    #[test]
    fn final_map_rejects_overflowing_or_noncanonical_spans() {
        for span in [(u64::MAX, 2), (1 << 47, 1)] {
            assert!(heap(false, &[span]).is_err());
        }
    }
}
