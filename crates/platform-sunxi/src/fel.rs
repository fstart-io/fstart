//! Return to the BROM's FEL USB loader: the entry hook saves the BROM's
//! state before the stage sets up its stack.

/// BROM state retained so a Sunxi stage can return to FEL.
#[repr(C)]
pub struct FelStash {
    pub sp: u32,
    pub lr: u32,
    pub cpsr: u32,
    pub sctlr: u32,
    pub vbar: u32,
}

core::arch::global_asm!(
    r#"
    .section .text.entry
    .global fstart_pre_stack_entry
    .global fstart_return_to_fel
    .global fstart_sunxi_fel_stash
    .arm
fstart_pre_stack_entry:
    ldr r0, =fstart_sunxi_fel_stash
    str sp, [r0, #0]
    str lr, [r0, #4]
    mrs r1, cpsr
    str r1, [r0, #8]
    mrc p15, 0, r1, c1, c0, 0
    str r1, [r0, #12]
    mrc p15, 0, r1, c12, c0, 0
    str r1, [r0, #16]
    bx lr
fstart_return_to_fel:
    mov sp, r0
    mov lr, r1
    ldr r0, =fstart_sunxi_fel_stash
    ldr r1, [r0, #16]
    mcr p15, 0, r1, c12, c0, 0
    ldr r1, [r0, #12]
    mcr p15, 0, r1, c1, c0, 0
    ldr r1, [r0, #8]
    msr cpsr, r1
    bx lr
    .ltorg
    .section .data
    .align 2
fstart_sunxi_fel_stash:
    .space 20
    "#
);

unsafe extern "C" {
    #[link_name = "fstart_sunxi_fel_stash"]
    static FEL_STASH: FelStash;
    fn fstart_return_to_fel(sp: u32, lr: u32) -> !;
}

/// Return to the BROM FEL handler saved by the pre-stack entry hook.
///
/// # Safety
/// Must be called only after the Sunxi entry hook saved valid BROM state.
pub unsafe fn return_to_fel() -> ! {
    // SAFETY: the pre-stack hook initializes this before Rust is entered.
    unsafe { fstart_return_to_fel(FEL_STASH.sp, FEL_STASH.lr) }
}
