//! Runtime support for fstart firmware stages.
//!
//! Provides panic handler and other essential lang items for `no_std` binaries.

#[cfg(all(not(test), target_os = "none"))]
use core::panic::PanicInfo;

#[cfg(all(not(test), target_os = "none"))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // Best-effort output: the message when it is a plain string, and the
    // location. No `core::fmt`, and the console write is emitted once
    // (`write_parts`), not inlined per piece: SRAM stages carry this too.
    let message = info.message().as_str().unwrap_or("firmware panic");
    let (file, mut line) = info.location().map_or(("?", 0), |l| (l.file(), l.line()));
    let mut digits = [0u8; 10];
    let mut i = digits.len();
    loop {
        i -= 1;
        digits[i] = b'0' + (line % 10) as u8;
        line /= 10;
        if line == 0 {
            break;
        }
    }
    // SAFETY: ASCII digits only.
    let line = unsafe { core::str::from_utf8_unchecked(&digits[i..]) };
    write_parts(&["[PANIC] ", message, " at ", file, ":", line, "\r\n"]);
    loop {
        core::hint::spin_loop();
    }
}

#[cfg(all(not(test), target_os = "none"))]
#[inline(never)]
fn write_parts(parts: &[&str]) {
    use fstart_log::ufmt::uWrite;
    let mut w = fstart_log::panic_writer();
    for part in parts {
        let _ = w.write_str(part);
    }
}
