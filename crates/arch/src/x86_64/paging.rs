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

use x86_64_crate::PhysAddr;
use x86_64_crate::structures::paging::{PageTable, PageTableFlags};

/// Pages are present and writable; leaf entries map 2 MiB directly.
const TABLE_FLAGS: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::WRITABLE);
const LEAF_FLAGS: PageTableFlags = TABLE_FLAGS.union(PageTableFlags::HUGE_PAGE);
/// Write-through plus cache-disable selects PAT entry 3 (uncached).
const MMIO_FLAGS: PageTableFlags = LEAF_FLAGS
    .union(PageTableFlags::WRITE_THROUGH)
    .union(PageTableFlags::NO_CACHE);

/// First address that is MMIO rather than RAM on this platform: GMADR and the
/// display BAR sit at 0x8000_0000 and everything above is MMIO/flash/APIC.
const MMIO_START: u64 = 0x8000_0000;

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
            // The display MMIO lives above MMIO_START; mapping it write-back
            // makes the DPLL/PIPECONF writes vanish into the cache.
            let flags = if addr >= MMIO_START {
                MMIO_FLAGS
            } else {
                LEAF_FLAGS
            };
            pd[j as usize].set_addr(PhysAddr::new(addr), flags);
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
        assert_eq!(
            last,
            0xffe0_0000 | MMIO_FLAGS.bits(),
            "MMIO must be uncached"
        );
        // RAM stays write-back; the first MMIO leaf (0x8000_0000) is uncached.
        assert_eq!(
            unsafe { base.add(1024).read_volatile() }
                & (PageTableFlags::WRITE_THROUGH | PageTableFlags::NO_CACHE).bits(),
            0
        );
        assert_eq!(
            unsafe { base.add(1024 + 1024).read_volatile() }
                & (PageTableFlags::WRITE_THROUGH | PageTableFlags::NO_CACHE).bits(),
            (PageTableFlags::WRITE_THROUGH | PageTableFlags::NO_CACHE).bits()
        );

        // Every leaf must be a present 2 MiB page at its own address.
        for index in 0..2048u64 {
            let entry = unsafe { base.add(1024 + index as usize).read_volatile() };
            assert_eq!(entry & LEAF_FLAGS.bits(), LEAF_FLAGS.bits());
            assert_eq!(entry & !0xfff & !((1 << 21) - 1), index * (1 << 21));
        }
    }
}
