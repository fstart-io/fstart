#![cfg_attr(not(test), no_std)]

mod sink;
pub use sink::Sink;

mod hub;

const MAX_SINKS: usize = 8;

#[cfg(test)]
mod tests {
    use std::vec::Vec;
    use super::*;

    static mut HUB: hub::Hub<MAX_SINKS> = hub::Hub::new();
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
        unsafe {
            #![allow(static_mut_refs)]
            HUB.write("No sinks added, this warning goes nowhere!".as_bytes());
            HUB.flush();
        }
    }

    #[test]
    fn say_hello() {
        unsafe {
            #![allow(static_mut_refs)]
            HUB.add(&mut SINK);

            let hello = "Say hello through a vector!";
            HUB.write(hello.as_bytes());
            HUB.flush();

            assert_eq!(hello.as_bytes(), SINK.data);
        }
    }
}
