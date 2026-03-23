//! Runtime support for fstart firmware stages.
//!
//! Provides panic handler and other essential lang items for `no_std` binaries.

#![no_std]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Best-effort output — write the panic location to the console.
    let mut w = fstart_log::writer();
    use fstart_log::ufmt::uWrite;
    let _ = w.write_str("[PANIC] ");
    // Try to print the panic message if available.
    if let Some(msg) = info.message().as_str() {
        let _ = w.write_str(msg);
    } else {
        let _ = w.write_str("firmware panic");
    }
    if let Some(loc) = info.location() {
        let _ = w.write_str(" at ");
        let _ = w.write_str(loc.file());
        let _ = w.write_str(":");
        // Print line number digit by digit (no ufmt dep in this crate).
        let mut line = loc.line();
        let mut buf = [0u8; 10];
        let mut i = buf.len();
        if line == 0 {
            i -= 1;
            buf[i] = b'0';
        }
        while line > 0 {
            i -= 1;
            buf[i] = b'0' + (line % 10) as u8;
            line /= 10;
        }
        if let Ok(s) = core::str::from_utf8(&buf[i..]) {
            let _ = w.write_str(s);
        }
    }
    let _ = w.write_str("\r\n");
    loop {
        core::hint::spin_loop();
    }
}
