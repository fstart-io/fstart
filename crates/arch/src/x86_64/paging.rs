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

/// Pages are present and writable; leaf entries map 2 MiB directly.
const PTE_PRESENT: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
/// Page-level write-through and cache-disable: together these select PAT entry
/// 3, i.e. uncached. MMIO must not be mapped write-back, or writes are absorbed
/// by the cache and never reach the device.
const PTE_PWT: u64 = 1 << 3;
const PTE_PCD: u64 = 1 << 4;
const PTE_PS: u64 = 1 << 7;

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
    let base = tables_phys as *mut u64;
    // Layout: PML4 [0..512), PDPT [512..1024), four page directories [1024..3072).
    let pdpt = unsafe { base.add(512) };
    let pds = unsafe { base.add(1024) };
    let pdpt_phys = tables_phys + 4096;
    let pds_phys = tables_phys + 8192;

    unsafe {
        base.write_volatile(pdpt_phys | PTE_PRESENT | PTE_RW);
        for i in 0..4u64 {
            // PDPT entry -> the page directory covering this 1 GiB chunk.
            pdpt.add(i as usize)
                .write_volatile(pds_phys + i * 4096 + PTE_PRESENT | PTE_RW);
            for j in 0..512u64 {
                let addr = i * (1 << 30) + j * (1 << 21);
                // The display MMIO lives above MMIO_START; mapping it write-back
                // makes the DPLL/PIPECONF writes vanish into the cache, which is
                // what stopped the pipe from enabling.
                let cache = if addr >= MMIO_START {
                    PTE_PWT | PTE_PCD
                } else {
                    0
                };
                pds.add((i * 512 + j) as usize)
                    .write_volatile(addr | PTE_PRESENT | PTE_RW | PTE_PS | cache);
            }
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
        assert_eq!(pml4, phys + 4096 + PTE_PRESENT + PTE_RW);
        assert_eq!(pml4 & PTE_PRESENT, PTE_PRESENT, "low 4 GiB must be mapped");

        let pdpt = unsafe { base.add(512).read_volatile() };
        assert_eq!(pdpt, phys + 8192 + PTE_PRESENT + PTE_RW);

        // First and last leaf entries: 0 and 4 GiB - 2 MiB, both 2 MiB pages.
        let first = unsafe { base.add(1024).read_volatile() };
        assert_eq!(first, PTE_PRESENT | PTE_RW | PTE_PS);
        let last = unsafe { base.add(1024 + 2047).read_volatile() };
        assert_eq!(
            last,
            0xffe0_0000 | PTE_PRESENT | PTE_RW | PTE_PS | PTE_PWT | PTE_PCD,
            "MMIO must be uncached"
        );
        // RAM stays write-back; the first MMIO leaf (0x8000_0000) is uncached.
        assert_eq!(
            unsafe { base.add(1024).read_volatile() } & (PTE_PWT | PTE_PCD),
            0
        );
        assert_eq!(
            unsafe { base.add(1024 + 1024).read_volatile() } & (PTE_PWT | PTE_PCD),
            PTE_PWT | PTE_PCD
        );

        // Every leaf must be a present 2 MiB page at its own address.
        for index in 0..2048u64 {
            let entry = unsafe { base.add(1024 + index as usize).read_volatile() };
            assert_eq!(
                entry & (PTE_PRESENT | PTE_RW | PTE_PS),
                PTE_PRESENT | PTE_RW | PTE_PS
            );
            assert_eq!(entry & !0xfff & !((1 << 21) - 1), index * (1 << 21));
        }
    }
}
