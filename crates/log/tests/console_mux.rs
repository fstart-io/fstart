#![cfg(feature = "debug-consoles")]

use fstart_core::services::{Console, NoConsole, ServiceError};
use fstart_log::{ConsoleMux, StaticConsole};
use std::sync::Mutex;

struct DebugConsole {
    output: Mutex<Vec<u8>>,
    input: Mutex<Option<u8>>,
}

impl Console for DebugConsole {
    fn write_byte(&self, byte: u8) -> Result<(), ServiceError> {
        self.output.lock().unwrap().push(byte);
        Ok(())
    }

    fn read_byte(&self) -> Result<Option<u8>, ServiceError> {
        Ok(self.input.lock().unwrap().take())
    }
}

#[test]
fn payload_console_retains_debug_output_and_input_without_a_board_console() {
    static BOARD: StaticConsole<NoConsole> = StaticConsole::new();
    static DEBUG: StaticConsole<DebugConsole> = StaticConsole::new();
    BOARD.install(NoConsole);
    let debug = DEBUG.install_debug(DebugConsole {
        output: Mutex::new(Vec::new()),
        input: Mutex::new(Some(b'x')),
    });

    ConsoleMux.write_bytes(b"CrabEFI\n").unwrap();
    ConsoleMux.flush().unwrap();
    assert_eq!(&*debug.output.lock().unwrap(), b"CrabEFI\n");
    assert_eq!(ConsoleMux.read_byte().unwrap(), Some(b'x'));
    assert_eq!(ConsoleMux.read_byte().unwrap(), None);
}
