#![no_std]
#![recursion_limit = "256"]
#![allow(clippy::modulo_one)]

extern crate alloc;

#[cfg(feature = "acpi")]
mod core2_aml;

pub mod ck505;
pub mod gm965;
pub mod gpio_ich;
pub mod ich7;
pub mod ich8;
pub mod pineview;
pub use fstart_arch::cpu_intel::microcode;
pub mod pmio_ich;
pub mod smbus;

/// Lazily heap-allocate the IGD opregion buffer at mainstage.
///
/// A `static` buffer would land in every stage's `.bss` — including the
/// bootblock, whose CAR is as small as 32 KiB on Pineview. The opregion is
/// only initialized post-DRAM, so it lives on the mainstage heap instead.
#[cfg(feature = "ffs-vbt")]
pub(crate) fn igd_opregion_buf(size: usize) -> &'static mut [u8] {
    use core::sync::atomic::{AtomicUsize, Ordering};

    static PTR: AtomicUsize = AtomicUsize::new(0);
    let mut p = PTR.load(Ordering::Relaxed);
    if p == 0 {
        let layout = core::alloc::Layout::from_size_align(size, 4096).expect("opregion layout");
        // SAFETY: non-zero-sized layout; the allocation is never freed (the
        // OS reads it through ASLS for the machine's lifetime).
        p = unsafe { alloc::alloc::alloc_zeroed(layout) } as usize;
        assert!(p != 0, "IGD opregion allocation failed");
        PTR.store(p, Ordering::Relaxed);
    }
    // SAFETY: BSP-only initialization before ASLS handoff; 'static because
    // the allocation is never freed.
    unsafe { core::slice::from_raw_parts_mut(p as *mut u8, size) }
}
