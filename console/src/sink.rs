use core::fmt::{Write, Result};

pub trait Sink {
    fn flush(&mut self) {}

    fn write(&mut self, bytes: &[u8]);

    fn as_write(&mut self) -> SinkWrite where Self: Sized {
        SinkWrite { sink: self }
    }
}

pub struct SinkWrite<'a> {
    sink: &'a mut dyn Sink
}

impl Write for SinkWrite<'_> {
    fn write_str(&mut self, s: &str) -> Result {
        self.sink.write(s.as_bytes());
        Ok(())
    }
}
