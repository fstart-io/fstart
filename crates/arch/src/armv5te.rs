//! ARMv5TE entry and handoff helpers: the ARM926EJ-S, and the ARM1176 run
//! as ARMv5TE (its LDREX/STREX hang without an exclusive monitor while the
//! MMU is off).
//!
//! Exceptions use whatever vector table sits at address 0; a platform that
//! executes from flash at 0 provides it in its image head and points the
//! vectors it does not handle at `_exception_hang`. Only the SVC stack is
//! set up, so that hang must not touch the stack.

use core::arch::global_asm;

global_asm!(
    r#"
    .section .text.entry
    .global _start
    .arm
_start:
    mov r6, r0

    @ Supervisor mode, IRQ and FIQ masked.
    mrs r0, cpsr
    bic r0, r0, #0x1f
    orr r0, r0, #0xd3
    msr cpsr_c, r0

    @ MMU, alignment checking, D-cache, I-cache and high vectors off, then
    @ invalidate caches and TLBs, then turn the I-cache back on: execute-in-
    @ place from SPI flash is slow without it.
    mrc p15, 0, r0, c1, c0, 0
    bic r0, r0, #0x2000
    bic r0, r0, #0x1000
    bic r0, r0, #0x7
    mcr p15, 0, r0, c1, c0, 0
    mov r1, #0
    mcr p15, 0, r1, c7, c7, 0
    mcr p15, 0, r1, c8, c7, 0
    mcr p15, 0, r1, c7, c10, 4
    orr r0, r0, #0x1000
    mcr p15, 0, r0, c1, c0, 0

    ldr r0, =_stack_top
    bic r0, r0, #7
    mov sp, r0

    @ Copy initialized data from its load address before clearing BSS.
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
    strlo r2, [r0], #4
    blo 2b

    mov r0, r6
    bl fstart_main
3:
    b 3b

    .ltorg

    .section .text._exception_hang,"ax",%progbits
    .global _exception_hang
    .type _exception_hang, %function
    .arm
_exception_hang:
    b _exception_hang
    "#
);

/// `fence(SeqCst)` lowers to this libcall on ARMv5TE, which has no barrier
/// instruction, and the bare-metal compiler builtins do not provide it. The
/// cores run single-threaded, with the MMU off or write-through caching
/// (see [`enable_dcache`]), so draining the write buffer orders memory as a
/// full barrier.
#[unsafe(no_mangle)]
pub extern "C" fn __sync_synchronize() {
    // SAFETY: CP15 c7/c10/4 drains the write buffer; the source register
    // should be zero and nothing else changes.
    unsafe {
        core::arch::asm!(
            "mcr p15, 0, {zero}, c7, c10, 4",
            zero = in(reg) 0u32,
            options(nostack, preserves_flags)
        );
    }
}

/// Wait for an interrupt through CP15; ARMv5TE has no `wfi` instruction.
#[inline(always)]
pub fn wait_for_interrupt() {
    // SAFETY: CP15 c7/c0/4 stalls until an interrupt; the source register
    // should be zero and nothing else changes.
    unsafe {
        core::arch::asm!(
            "mcr p15, 0, {zero}, c7, c0, 4",
            zero = in(reg) 0u32,
            options(nomem, nostack, preserves_flags)
        );
    }
}

/// A first-level translation table: one descriptor per 1 MiB section.
#[repr(C, align(16384))]
pub struct TranslationTable([u32; 4096]);

impl TranslationTable {
    pub const fn new() -> Self {
        Self([0; 4096])
    }
}

impl Default for TranslationTable {
    fn default() -> Self {
        Self::new()
    }
}

/// Section descriptor in the ARMv5 format, which the ARM1176 also reads
/// with its extended format off: full access in domain 0, bit 4 set as the
/// ARM926 requires. Without C and B the section is uncached, unbuffered.
const SECTION: u32 = 0b11 << 10 | 1 << 4 | 0b10;
/// C without B: cached write-through.
const WRITE_THROUGH: u32 = 1 << 3;

