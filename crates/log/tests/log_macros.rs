use fstart_core::services::{Console, ServiceError};
use fstart_log::{Hex, Level, StaticConsole, is_plain_literal};
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Default)]
struct RecordingConsole {
    output: Mutex<Vec<u8>>,
    flushes: AtomicUsize,
}

impl RecordingConsole {
    fn take_output(&self) -> String {
        let mut buf = self.output.lock().unwrap();
        let out = String::from_utf8(buf.clone()).unwrap();
        buf.clear();
        out
    }

    fn take_flushes(&self) -> usize {
        self.flushes.swap(0, Ordering::Relaxed)
    }
}

impl Console for RecordingConsole {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        self.output.lock().unwrap().push(byte);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        Ok(None)
    }

    fn flush(&self) -> Result<(), ServiceError> {
        self.flushes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[test]
fn plain_literal_detection_rejects_format_braces() {
    assert!(is_plain_literal(""));
    assert!(is_plain_literal("plain message\nwith newline"));
    assert!(!is_plain_literal("value={}"));
    assert!(!is_plain_literal("{{escaped}}"));
    assert!(!is_plain_literal("unmatched }"));
}

#[test]
fn log_macros_frame_plain_escaped_and_formatted_records() {
    static CONSOLE: StaticConsole<RecordingConsole> = StaticConsole::new();
    let console = CONSOLE.install(RecordingConsole::default());

    fstart_log::info!("boot ok");
    assert_eq!(console.take_output(), "[INFO ] boot ok\r\n");
    assert_eq!(console.take_flushes(), 1);

    fstart_log::warn!("line1\nline2",);
    assert_eq!(console.take_output(), "[WARN ] line1\r\nline2\r\n");
    assert_eq!(console.take_flushes(), 1);

    // Single literal containing escaped braces must still go through ufmt.
    fstart_log::error!("{{braced}}");
    assert_eq!(console.take_output(), "[ERROR] {braced}\r\n");
    assert_eq!(console.take_flushes(), 1);

    // Formatted records emit tag, formatted body, CRLF, and flush once.
    fstart_log::info!("n={} addr={}", 42u32, Hex(0x1a));
    assert_eq!(console.take_output(), "[INFO ] n=42 addr=0x1a\r\n");
    assert_eq!(console.take_flushes(), 1);

    // Filtered levels emit nothing and do not evaluate arguments.
    unsafe { fstart_log::set_max_level(Level::Warn) };
    let mut evaluated = false;
    fstart_log::info!("skipped {}", {
        evaluated = true;
        1u32
    });
    fstart_log::debug!("also skipped");
    assert!(!evaluated);
    assert_eq!(console.take_output(), "");
    assert_eq!(console.take_flushes(), 0);
}
