// SPDX-License-Identifier: MIT OR Apache-2.0
//! Cooperative single-core `async` executor and polling primitives for
//! `no_std` firmware.
//!
//! No interrupts, waker queues, or task proc-macros: futures are polled
//! round-robin on the BSP with [`Waker::noop`]. Intended for overlapping
//! high-latency ramstage hardware waits (IPMI KCS, SMU/DXIO training, display
//! panel sequencing, PCIe link training) using standard `async`/`await`.
//!
//! # Example (ramstage with `alloc`)
//!
//! ```ignore
//! fstart_core::task::run(|spawner| async move {
//!     let bmc = spawner.spawn(run_bmc_post_done(clock));
//!     let stage = stage.train_dxio_async(&lanes).await;
//!     let result = bmc.await;
//!     ...
//! });
//! ```

use core::future::{Future, poll_fn};
use core::pin::{Pin, pin};
use core::task::{Context, Poll, Waker};

use crate::services::Timer;

/// Poll a pinned future once with a no-op waker.
#[inline]
pub fn poll_once<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
    let mut cx = Context::from_waker(Waker::noop());
    future.poll(&mut cx)
}

/// Drive `future` to completion on the current core by polling in a spin loop.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        core::hint::spin_loop();
    }
}

/// Yield once so sibling tasks in an [`Executor`] or [`join`] can advance.
pub async fn yield_now() {
    let mut yielded = false;
    poll_fn(|_| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            Poll::Pending
        }
    })
    .await
}

/// Cooperatively wait for at least `us` microseconds on `timer`, yielding
/// between checks so sibling tasks can run during the delay.
pub async fn delay_us(timer: &(impl Timer + ?Sized), us: u64) {
    if us == 0 {
        return;
    }
    let start = timer.timestamp_us();
    while timer.timestamp_us().wrapping_sub(start) < us {
        yield_now().await;
    }
}

/// Cooperatively wait for at least `ms` milliseconds on `timer`.
pub async fn delay_ms(timer: &(impl Timer + ?Sized), ms: u64) {
    delay_us(timer, ms.saturating_mul(1000)).await;
}

/// Cooperatively poll `f` until it returns `Some` or `timeout_us` elapses on
/// `timer`, yielding to sibling tasks between checks.
///
/// Like [`crate::services::poll_until`], `f` is checked once before the
/// deadline is examined and once more after it, so a condition met right at
/// the end is not reported as a timeout.
pub async fn poll_until<T>(
    timer: &(impl Timer + ?Sized),
    timeout_us: u64,
    mut f: impl FnMut() -> Option<T>,
) -> Option<T> {
    let start = timer.timestamp_us();
    loop {
        if let Some(value) = f() {
            return Some(value);
        }
        if timer.timestamp_us().wrapping_sub(start) >= timeout_us {
            return f();
        }
        yield_now().await;
    }
}

/// Poll two futures concurrently until both complete, returning their outputs.
pub async fn join<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    let mut a = pin!(a);
    let mut b = pin!(b);
    let mut a_out = None;
    let mut b_out = None;
    poll_fn(|cx| {
        if a_out.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(cx)
        {
            a_out = Some(v);
        }
        if b_out.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(cx)
        {
            b_out = Some(v);
        }
        if a_out.is_some() && b_out.is_some() {
            match (a_out.take(), b_out.take()) {
                (Some(av), Some(bv)) => Poll::Ready((av, bv)),
                _ => Poll::Pending,
            }
        } else {
            Poll::Pending
        }
    })
    .await
}

/// Poll an array of `N` futures concurrently until all complete, returning
/// their outputs in order.
pub async fn join_array<F: Future, const N: usize>(futures: [F; N]) -> [F::Output; N] {
    let mut futures = pin!(futures);
    let mut outputs: [Option<F::Output>; N] = core::array::from_fn(|_| None);
    let mut remaining = N;
    poll_fn(|cx| {
        for (i, out) in outputs.iter_mut().enumerate() {
            if out.is_none() {
                // SAFETY: `futures` is pinned on the stack and never moved or
                // overwritten; each element is polled at most until `Ready`.
                let fut = unsafe { futures.as_mut().map_unchecked_mut(|arr| &mut arr[i]) };
                if let Poll::Ready(v) = fut.poll(cx) {
                    *out = Some(v);
                    remaining -= 1;
                }
            }
        }
        if remaining == 0 {
            Poll::Ready(core::array::from_fn(|i| {
                outputs[i].take().expect("all futures completed")
            }))
        } else {
            Poll::Pending
        }
    })
    .await
}