/// Map the address space flat in 1 MiB sections and turn on the MMU and the
/// D-cache. Sections wholly inside a `cached` range (start, size) are
/// cached write-through, everything else stays uncached and unbuffered.
///
/// Write-through leaves no dirty lines, so the jump helpers below turn the
/// MMU and D-cache off again without cleaning anything, as the next stage
/// or payload expects them.
///
/// # Safety
///
/// The `cached` ranges are memory without read side effects that nothing
/// but this CPU writes while the D-cache is on.
pub unsafe fn enable_dcache(table: &'static mut TranslationTable, cached: &[(u64, u64)]) {
    const MIB: u64 = 1 << 20;
    for (section, entry) in (0u64..).zip(table.0.iter_mut()) {
        let start = section * MIB;
        let wholly = |&(base, size): &(u64, u64)| base <= start && start + MIB <= base + size;
        *entry = start as u32
            | SECTION
            | if cached.iter().any(wholly) {
                WRITE_THROUGH
            } else {
                0
            };
    }
    // SAFETY: the table is written to memory (D-cache off) and maps every
    // address to itself, so execution continues where it is. CP15: drain
    // the write buffer, invalidate the D-cache and TLBs, domain 0 client,
    // table base, then the MMU and D-cache on with the ARM1176's extended
    // table format (bit 23) off.
    unsafe {
        core::arch::asm!(
            "mcr p15, 0, {zero}, c7, c10, 4",
            "mcr p15, 0, {zero}, c7, c6, 0",
            "mcr p15, 0, {zero}, c8, c7, 0",
            "mcr p15, 0, {client}, c3, c0, 0",
            "mcr p15, 0, {table}, c2, c0, 0",
            "mrc p15, 0, {ctrl}, c1, c0, 0",
            "bic {ctrl}, {ctrl}, #0x800000",
            "orr {ctrl}, {ctrl}, #0x5",
            "mcr p15, 0, {ctrl}, c1, c0, 0",
            zero = in(reg) 0u32,
            client = in(reg) 1u32,
            table = in(reg) table.0.as_ptr(),
            ctrl = out(reg) _,
            options(nostack, preserves_flags)
        );
    }
}

/// Leave the CPU as a stage or payload loaded into RAM expects it: drain
/// the write buffer, turn the MMU and D-cache off (write-through, see
/// [`enable_dcache`], so nothing needs cleaning), then invalidate the
/// D-cache, the TLBs and the I-cache, which `_start` turned on, so that
/// the code just written is visible to instruction fetch (on the ARM1176
/// this also flushes the branch target cache).
#[inline(always)]
fn sync_icache() {
    // SAFETY: the running code is mapped flat if the MMU is on, so turning
    // it off changes no address; the CP15 operations take a zero register.
    unsafe {
        core::arch::asm!(
            "mcr p15, 0, {zero}, c7, c10, 4",
            "mrc p15, 0, {ctrl}, c1, c0, 0",
            "bic {ctrl}, {ctrl}, #0x5",
            "mcr p15, 0, {ctrl}, c1, c0, 0",
            "mcr p15, 0, {zero}, c7, c6, 0",
            "mcr p15, 0, {zero}, c8, c7, 0",
            "mcr p15, 0, {zero}, c7, c5, 0",
            zero = in(reg) 0u32,
            ctrl = out(reg) _,
            options(nostack, preserves_flags)
        );
    }
}

/// Jump unconditionally to an ARM stage or payload entry loaded into RAM.
#[inline(always)]
pub fn jump_to(addr: u64) -> ! {
    sync_icache();
    let addr = addr as u32;
    // SAFETY: caller provides a valid executable ARM entry point.
    unsafe { core::arch::asm!("bx {addr}", addr = in(reg) addr, options(noreturn)) }
}

/// Jump to an ARM entry loaded into RAM with a handoff pointer in `r0`.
#[inline(always)]
pub fn jump_to_with_handoff(addr: u64, handoff_addr: usize) -> ! {
    sync_icache();
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
