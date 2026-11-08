// SPDX-License-Identifier: MIT OR Apache-2.0
//! ax-sync's required runtime boundary for a single, nonpreemptible firmware
//! executor. This is NOT an OS scheduler/IRQ provider: no AP/interrupt/NMI may
//! enter these libraries. Unexpected contention fails closed instead of an
//! unbounded busy wait. Opt-in only; other compositions supply their runtime.
use ax_sync::interface::{
    AcquireResult, ContextOps, LOCK_MODE_READ, LOCK_MODE_WRITE, LockMetadata, RwLockOps, SpinOps,
};
use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
struct FirmwareContext;
struct FirmwareSpin;
struct FirmwareRw;
#[ax_crate_interface::impl_interface]
impl ContextOps for FirmwareContext {
    fn enter(context: u8) -> usize {
        assert!(context <= 3);
        0
    }
    fn exit(_context: u8, _state: usize) {}
    fn exit_preempt_from_irq_return(_state: usize) {
        panic!("USB single-executor runtime used from IRQ return")
    }
}
#[ax_crate_interface::impl_interface]
impl SpinOps for FirmwareSpin {
    fn acquire(
        locked: &AtomicBool,
        _metadata: &LockMetadata,
        _address: usize,
        _context: u8,
        _subclass: u32,
        is_try: bool,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        let acquired = locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();
        assert!(
            is_try || acquired,
            "USB single-executor spin-lock contention"
        );
        AcquireResult::new(acquired, 0)
    }
    fn release(locked: &AtomicBool, _address: usize, _context: u8, _state: usize) {
        assert!(locked.swap(false, Ordering::Release));
    }
    fn force_release(locked: &AtomicBool, _address: usize, _context: u8) {
        locked.store(false, Ordering::Release);
    }
    fn is_locked(locked: &AtomicBool) -> bool {
        locked.load(Ordering::Relaxed)
    }
}
fn rw_acquire(state: &AtomicUsize, mode: u8, is_try: bool) -> AcquireResult {
    let old = state.load(Ordering::Relaxed);
    let next = match mode {
        LOCK_MODE_READ if old < usize::MAX - 1 => Some(old + 1),
        LOCK_MODE_WRITE if old == 0 => Some(usize::MAX),
        _ => None,
    };
    let acquired = next.is_some_and(|next| {
        state
            .compare_exchange(old, next, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    });
    assert!(is_try || acquired, "USB single-executor rw-lock contention");
    AcquireResult::new(acquired, 0)
}
fn rw_release(state: &AtomicUsize, mode: u8) {
    match mode {
        LOCK_MODE_READ => {
            let old = state.fetch_sub(1, Ordering::Release);
            assert!(old > 0 && old != usize::MAX);
        }
        LOCK_MODE_WRITE => assert_eq!(state.swap(0, Ordering::Release), usize::MAX),
        _ => panic!("invalid USB rw-lock mode"),
    }
}
#[ax_crate_interface::impl_interface]
impl RwLockOps for FirmwareRw {
    fn acquire(
        state: &AtomicUsize,
        _metadata: &LockMetadata,
        _address: usize,
        _context: u8,
        mode: u8,
        is_try: bool,
        _caller: &'static Location<'static>,
    ) -> AcquireResult {
        rw_acquire(state, mode, is_try)
    }
    fn release(state: &AtomicUsize, _address: usize, _context: u8, _saved: usize, mode: u8) {
        rw_release(state, mode)
    }
    fn force_read_decrement(state: &AtomicUsize, _address: usize, _context: u8) {
        rw_release(state, LOCK_MODE_READ)
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn runtime_supports_nested_readers_and_exclusive_try_lock() {
        let lock = ax_sync::SpinRwLock::new(5u32);
        let a = lock.read();
        let b = lock.read();
        assert_eq!(*a, *b);
        assert!(lock.try_write().is_none());
        drop(a);
        drop(b);
        *lock.write() = 9;
        assert_eq!(*lock.read(), 9);
        let lock = ax_sync::SpinLock::new(2u32);
        let mut guard = lock.lock();
        *guard = 3;
        assert!(lock.try_lock().is_none());
        drop(guard);
        assert_eq!(*lock.lock(), 3);
    }
}
