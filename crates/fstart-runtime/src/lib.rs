//! Runtime support for fstart firmware stages.
//!
//! Provides panic handler and other essential lang items for `no_std` binaries.

#![no_std]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    // TODO: write panic info to console if available
    loop {
        core::hint::spin_loop();
    }
}
