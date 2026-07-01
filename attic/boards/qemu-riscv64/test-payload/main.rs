//! Minimal all-assembly RISC-V S-mode test payload.
//!
//! Entered by OpenSBI with a0 = hartid and a1 = DTB address. It writes a
//! visible marker to QEMU virt's UART and exits through QEMU's test finisher.

#![no_std]
#![no_main]

use core::panic::PanicInfo;

core::arch::global_asm!(
    r#"
    .section .text.entry, "ax"
    .globl _start
_start:
    li sp, 0x82008000

    la t2, payload_msg
1:
    lbu t1, 0(t2)
    beqz t1, 2f
    li t0, 0x10000000
    sb t1, 0(t0)
    addi t2, t2, 1
    j 1b

2:
    li t0, 0x00100000
    li t1, 0x5555
    sw t1, 0(t0)

3:
    wfi
    j 3b

payload_msg:
    .ascii "\r\n[payload] FSTART_CI_BOOT_SUCCESS\r\n"
    .byte 0
"#
);

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe { core::arch::asm!("wfi") };
    }
}
