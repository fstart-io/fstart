use fstart_core::services::{Console, ServiceError};
use fstart_log::{ConsoleMux, StaticConsole};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};

#[derive(Default)]
struct Backend {
    failed: AtomicBool,
    recurse: AtomicBool,
    input: Mutex<Option<u8>>,
    output: Mutex<Vec<u8>>,
}

impl Console for Backend {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        if self.failed.load(Ordering::Relaxed) {
            return Err(ServiceError::HardwareError);
        }
        if self.recurse.load(Ordering::Relaxed) {
            assert_eq!(ConsoleMux.write_byte(b'!'), Err(ServiceError::IoError));
        }
        self.output.lock().unwrap().push(byte);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        if self.failed.load(Ordering::Relaxed) {
            return Err(ServiceError::HardwareError);
        }
        Ok(self.input.lock().unwrap().take())
    }

    fn flush(&self) -> Result<(), ServiceError> {
        if self.failed.load(Ordering::Relaxed) {
            Err(ServiceError::HardwareError)
        } else {
            Ok(())
        }
    }
}

// Registration is global, so exercise the state transitions in one test.
// Run this binary both with and without debug-consoles.
#[test]
fn mux_preserves_errors_priority_and_nonblocking_serialization() {
    assert_eq!(
        ConsoleMux.write_byte(b'?'),
        Err(ServiceError::NotInitialized)
    );
    assert_eq!(ConsoleMux.read_byte(), Err(ServiceError::NotInitialized));
    assert_eq!(ConsoleMux.flush(), Err(ServiceError::NotInitialized));

    static BOARD: StaticConsole<Backend> = StaticConsole::new();
    let board = BOARD.install(Backend::default());
    ConsoleMux.write_bytes(b"board").unwrap();
    assert_eq!(&*board.output.lock().unwrap(), b"board");
    assert_eq!(ConsoleMux.read_byte(), Ok(None));

    board.failed.store(true, Ordering::Relaxed);
    assert_eq!(
        ConsoleMux.write_byte(b'?'),
        Err(ServiceError::HardwareError)
    );
    assert_eq!(ConsoleMux.read_byte(), Err(ServiceError::HardwareError));
    assert_eq!(ConsoleMux.flush(), Err(ServiceError::HardwareError));
    board.failed.store(false, Ordering::Relaxed);

    // A firmware log record owns this same lock; raw payload callbacks must
    // return rather than spinning if a backend logs or another CPU owns it.
    {
        let _writer = fstart_log::writer();
        assert_eq!(ConsoleMux.write_byte(b'?'), Err(ServiceError::IoError));
        assert_eq!(ConsoleMux.read_byte(), Err(ServiceError::IoError));
        assert_eq!(ConsoleMux.flush(), Err(ServiceError::IoError));
    }
    board.recurse.store(true, Ordering::Relaxed);
    ConsoleMux.write_byte(b'+').unwrap();
    board.recurse.store(false, Ordering::Relaxed);
    ConsoleMux.write_byte(b'-').unwrap();

    #[cfg(feature = "debug-consoles")]
    {
        static DEBUG: StaticConsole<Backend> = StaticConsole::new();
        let debug = DEBUG.install_debug(Backend::default());
        *board.input.lock().unwrap() = Some(b'a');
        *debug.input.lock().unwrap() = Some(b'b');
        assert_eq!(ConsoleMux.read_byte(), Ok(Some(b'a')));
        assert_eq!(ConsoleMux.read_byte(), Ok(Some(b'b')));
        assert_eq!(ConsoleMux.read_byte(), Ok(None));

        board.failed.store(true, Ordering::Relaxed);
        *debug.input.lock().unwrap() = Some(b'c');
        assert_eq!(ConsoleMux.read_byte(), Ok(Some(b'c')));
        ConsoleMux.write_bytes(b"debug").unwrap();
        ConsoleMux.flush().unwrap();
        assert_eq!(&*debug.output.lock().unwrap(), b"debug");
        debug.failed.store(true, Ordering::Relaxed);
        assert_eq!(
            ConsoleMux.write_byte(b'?'),
            Err(ServiceError::HardwareError)
        );
        assert_eq!(ConsoleMux.read_byte(), Err(ServiceError::HardwareError));
        assert_eq!(ConsoleMux.flush(), Err(ServiceError::HardwareError));
    }
}
