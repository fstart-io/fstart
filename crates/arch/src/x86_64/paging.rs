//! Identity page tables in DRAM.
//!
//! The early assembly builds its page tables into link-time statics that live in
//! the cache-as-RAM window at the top of the 4 GiB space. Those tables are backed
//! by the boot CPU's cache, so only that CPU can read them. An application
//! processor started with INIT+SIPI begins in real mode with no cache
//! configured, reads garbage where the tables should be, triple-faults, and the
//! chipset turns that into a platform reset.
//!
//! Postcar already runs from DRAM, so it builds a replacement set there and loads
//! it. Every stage and CPU afterwards then shares one DRAM-backed address space,
//! which is what the AP trampoline needs when it inherits the BSP's CR3.

/// Space the tables need: one PML4, one PDPT and four page directories.
pub const TABLE_BYTES: usize = 6 * 4096;

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use fstart_core::services::ServiceError;
use x86_64_crate::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, PageTable, PageTableFlags, PhysFrame, Size2MiB,
    Size4KiB, mapper::MapToError,
};
use x86_64_crate::{PhysAddr, VirtAddr};

/// Pages are present and writable; leaf entries map 2 MiB directly.
const TABLE_FLAGS: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::WRITABLE);
const LEAF_FLAGS: PageTableFlags = TABLE_FLAGS.union(PageTableFlags::HUGE_PAGE);

/// Build identity tables for the low 4 GiB at `tables_phys`.
///
/// Returns `tables_phys` so callers can hand it to [`load_cr3`]. Mapping the
/// whole low 4 GiB covers RAM and every MMIO window the firmware touches, so a
/// driver that relied on the old map cannot regress; MTRRs still decide the
/// caching attributes of the MMIO ranges.
///
/// # Safety
///
/// `tables_phys` must be 4 KiB aligned, writable, identity mapped in the current
/// address space, and must have [`TABLE_BYTES`] free. The result stays live until
/// the payload replaces CR3, so the region must remain reserved.
pub unsafe fn build_identity_tables(tables_phys: u64) -> u64 {
    let pml4 = unsafe { &mut *(tables_phys as *mut PageTable) };
    let pdpt = unsafe { &mut *((tables_phys + 4096) as *mut PageTable) };
    let pdpt_phys = tables_phys + 4096;
    let pds_phys = tables_phys + 8192;

    pml4.zero();
    pdpt.zero();
    pml4[0].set_addr(PhysAddr::new(pdpt_phys), TABLE_FLAGS);
    for i in 0..4u64 {
        let pd_phys = pds_phys + i * 4096;
        let pd = unsafe { &mut *(pd_phys as *mut PageTable) };
        pd.zero();
        pdpt[i as usize].set_addr(PhysAddr::new(pd_phys), TABLE_FLAGS);
        for j in 0..512u64 {
            let addr = i * (1 << 30) + j * (1 << 21);
            // PTEs request write-back uniformly. Platform MTRRs remain the
            // source of truth for RAM versus MMIO cacheability, so valid RAM
            // above 2 GiB is not accidentally forced uncached here.
            pd[j as usize].set_addr(PhysAddr::new(addr), LEAF_FLAGS);
        }
    }
    tables_phys
}

/// Build the tables and load them into CR3, flushing the TLB as a side effect.
///
/// # Safety
///
/// As [`build_identity_tables`], and the new tables must map every address this
/// CPU will execute or touch after the switch — including its own stack.
pub unsafe fn install_identity_tables(tables_phys: u64) -> u64 {
    let base = unsafe { build_identity_tables(tables_phys) };
    // APs begin after INIT with caching disabled. Make the page tables visible
    // in DRAM rather than leaving any entries dirty in the BSP's cache.
    unsafe { crate::x86::writeback_cache_range(base as *const u8, TABLE_BYTES) };
    load_cr3(base);
    base
}

/// Final identity map, owned until installation. All backing pages are heap
/// allocations, so the platform must reserve that heap through payload handoff.
/// Dropping an uninstalled map frees it without changing the CPU's page tables.
pub struct IdentityTables {
    pages: Vec<Box<PageTable>>,
}

// SAFETY: each allocation is a distinct, aligned PageTable retained in `pages`.
// Firmware's heap is identity mapped, so its pointer is its physical address.
unsafe impl FrameAllocator<Size4KiB> for IdentityTables {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        let mut page = Box::new(PageTable::new());
        let address = PhysAddr::try_new((&mut *page as *mut PageTable) as u64).ok()?;
        let frame = PhysFrame::from_start_address(address).ok()?;
        self.pages.push(page);
        Some(frame)
    }
}

