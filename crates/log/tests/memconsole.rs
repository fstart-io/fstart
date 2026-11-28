#![cfg(feature = "memconsole")]

use embedded_io::Write;
use fstart_core::services::{Console, NoConsole};
use fstart_log::{StaticConsole, mem};

#[test]
fn no_console_keeps_entire_formatted_records_and_raw_writes() {
    static BOARD: StaticConsole<NoConsole> = StaticConsole::new();
    BOARD.install(NoConsole);
    let mut backing = [0u32; 32];
    // SAFETY: aligned, exclusive backing, detached before leaving this test.
    assert!(unsafe { mem::init(backing.as_mut_ptr().cast(), size_of_val(&backing)) });
    fstart_log::info!("number={} end", 42u32);
    let raw = fstart_log::writer().write(b"raw\n");
    fstart_log::raw_write_byte(b'>');
    let mux = fstart_log::ConsoleMux.write_bytes(b"payload\n");
    let mut buf = [0; 64];
    let n = mem::copy_to_and_detach(&mut buf);
    assert_eq!(raw, Ok(4));
    assert_eq!(mux, Ok(()));
    assert_eq!(&buf[..n], b"[INFO ] number=42 end\nraw\n>payload\n");
    assert!(fstart_log::writer().write(b"detached").is_err());
}