#[cfg(any(feature = "alloc", feature = "std"))]
mod exec {
    extern crate alloc;

    use super::*;
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};

    type BoxTask<'scope> = Pin<Box<dyn Future<Output = ()> + 'scope>>;

    struct Inner<'scope> {
        tasks: RefCell<Vec<BoxTask<'scope>>>,
        pending: RefCell<Vec<BoxTask<'scope>>>,
    }

    /// Minimal single-core cooperative executor.
    ///
    /// Spawned tasks are polled round-robin on every `.await` of the root
    /// future. Tasks spawned while polling are staged in `pending` so nested
    /// `spawner.spawn(...)` calls never cause a `RefCell` borrow conflict.
    pub struct Executor<'scope> {
        inner: Rc<Inner<'scope>>,
    }

    impl<'scope> Default for Executor<'scope> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<'scope> Executor<'scope> {
        /// Create an empty executor.
        pub fn new() -> Self {
            Self {
                inner: Rc::new(Inner {
                    tasks: RefCell::new(Vec::new()),
                    pending: RefCell::new(Vec::new()),
                }),
            }
        }

        /// Obtain a [`Spawner`] handle for spawning tasks onto this executor.
        pub fn spawner(&self) -> Spawner<'scope> {
            Spawner {
                inner: Rc::clone(&self.inner),
            }
        }

        /// Poll every live spawned task once, draining any newly spawned tasks
        /// and dropping tasks that complete.
        pub fn poll_tasks(&self) {
            let mut cx = Context::from_waker(Waker::noop());
            {
                let mut tasks = self.inner.tasks.borrow_mut();
                let mut pending = self.inner.pending.borrow_mut();
                tasks.append(&mut pending);
            }
            let mut tasks = self.inner.tasks.borrow_mut();
            tasks.retain_mut(|task| task.as_mut().poll(&mut cx).is_pending());
        }

        /// Drive `root` to completion while round-robin polling all spawned
        /// tasks whenever `root` yields. Any unfinished detached tasks are
        /// dropped when `root` returns.
        pub fn run<F: Future>(&self, root: F) -> F::Output {
            let mut root = pin!(root);
            let mut cx = Context::from_waker(Waker::noop());
            let output = loop {
                if let Poll::Ready(output) = root.as_mut().poll(&mut cx) {
                    break output;
                }
                self.poll_tasks();
                core::hint::spin_loop();
            };
            self.inner.tasks.borrow_mut().clear();
            self.inner.pending.borrow_mut().clear();
            output
        }
    }

    /// Cloneable handle for spawning tasks onto an [`Executor`].
    pub struct Spawner<'scope> {
        inner: Rc<Inner<'scope>>,
    }

    impl<'scope> Clone for Spawner<'scope> {
        fn clone(&self) -> Self {
            Self {
                inner: Rc::clone(&self.inner),
            }
        }
    }

    impl<'scope> Spawner<'scope> {
        /// Spawn `future` onto the executor and return a [`JoinHandle`] that
        /// resolves to its output when `.await`ed.
        pub fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
        where
            F: Future + 'scope,
            F::Output: 'scope,
        {
            let slot: Rc<Cell<Option<F::Output>>> = Rc::new(Cell::new(None));
            let writer = Rc::clone(&slot);
            let task = Box::pin(async move {
                let output = future.await;
                writer.set(Some(output));
            });
            self.inner.pending.borrow_mut().push(task);
            JoinHandle { slot }
        }
    }

    /// Handle to a task spawned via [`Spawner::spawn`]. Awaiting it yields the
    /// task's return value once it finishes.
    pub struct JoinHandle<T> {
        slot: Rc<Cell<Option<T>>>,
    }

    impl<T> Future for JoinHandle<T> {
        type Output = T;

        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<T> {
            match self.slot.take() {
                Some(output) => Poll::Ready(output),
                None => Poll::Pending,
            }
        }
    }

    /// Create a single-core cooperative [`Executor`], pass its [`Spawner`] to
    /// `f`, and drive the returned root future (along with any spawned tasks)
    /// to completion.
    pub fn run<F, Fut, R>(f: F) -> R
    where
        F: FnOnce(Spawner<'static>) -> Fut,
        Fut: Future<Output = R>,
    {
        let exec = Executor::new();
        exec.run(f(exec.spawner()))
    }
}

