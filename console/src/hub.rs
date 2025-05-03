use heapless::Vec;
use crate::sink::Sink;

pub struct Hub<'a, const N: usize> {
    sinks: Vec<&'a mut (dyn Sink + Send), N>,
}

impl<'a, const N: usize> Hub<'a, N> {
    pub const fn new() -> Self { Self { sinks: Vec::new() } }

    pub fn add(&mut self, sink: &'a mut (dyn Sink + Send)) {
        let _ = self.sinks.push(sink);
    }
}

impl<const N: usize> Sink for Hub<'_, N> {
    fn flush(&mut self) {
        self.sinks.iter_mut().for_each(|s| {
            s.flush();
        });
    }
    fn write(&mut self, bytes: &[u8]) {
        self.sinks.iter_mut().for_each(|s| {
            s.write(bytes);
        });
    }
}
