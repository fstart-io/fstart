use core::fmt::{Write, Result};

pub trait Sink {
    fn flush(&mut self) {}

    fn write(&mut self, bytes: &[u8]);

    fn as_write(&mut self) -> SinkWrite<Self> where Self: Sized {
        SinkWrite { sink: self }
    }
}

pub struct SinkWrite<'a, S: Sink> {
    sink: &'a mut S
}

impl<S: Sink> Write for SinkWrite<'_, S> {
    fn write_str(&mut self, s: &str) -> Result {
        self.sink.write(s.as_bytes());
        Ok(())
    }
}
