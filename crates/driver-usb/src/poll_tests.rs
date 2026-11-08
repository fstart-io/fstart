// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
struct Clock(AtomicU64);
impl Timer for Clock {
    fn delay_us(&self, _: u64) {}
    fn timestamp_us(&self) -> u64 {
        self.0.fetch_add(1_000_000, Ordering::Relaxed)
    }
}
struct Pending(Arc<AtomicBool>);
impl Future for Pending {
    type Output = ();
    fn poll(self: core::pin::Pin<&mut Self>, _: &mut Context<'_>) -> Poll<()> {
        Poll::Pending
    }
}
impl Drop for Pending {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}
#[test]
fn timeout_quarantines_inflight_future_instead_of_cancelling_or_reusing_it() {
    let dropped = Arc::new(AtomicBool::new(false));
    let clock = Clock(AtomicU64::new(0));
    assert!(matches!(
        run(Pending(dropped.clone()), None, &clock),
        Err(Error::Timeout)
    ));
    assert!(!dropped.load(Ordering::Relaxed));
    assert!(clock.0.load(Ordering::Relaxed) <= 22_000_000);
}
#[test]
fn capability_bounds_use_the_xhci_register_library_before_any_dma() {
    #[repr(align(4096))]
    struct Bar([u8; 0x4000]);
    let mut bar = Bar([0; 0x4000]);
    for (offset, word) in [
        (0, 0x01200020u32),
        (4, 0x04000840),
        (8, 2 << 27),
        (0x14, 0x2000),
        (0x18, 0x3000),
    ] {
        bar.0[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
    }
    let base = bar.0.as_ptr() as usize;
    let cap = unsafe { capabilities(base, bar.0.len()) }.unwrap();
    assert_eq!(
        (cap.version, cap.slots, cap.ports, cap.scratchpads),
        (0x120, 64, 4, 2)
    );
    bar.0[0x14..0x18].copy_from_slice(&0xfffffffcu32.to_le_bytes());
    assert!(matches!(
        unsafe { capabilities(base, bar.0.len()) },
        Err(Error::Mmio)
    ));
    assert!(matches!(
        unsafe { capabilities(0, 0x4000) },
        Err(Error::Mmio)
    ));
    assert!(matches!(
        unsafe { capabilities(base, 16) },
        Err(Error::Mmio)
    ));
}
#[test]
fn persistent_reads_refuse_wrap_out_of_capacity_unaligned_and_unbudgeted_requests() {
    assert_eq!(read_extent(1024, 512, 1023, 512, 4096), Ok(()));
    for (lba, bytes, budget) in [
        (1024, 512, 4096),
        (u64::MAX, 512, 4096),
        (0, 513, 4096),
        (0, 0, 4096),
        (0, 4096, 512),
    ] {
        assert_eq!(
            read_extent(1024, 512, lba, bytes, budget),
            Err(Error::Policy)
        );
    }
    assert_eq!(read_extent(1024, 0, 0, 512, 4096), Err(Error::Policy));
}
#[test]
fn ready_future_is_returned_without_touching_event_mmio() {
    assert_eq!(run(async { 7 }, None, &Clock(AtomicU64::new(0))), Ok(7));
}
