//! Minimal RISC-V S-mode test payload.
//!
//! This binary is entered by RustSBI in S-mode with:
//!   a0 = hartid
//!   a1 = DTB address
//!
//! It prints a success message via SBI console_putchar (legacy extension)
//! and then shuts down via SBI SRST extension.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

/// SBI legacy console_putchar (EID 0x01).
fn sbi_console_putchar(ch: u8) {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x01_usize,  // legacy console_putchar
            in("a0") ch as usize,
            lateout("a0") _,
            lateout("a1") _,
        );
    }
}

/// SBI system reset (EID 0x53525354 "SRST", FID 0).
fn sbi_shutdown() -> ! {
    unsafe {
        core::arch::asm!(
            "ecall",
            in("a7") 0x53525354_usize,  // SRST extension
            in("a6") 0_usize,           // FID 0 = system_reset
            in("a0") 0_usize,           // reset_type = shutdown
            in("a1") 0_usize,           // reset_reason = no reason
            options(noreturn),
        );
    }
}

fn print_str(s: &str) {
    for byte in s.bytes() {
        sbi_console_putchar(byte);
    }
}

/// Entry point — called from `_start` after stack setup.
#[unsafe(no_mangle)]
extern "C" fn payload_main(_hartid: usize, _dtb_addr: usize) -> ! {
    print_str("\r\n");
    print_str("========================================\r\n");
    print_str("  fstart test payload running in S-mode\r\n");
    print_str("  Boot chain: QEMU -> fstart -> RustSBI -> HERE\r\n");
    print_str("========================================\r\n");
    print_str("\r\n");
    print_str("[payload] hartid = 0x");
    // Print hartid as hex (simple, no alloc needed)
    print_hex(_hartid);
    print_str("\r\n");
    print_str("[payload] dtb    = 0x");
    print_hex(_dtb_addr);
    print_str("\r\n");
    print_str("[payload] SUCCESS — full boot chain verified!\r\n");
    print_str("\r\n");

    // Shutdown via SBI SRST
    print_str("[payload] Shutting down via SBI SRST...\r\n");
    sbi_shutdown();
}

fn print_hex(val: usize) {
    if val == 0 {
        sbi_console_putchar(b'0');
        return;
    }
    // Find the highest non-zero nibble
    let mut started = false;
    for i in (0..16).rev() {
        let nibble = (val >> (i * 4)) & 0xF;
        if nibble != 0 {
            started = true;
        }
        if started {
            let ch = if nibble < 10 {
                b'0' + nibble as u8
            } else {
                b'a' + (nibble - 10) as u8
            };
            sbi_console_putchar(ch);
        }
    }
}

#[unsafe(link_section = ".text.entry")]
#[unsafe(no_mangle)]
unsafe extern "C" fn _start() -> ! {
    core::arch::asm!(
        // a0 = hartid (passed by SBI, preserve it)
        // a1 = dtb_addr (passed by SBI, preserve it)
        "la sp, __stack_top",
        "call payload_main",
        // Should not return, but just in case:
        "1: wfi",
        "j 1b",
        options(noreturn),
    );
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    print_str("[payload] PANIC!\r\n");
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
