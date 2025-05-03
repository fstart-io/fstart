use core::fmt;
use core::cell::LazyCell;
use embassy_sync::pipe;
use embassy_executor::raw::Executor;
use crate::{MAX_SINKS, RawMutex, Guard, sink::Sink, hub::Hub, format};

const PIPEDEPTH: usize = 512;
type Pipe = pipe::Pipe<RawMutex, PIPEDEPTH>;

pub fn init() {
    unsafe {
        #[allow(static_mut_refs)]
        let _ = EXEC.spawner().spawn(forward_formatted_task(&LOGGER.pipe, &LOGGER.hub));
    }
    let _ = log::set_logger(&LOGGER);
}

pub fn add_sink(sink: &'static mut (dyn Sink + Send)) {
    LOGGER.hub.lock_mut(|hub| {
        hub.add(sink);
    });
}

static LOGGER: Logger = Logger { hub: Guard::new(Hub::new()), pipe: Pipe::new() };

struct Logger<'a> {
    // Hub is only guarded because we allow calls to `add_sink()` at runtime.
    // Could we "send" sinks to it instead? (and move the hub to the async task)
    hub: Guard<Hub<'a, MAX_SINKS>>,
    pipe: Pipe,
}

impl log::Log for Logger<'_> {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Trace
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            format(&mut AsyncWrite { 
                            pipe: &self.pipe, },
                    record);
        }
    }

    fn flush(&self) {
        while !self.pipe.is_empty() {
            // SAFETY
            // Seriously unsafe, but poll() demands a 'static ref,
            // so we can't guard it?
            unsafe {
                #[allow(static_mut_refs)]
                EXEC.poll();
            }
        }
    }
}

struct AsyncWrite<'a> {
    pipe: &'a Pipe,
}

impl fmt::Write for AsyncWrite<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let s = s.as_bytes();
        let mut n = 0;
        while n < s.len() {
            match self.pipe.try_write(&s[n..]) {
                Ok(m) =>
                    n += m,
                _ => {
                    // SAFETY
                    // Seriously unsafe, but poll() demands a 'static ref,
                    // so we can't guard it?
                    unsafe {
                        #[allow(static_mut_refs)]
                        EXEC.poll();
                    }
                }
            }
        }
        Ok(())
    }
}

static mut EXEC: LazyCell<Executor> = LazyCell::new(|| {
    Executor::new(0 as *mut ())
});

#[embassy_executor::task]
async fn forward_formatted_task(
    pipe: &'static Pipe, hub: &'static Guard<Hub<'static, MAX_SINKS>>)
{
    let mut buf = [0; 64];
    loop {
        let n = pipe.read(&mut buf).await;
        hub.lock_mut(|hub| {
            hub.write(&buf[..n]);
        });
    }
}

#[cfg(test)]
mod test {
    #[unsafe(export_name = "__pender")]
    fn embassy_pender(_ctx: *mut ()) { }
}
