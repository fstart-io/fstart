//! Runtime support for fstart firmware stages.
//!
//! Provides panic handler and other essential lang items for `no_std` binaries.

#![no_std]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // Best-effort output — write a fixed marker to the console using
    // fstart_log's writer and ufmt's uWrite trait (no macro expansion
    // needed, avoids ufmt crate dependency in this crate).
    //
    // If the console isn't initialized yet, writer() returns a no-op
    // writer and this silently does nothing.
    let mut w = fstart_log::writer();
    use fstart_log::ufmt::uWrite;
    let _ = w.write_str("[PANIC] firmware panic\r\n");
    loop {
        core::hint::spin_loop();
    }
}
