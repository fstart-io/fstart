//! ARMv7-A entry and handoff helpers.

#[cfg(target_arch = "arm")]
use core::arch::global_asm;

#[cfg(all(target_arch = "arm", not(feature = "sunxi-fel")))]
global_asm!(
    ".section .text.pre_stack\n\
     .weak fstart_pre_stack_entry\n\
     fstart_pre_stack_entry:\n\
     bx lr\n"
);

#[cfg(target_arch = "arm")]
global_asm!(
    r#"
    .section .text.entry
    .global _start
    .arm
_start:
    b reset
    ldr pc, _undefined_addr
    ldr pc, _swi_addr
    ldr pc, _prefetch_addr
    ldr pc, _data_addr
    ldr pc, _reserved_addr
    ldr pc, _irq_addr
    ldr pc, _fiq_addr

_undefined_addr: .word _exception_hang
_swi_addr:       .word _exception_hang
_prefetch_addr:  .word _exception_hang
_data_addr:      .word _exception_hang
_reserved_addr:  .word _exception_hang
_irq_addr:       .word _exception_hang
_fiq_addr:       .word _exception_hang
                .word 0xdeadbeef

_exception_hang:
    b _exception_hang

reset:
    mov r6, r0

    @ SoC code may preserve BROM state before this generic entry changes it.
    bl fstart_pre_stack_entry

    @ Match the ARMv7 reset-mode sequence: do not use cpsid to leave HYP.
    mrc p15, 0, r0, c0, c1, 1
    and r0, r0, #0xf000
    cmp r0, #0x1000
    beq switch_to_hypervisor
switch_to_hypervisor_ret:
    mrs r0, cpsr
    and r1, r0, #0x1f
    teq r1, #0x1a
    bicne r0, r0, #0x1f
    orrne r0, r0, #0x13
    orr r0, r0, #0xc0
    msr cpsr, r0

    mrc p15, 0, r0, c1, c0, 0
    bic r0, r0, #0x2000
    mcr p15, 0, r0, c1, c0, 0
    ldr r0, =_start
    mcr p15, 0, r0, c12, c0, 0

    mrc p15, 0, r0, c1, c0, 1
    orr r0, r0, #(1 << 6)
    mcr p15, 0, r0, c1, c0, 1
    mov r0, #0
    mcr p15, 0, r0, c8, c7, 0
    mcr p15, 0, r0, c7, c5, 0
    mcr p15, 0, r0, c7, c5, 6
    dsb
    isb
    mrc p15, 0, r0, c1, c0, 0
    bic r0, r0, #0x2000
    bic r0, r0, #0x7
    orr r0, r0, #0x2
    orr r0, r0, #0x800
    orr r0, r0, #0x1000
    mcr p15, 0, r0, c1, c0, 0

    ldr r0, =_stack_top
    bic r0, r0, #7
    mov sp, r0

    @ Copy initialized data from ROM LMA to RAM VMA before BSS clear.
    ldr r0, =_data_load
    ldr r1, =_data_start
    ldr r2, =_data_end
1:
    cmp r1, r2
    ldrlo r3, [r0], #4
    strlo r3, [r1], #4
    blo 1b

    ldr r0, =_bss_start
    ldr r1, =_bss_end
    mov r2, #0
2:
    cmp r0, r1
    strlo r2, [r0]
    addlo r0, r0, #4
    blo 2b

    mov r0, r6
    bl fstart_main
2:
    wfe
    b 2b
switch_to_hypervisor:
    b switch_to_hypervisor_ret

    .ltorg
    "#
);

/// Jump unconditionally to an ARMv7 stage or payload entry.
#[cfg(target_arch = "arm")]
#[inline(always)]
pub fn jump_to(addr: u64) -> ! {
    let addr = addr as u32;
    // SAFETY: caller provides a valid executable ARM entry point.
    unsafe { core::arch::asm!("bx {addr}", addr = in(reg) addr, options(noreturn)) }
}

/// Jump to an ARMv7 entry with a handoff pointer in `r0`.
#[cfg(target_arch = "arm")]
#[inline(always)]
pub fn jump_to_with_handoff(addr: u64, handoff_addr: usize) -> ! {
    let addr = addr as u32;
    let handoff_addr = handoff_addr as u32;
    // SAFETY: caller provides a valid entry point and handoff buffer.
    unsafe {
        core::arch::asm!(
            "bx {addr}",
            addr = in(reg) addr,
            in("r0") handoff_addr,
            options(noreturn),
        )
    }
}

/// Disable the I-cache and branch predictor before Linux handoff.
#[cfg(target_arch = "arm")]
pub fn cleanup_before_linux() {
    // SAFETY: cache maintenance is valid from secure ARMv7 firmware.
    unsafe {
        core::arch::asm!(
            "mrc p15, 0, r0, c1, c0, 0",
            "bic r0, r0, #(1 << 12)",
            "mcr p15, 0, r0, c1, c0, 0",
            "isb",
            "mov r0, #0",
            "mcr p15, 0, r0, c7, c5, 0",
            "mcr p15, 0, r0, c7, c5, 6",
            "dsb",
            "isb",
            out("r0") _,
            options(nomem, nostack),
        );
    }
}

/// Clean up ARM state and boot a Linux kernel using its DT-only protocol.
#[cfg(target_arch = "arm")]
pub fn boot_linux(params: &fstart_core::services::BootLinuxParams<'_>) -> ! {
    cleanup_before_linux();
    boot_linux_direct(params.kernel_addr, params.dtb_addr)
}

/// Boot a Linux kernel using the ARM DT-only protocol.
#[cfg(target_arch = "arm")]
#[inline(always)]
pub fn boot_linux_direct(kernel_addr: u64, dtb_addr: u64) -> ! {
    let kernel = kernel_addr as u32;
    let dtb = dtb_addr as u32;
    // SAFETY: caller supplies a valid ARM kernel and DTB in RAM.
    unsafe {
        core::arch::asm!(
            "cpsid aif, #0x13",
            "bx {kernel}",
            kernel = in(reg) kernel,
            in("r0") 0u32,
            in("r1") 0xffff_ffffu32,
            in("r2") dtb,
            options(noreturn),
        )
    }
}
