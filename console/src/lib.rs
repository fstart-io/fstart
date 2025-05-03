#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(any(test, feature = "thread-unsafe")),
            forbid(unsafe_code))]

#[cfg(feature = "thread-unsafe")]
use embassy_sync::blocking_mutex::raw::NoopRawMutex as RawMutex;
#[cfg(not(feature = "thread-unsafe"))]
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as RawMutex;

use embassy_sync::mutex::Mutex;


mod sink;
pub use sink::Sink;

mod hub;

const MAX_SINKS: usize = 8;

struct Guard<T> {
    inner: Mutex<RawMutex, T>
}

// SAFETY: Fake `Sync` only if we build explicitly without thread safety.
#[cfg(feature = "thread-unsafe")]
unsafe impl<T> Sync for Guard<T> { }

impl<T> Guard<T> {
    const fn new(value: T) -> Self { Self { inner: Mutex::new(value) } }

    fn lock_mut<U>(&self, f: impl FnOnce(&mut T) -> U) -> U {
        // Block manually on non-blocking mutex.
        loop {
            // Console sinks trying to log something would deadlock here.
            // This could be avoided with some thread-local state, telling
            // us if the current thread runs console code.
            match self.inner.try_lock() {
                Ok(mut val) => {
                    return f(&mut val);
                }
                _ => ()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::vec::Vec;
    use super::*;

    static HUB: Guard<hub::Hub<MAX_SINKS>> = Guard::new(hub::Hub::new());
    static mut SINK: VecSink = VecSink { data: Vec::new() };

    struct VecSink {
        data: Vec<u8>,
    }

    impl Sink for VecSink {
        fn write(&mut self, bytes: &[u8]) {
            self.data.extend(bytes);
        }
    }

    #[test]
    fn log_nowhere() {
        HUB.lock_mut(|hub| {
            hub.write("No sinks added, this warning goes nowhere!".as_bytes());
            hub.flush();
        });
    }

    #[test]
    fn say_hello() {
        unsafe {
            #![allow(static_mut_refs)]
            let hello = "Say hello through a vector!";

            HUB.lock_mut(|hub| {
                hub.add(&mut SINK);

                hub.write(hello.as_bytes());
                hub.flush();
            });

            assert_eq!(hello.as_bytes(), SINK.data);
        }
    }
}