#[cfg(any(feature = "alloc", feature = "std"))]
pub use exec::{Executor, JoinHandle, Spawner, run};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ManualTimer;
    use core::cell::Cell;

    #[test]
    fn block_on_drives_yield_and_delay() {
        let timer = ManualTimer::ticking();
        let val = block_on(async {
            yield_now().await;
            delay_us(&timer, 5).await;
            42u32
        });
        assert_eq!(val, 42);
        assert!(timer.now() >= 5);
    }

    #[test]
    fn join_interleaves_two_tasks() {
        let trace = Cell::new(0u32);
        let push = |digit: u32| trace.set(trace.get() * 10 + digit);
        let (a, b) = block_on(join(
            async {
                push(1);
                yield_now().await;
                push(3);
                "a"
            },
            async {
                push(2);
                yield_now().await;
                push(4);
                "b"
            },
        ));
        assert_eq!((a, b), ("a", "b"));
        assert_eq!(trace.get(), 1234);
    }

    #[test]
    fn join_array_overlaps_delays() {
        let timer = ManualTimer::ticking();
        let futs = core::array::from_fn::<_, 3, _>(|i| {
            let t = &timer;
            async move {
                delay_us(t, (i as u64 + 1) * 3).await;
                i * 10
            }
        });
        let results = block_on(join_array(futs));
        assert_eq!(results, [0, 10, 20]);
        assert!(timer.now() < 15);
    }

    #[test]
    fn poll_once_steps_future_incrementally() {
        let timer = ManualTimer::new();
        let mut fut = pin!(async {
            delay_us(&timer, 20).await;
            99u32
        });
        assert_eq!(poll_once(fut.as_mut()), Poll::Pending);
        timer.delay_us(10);
        assert_eq!(poll_once(fut.as_mut()), Poll::Pending);
        timer.delay_us(10);
        assert_eq!(poll_once(fut.as_mut()), Poll::Ready(99));
    }

    #[test]
    fn executor_spawns_overlaps_and_awaits_handles() {
        let timer = ManualTimer::ticking();
        let exec = Executor::new();
        let spawner = exec.spawner();
        let (bg_res, fg_res) = exec.run(async {
            let child_spawner = spawner.clone();
            let t = &timer;
            let bg = spawner.spawn(async move {
                delay_us(t, 4).await;
                // Nested spawn while inside a spawned task must not panic.
                let child = child_spawner.spawn(async {
                    yield_now().await;
                    7u32
                });
                100u32 + child.await
            });
            delay_us(&timer, 10).await;
            (bg.await, 200u32)
        });
        assert_eq!((bg_res, fg_res), (107, 200));
        // Overlapped: bg (4) ran during fg (10), not 4 + 10 sequentially.
        assert!(timer.now() < 15);
    }

    #[test]
    fn poll_until_checks_immediately_yields_and_times_out() {
        let timer = ManualTimer::ticking();
        let calls = Cell::new(0u32);
        let found = block_on(poll_until(&timer, 20, || {
            calls.set(calls.get() + 1);
            (calls.get() == 3).then_some(77u32)
        }));
        assert_eq!(found, Some(77));
        assert_eq!(calls.get(), 3);

        let timed_out = block_on(poll_until(&timer, 5, || None::<u32>));
        assert_eq!(timed_out, None);
    }

    #[test]
    fn timer_delay_implements_embedded_hal_async() {
        use crate::services::TimerDelay;
        use embedded_hal_async::delay::DelayNs;

        let timer = ManualTimer::ticking();
        let mut delay = TimerDelay(&timer);
        block_on(async {
            DelayNs::delay_us(&mut delay, 5).await;
        });
        assert!(timer.now() >= 5);
    }
}