impl IdentityTables {
    /// Build a final map, retaining the bootstrap low-4-GiB coverage and adding
    /// the supplied physical spans (RAM and allocated PCI memory BARs).
    ///
    /// Spans are covered by 2-MiB leaves; no 1-GiB-page CPU support is needed.
    /// PTEs leave cacheability to platform MTRRs, like the bootstrap map.
    ///
    /// # Safety
    ///
    /// The allocator must supply identity-mapped DRAM that remains reserved
    /// from the payload. The spans must be valid platform physical addresses.
    pub unsafe fn build(spans: impl IntoIterator<Item = (u64, u64)>) -> Result<Self, ServiceError> {
        let mut tables = Self { pages: Vec::new() };
        let root = tables.allocate_frame().ok_or(ServiceError::HardwareError)?;
        // SAFETY: the root is a new zeroed page owned by `tables`; subsequent
        // frame allocation cannot move its boxed storage or alias its entries.
        let mut mapper = unsafe {
            OffsetPageTable::new(
                &mut *(root.start_address().as_u64() as *mut PageTable),
                VirtAddr::zero(),
            )
        };
        for (base, size) in core::iter::once((0, 1u64 << 32)).chain(spans) {
            if size == 0 {
                continue;
            }
            let last = base
                .checked_add(size - 1)
                .ok_or(ServiceError::InvalidParam)?;
            let start = PhysFrame::<Size2MiB>::containing_address(
                PhysAddr::try_new(base).map_err(|_| ServiceError::InvalidParam)?,
            );
            let end = PhysFrame::<Size2MiB>::containing_address(
                PhysAddr::try_new(last).map_err(|_| ServiceError::InvalidParam)?,
            );
            for frame in PhysFrame::range_inclusive(start, end) {
                let address = VirtAddr::try_new(frame.start_address().as_u64())
                    .map_err(|_| ServiceError::InvalidParam)?;
                // SAFETY: this disconnected table maps each frame to itself;
                // no active address space or live Rust references are changed.
                match unsafe {
                    mapper.map_to(
                        x86_64_crate::structures::paging::Page::from_start_address(address)
                            .map_err(|_| ServiceError::InvalidParam)?,
                        frame,
                        TABLE_FLAGS,
                        &mut tables,
                    )
                } {
                    Ok(flush) => flush.ignore(), // This CR3 is not active yet.
                    Err(MapToError::PageAlreadyMapped(existing)) if existing == frame => {}
                    Err(_) => return Err(ServiceError::HardwareError),
                }
            }
        }
        Ok(tables)
    }

    /// Publish the completed map and retain its storage for the boot lifetime.
    ///
    /// # Safety
    ///
    /// Called on the BSP before AP startup, with every live instruction, stack,
    /// table and data address identity mapped. The heap holding these tables
    /// must remain reserved until the payload replaces CR3 (including S3).
    pub unsafe fn install(self) -> u64 {
        let root = (&*self.pages[0] as *const PageTable) as u64;
        for page in &self.pages {
            // SAFETY: each page is a live, aligned allocation. APs initially
            // walk the tables with caching disabled, so publish all entries.
            unsafe {
                crate::x86::writeback_cache_range(
                    (&**page as *const PageTable).cast(),
                    core::mem::size_of::<PageTable>(),
                );
            }
        }
        load_cr3(root);
        core::mem::forget(self);
        root
    }
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

    #[repr(align(4096))]
    struct Storage([u8; TABLE_BYTES]);

    #[test]
    fn final_map_covers_remapped_ram_and_high_pci_bars() {
        use x86_64_crate::structures::paging::Translate;
        // SAFETY: host allocations have pointer-as-physical addressing for the
        // software walk; these tables are never installed into the host CR3.
        let mut tables = unsafe {
            IdentityTables::build([
                (0x100_000000, 0x3c00_0000),
                (0x200_000000, 0x1000_0000),
                (0x100_000001, 0x1000), // Overlapping spans are harmless.
            ])
        }
        .unwrap();
        let mapper = unsafe { OffsetPageTable::new(&mut tables.pages[0], VirtAddr::zero()) };
        for address in [0, 0xfff_fffff, 0x13bc_00000, 0x13bff_ffff, 0x200_000000] {
            assert_eq!(
                mapper.translate_addr(VirtAddr::new(address)),
                Some(PhysAddr::new(address))
            );
        }
        assert_eq!(mapper.translate_addr(VirtAddr::new(0x13c00_0000)), None);
        assert_eq!(mapper.translate_addr(VirtAddr::new(0x210_000000)), None);
    }

    #[test]
    fn final_map_rejects_overflowing_or_noncanonical_spans() {
        for span in [(u64::MAX, 2), (1 << 47, 1)] {
            assert!(unsafe { IdentityTables::build([span]) }.is_err());
        }
    }

    #[test]
    fn builds_identity_entries_for_the_low_4gib() {
        let mut storage = Storage([0; TABLE_BYTES]);
        let phys = storage.0.as_mut_ptr() as u64;
        // SAFETY: the buffer is aligned, writable and large enough; the tables are
        // never installed into CR3 here, so address space is untouched.
        let built = unsafe { build_identity_tables(phys) };
        assert_eq!(built, phys);

        let base = storage.0.as_ptr() as *const u64;
        let pml4 = unsafe { base.read_volatile() };
        assert_eq!(pml4, phys + 4096 + TABLE_FLAGS.bits());
        assert_eq!(
            pml4 & PageTableFlags::PRESENT.bits(),
            PageTableFlags::PRESENT.bits(),
            "low 4 GiB must be mapped"
        );

        let pdpt = unsafe { base.add(512).read_volatile() };
        assert_eq!(pdpt, phys + 8192 + TABLE_FLAGS.bits());

        // First and last leaf entries: 0 and 4 GiB - 2 MiB, both 2 MiB pages.
        let first = unsafe { base.add(1024).read_volatile() };
        assert_eq!(first, LEAF_FLAGS.bits());
        let last = unsafe { base.add(1024 + 2047).read_volatile() };
        assert_eq!(last, 0xffe0_0000 | LEAF_FLAGS.bits());
        // Page tables do not duplicate platform cacheability policy.
        for index in [0usize, 1024, 2047] {
            assert_eq!(
                unsafe { base.add(1024 + index).read_volatile() }
                    & (PageTableFlags::WRITE_THROUGH | PageTableFlags::NO_CACHE).bits(),
                0
            );
        }

        // Every leaf must be a present 2 MiB page at its own address.
        for index in 0..2048u64 {
            let entry = unsafe { base.add(1024 + index as usize).read_volatile() };
            assert_eq!(entry & LEAF_FLAGS.bits(), LEAF_FLAGS.bits());
            assert_eq!(entry & !0xfff & !((1 << 21) - 1), index * (1 << 21));
        }
    }
}
