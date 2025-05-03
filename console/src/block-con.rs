use crate::{MAX_SINKS, Guard, sink::Sink, hub::Hub, format};

pub fn init() {
    let _ = log::set_logger(&LOGGER);
}

pub fn add_sink(sink: &'static mut (dyn Sink + Send)) {
    LOGGER.hub.lock_mut(|hub| {
        hub.add(sink);
    });
}

static LOGGER: Logger = Logger { hub: Guard::new(Hub::new()) };

struct Logger<'a> {
    hub: Guard<Hub<'a, MAX_SINKS>>,
}

impl log::Log for Logger<'_> {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Trace
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            self.hub.lock_mut(|hub| {
                format(&mut hub.as_write(), record);
            });
        }
    }

    fn flush(&self) {
        self.hub.lock_mut(|hub| {
            hub.flush();
        });
    }
}
