#![cfg_attr(not(test), no_std)]
#![cfg_attr(not(any(test, feature = "thread-unsafe")),
            forbid(unsafe_code))]

#[cfg(feature = "thread-unsafe")]
use embassy_sync::blocking_mutex::raw::NoopRawMutex as RawMutex;
#[cfg(not(feature = "thread-unsafe"))]
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex as RawMutex;

use embassy_sync::mutex::Mutex;

#[cfg_attr(feature = "block-con", path = "block-con.rs")]
mod logger;
pub use logger::init;
pub use logger::add_sink;

mod sink;
pub use sink::Sink;

mod hub;

const MAX_SINKS: usize = 8;

fn format(writer : &mut dyn core::fmt::Write, record : &log::Record) {
    let _ = write!(writer, "[{:-5}] ", record.metadata().level());
    let _ = core::fmt::write(writer, *record.args());
}

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
        logger::init();
        log::set_max_level(log::LevelFilter::Trace);
        log::warn!("No sinks added, this warning goes nowhere!");
        log::logger().flush();
    }

    #[test]
    fn say_hello() {
        unsafe {
            #![allow(static_mut_refs)]
            add_sink(&mut SINK);

            let hello = "Say hello through a vector!";
            log::info!("{hello}");
            log::logger().flush();

            assert_eq!((String::from("[INFO ] ") + hello).as_bytes(), SINK.data);
        }
    }
}
