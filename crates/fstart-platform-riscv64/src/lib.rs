//! RISC-V 64-bit platform support.
//!
//! Provides the reset vector entry point, stack setup, BSS clearing,
//! and architecture-specific helpers. Captures boot parameters (hart ID,
//! DTB address) passed by QEMU at reset.
//!
//! Two entry paths are supported:
//!
//! - **Default** (`entry.rs`): Standard RISC-V entry for platforms that
//!   start with DTB in `a1` (e.g., QEMU virt).
//!
//! - **Sunxi** (`entry_sunxi.rs`, behind `sunxi` feature): Entry for
//!   Allwinner D1/T113 SoCs that boot from the BROM via eGON. The D1
//!   starts directly in RV64 M-mode — no mode switch is needed.

#![no_std]

#[cfg(not(any(feature = "sunxi", feature = "smode-entry")))]
pub mod entry;

#[cfg(feature = "smode-entry")]
pub mod entry_smode;

#[cfg(feature = "sunxi")]
pub mod entry_sunxi;

// ---------------------------------------------------------------------------
// Boot parameters — stored in CSRs, immune to stack overflow
// ---------------------------------------------------------------------------

/// Return the boot hart ID passed by QEMU at reset (`a0`).
///
/// Reads directly from the `mhartid` CSR (always available in M-mode).
pub fn boot_hart_id() -> u64 {
    let id: u64;
    // SAFETY: reading mhartid is a read-only CSR access, valid at any privilege level.
    unsafe {
        core::arch::asm!("csrr {}, mhartid", out(reg) id);
    }
    id
}

// ---------------------------------------------------------------------------
// SBI copy trampoline (position-independent)
// ---------------------------------------------------------------------------
//
// Copies firmware from a3 (src) to a6 (dst/entry), a5 bytes, then
// issues fence + fence.i and jumps to a6.
// a0/a1/a2 are preserved for the SBI entry convention.
core::arch::global_asm!(
    r#"
    .section .text
    .balign 4
    .global _sbi_copy_trampoline
    .global _sbi_copy_trampoline_end
_sbi_copy_trampoline:
    mv   a4, a6          // a4 = write pointer (starts at entry/dst)
1:
    beqz a5, 2f           // if remaining == 0, done
    lbu  t0, 0(a3)        // load byte from src
    sb   t0, 0(a4)        // store byte to dst
    addi a3, a3, 1
    addi a4, a4, 1
    addi a5, a5, -1
    j    1b
2:
    fence rw, rw
    fence.i
    jr   a6               // jump to firmware entry point
_sbi_copy_trampoline_end:
    "#
);

/// Return the DTB address passed by QEMU at reset (`a1`).
///
/// The `_start` assembly saves `a1` into the `mscratch` CSR (following
/// coreboot's approach). This is immune to stack overflow — unlike a
/// BSS global which can be clobbered when debug-build crypto operations
/// exhaust the stack.
pub fn boot_dtb_addr() -> u64 {
    let addr: u64;
    // SAFETY: reading mscratch retrieves the DTB address saved by _start; always valid
    // in M-mode context.
    unsafe {
        core::arch::asm!("csrr {}, mscratch", out(reg) addr);
    }
    addr
}

// ---------------------------------------------------------------------------
// OpenSBI / RustSBI fw_dynamic protocol
// ---------------------------------------------------------------------------

/// `fw_dynamic_info` structure for the SBI fw_dynamic boot protocol.
///
/// Passed in `a2` when jumping to OpenSBI or RustSBI. Tells the SBI
/// firmware where the next boot stage (Linux) lives and what mode to
/// run it in.
///
/// Reference: OpenSBI `include/sbi/fw_dynamic.h`
#[derive(Debug)]
#[repr(C)]
pub struct FwDynamicInfo {
    /// Magic value: `0x4942534f` ("OSBI").
    pub magic: u64,
    /// Version of the info struct (currently 2).
    pub version: u64,
    /// Entry address of the next boot stage (Linux kernel).
    pub next_addr: u64,
    /// Privilege mode for the next stage: 1 = Supervisor.
    pub next_mode: u64,
    /// Options (reserved, set to 0).
    pub options: u64,
    /// Hart ID to boot on (usually 0).
    pub boot_hart: u64,
}

impl FwDynamicInfo {
    /// Magic value identifying this struct to the SBI firmware.
    pub const MAGIC: u64 = 0x4942534f; // "OSBI"
    /// Current version of the fw_dynamic_info protocol.
    pub const VERSION: u64 = 2;
    /// Supervisor mode constant for `next_mode`.
    pub const MODE_S: u64 = 1;

    /// Create a new `FwDynamicInfo` for booting Linux.
    pub fn new(kernel_addr: u64, boot_hart: u64) -> Self {
        Self {
            magic: Self::MAGIC,
            version: Self::VERSION,
            next_addr: kernel_addr,
            next_mode: Self::MODE_S,
            options: 0,
            boot_hart,
        }
    }
}

/// Jump to an SBI firmware (OpenSBI / RustSBI) using the fw_dynamic
/// protocol, which then boots Linux in S-mode.
///
/// Register convention on entry to SBI firmware:
/// - `a0` = boot hart ID
/// - `a1` = DTB address (for Linux, passed through by SBI)
/// - `a2` = pointer to `FwDynamicInfo`
///
/// # Safety
///
/// The caller must ensure all addresses are valid and the SBI binary
/// is loaded at `sbi_addr`.
pub fn boot_linux_sbi(sbi_addr: u64, hart_id: u64, dtb_addr: u64, info: &FwDynamicInfo) -> ! {
    // SAFETY: caller guarantees all addresses are valid mapped memory and firmware is
    // at sbi_addr; this is a non-returning M-mode to S-mode transition.
    unsafe {
        // Use explicit register constraints for a0/a1/a2 so the compiler
        // places values directly — avoids clobbering `{sbi}` with `mv`
        // instructions (the compiler is free to pick any register for
        // `{sbi}`, guaranteed not to be a0/a1/a2).
        core::arch::asm!(
            // Ensure stores to the SBI region are visible and the
            // I-cache fetches the new instructions (not stale FFS data).
            "fence rw, rw",
            "fence.i",
            "jr {sbi}",
            sbi = in(reg) sbi_addr,
            in("a0") hart_id,
            in("a1") dtb_addr,
            in("a2") info as *const FwDynamicInfo as u64,
            options(noreturn),
        );
    }
}

/// Copy an SBI firmware blob to its load address, then jump to it
/// using the fw_dynamic protocol.
///
/// This is needed when the firmware's load address overlaps with the
/// currently-executing code (e.g., both at 0x80000000). The copy +
/// jump is performed from a position-independent trampoline that is
/// first copied to a safe location (near the stack top) so it
/// survives the firmware overwrite.
///
/// # Safety
///
/// - `fw_src` and `fw_dst` must point to valid memory.
/// - `fw_len` must be the exact size of the firmware binary.
/// - The trampoline destination (`trampoline_addr`) must be in safe
///   RAM that won't be overwritten by the firmware copy.
/// - All other payload files must already be loaded.
pub fn copy_and_boot_sbi(
    fw_src: *const u8,
    fw_dst: u64,
    fw_len: usize,
    hart_id: u64,
    dtb_addr: u64,
    info: &FwDynamicInfo,
    trampoline_addr: u64,
) -> ! {
    // Copy the trampoline (defined in assembly below) to a safe
    // high-memory location, then jump to it.  The trampoline copies
    // the firmware blob from FFS to its load address and jumps.

    extern "C" {
        fn _sbi_copy_trampoline();
        fn _sbi_copy_trampoline_end();
    }
    let tramp_src = _sbi_copy_trampoline as *const u8;
    let tramp_end = _sbi_copy_trampoline_end as *const u8;
    let tramp_size = tramp_end as usize - tramp_src as usize;
    let tramp_dst = trampoline_addr as *mut u8;

    // Copy trampoline to safe location.
    // SAFETY: trampoline_addr is in safe high RAM (near stack top).
    unsafe {
        core::ptr::copy_nonoverlapping(tramp_src, tramp_dst, tramp_size);
    }

    // Flush I-cache for the trampoline region.
    // SAFETY: fence instructions are always safe.
    unsafe {
        core::arch::asm!("fence rw, rw");
        core::arch::asm!("fence.i");
    }

    // Jump to the trampoline with all parameters in registers.
    // a3 = src, a4 = dst, a5 = len, a6 = entry point (same as dst)
    // SAFETY: all addresses are valid, trampoline is at trampoline_addr.
    unsafe {
        core::arch::asm!(
            "jr {tramp}",
            tramp = in(reg) trampoline_addr,
            in("a0") hart_id,
            in("a1") dtb_addr,
            in("a2") info as *const FwDynamicInfo as u64,
            in("a3") fw_src as u64,
            in("a4") fw_dst,
            in("a5") fw_len,
            in("a6") fw_dst, // entry point = destination base
            options(noreturn),
        );
    }
}

// ---------------------------------------------------------------------------
// OpenSBI boot-and-resume (for CrabEFI UEFI payload)
// ---------------------------------------------------------------------------

// Assembly for the OpenSBI resume trampoline. OpenSBI's fw_dynamic protocol
// boots OpenSBI in M-mode; it initializes SBI services, then mrets to
// `next_addr` in S-mode.  Our trampoline at `next_addr` restores the
// callee-saved registers and stack pointer that were saved before jumping
// to OpenSBI, effectively "returning" from boot_opensbi_and_resume() in
// S-mode.
//
// Also installs a minimal S-mode trap handler (stvec) for CrabEFI.
core::arch::global_asm!(
    r#"
    .section .text
    .balign 4
    .global _opensbi_resume_trampoline
_opensbi_resume_trampoline:
    // OpenSBI mrets here in S-mode with:
    //   a0 = hart_id, a1 = DTB address
    //
    // We saved a1 (DTB) into mscratch before jumping to OpenSBI.
    // In S-mode we can't access mscratch, but a1 from OpenSBI IS the
    // DTB address (OpenSBI passes it through).  Save the S-mode DTB
    // address into sscratch so boot_dtb_addr_smode() can read it.
    csrw sscratch, a1

    // Disable S-mode interrupts.  OpenSBI may have left a timer
    // interrupt pending (STIP via MIDELEG).  CrabEFI's init path
    // logs via the `log` crate which isn't ready yet; a timer
    // interrupt hitting the trap handler before logger init would
    // deadlock on the logger lock.
    csrci sstatus, 0x2   // clear SIE (bit 1)
    csrw sie, zero        // mask all S-mode interrupt sources

    // Install the S-mode trap handler (stvec) for CrabEFI.
    la t0, _fstart_stvec_entry
    csrw stvec, t0

    // Restore callee-saved registers from the save area.
    la t0, _opensbi_resume_save_area
    ld ra,    0(t0)
    ld sp,    8(t0)
    ld s0,   16(t0)
    ld s1,   24(t0)
    ld s2,   32(t0)
    ld s3,   40(t0)
    ld s4,   48(t0)
    ld s5,   56(t0)
    ld s6,   64(t0)
    ld s7,   72(t0)
    ld s8,   80(t0)
    ld s9,   88(t0)
    ld s10,  96(t0)
    ld s11, 104(t0)

    // Return to boot_opensbi_and_resume() caller (now in S-mode).
    ret

    // Save area for callee-saved registers (16 x 8 bytes = 128 bytes).
    .section .data
    .balign 8
    .global _opensbi_resume_save_area
_opensbi_resume_save_area:
    .space 128

    // S-mode trap handler for CrabEFI.  Saves caller-saved registers,
    // reads scause/stval/sepc, and calls the Rust trap handler.
    .section .text
    .balign 4
    .global _fstart_stvec_entry
_fstart_stvec_entry:
    csrw sscratch, sp
    addi sp, sp, -128

    sd ra,   0(sp)
    sd t0,   8(sp)
    sd t1,  16(sp)
    sd t2,  24(sp)
    sd a0,  32(sp)
    sd a1,  40(sp)
    sd a2,  48(sp)
    sd a3,  56(sp)
    sd a4,  64(sp)
    sd a5,  72(sp)
    sd a6,  80(sp)
    sd a7,  88(sp)

    csrr a0, scause
    csrr a1, stval
    csrr a2, sepc

    call fstart_smode_trap_handler

    ld ra,   0(sp)
    ld t0,   8(sp)
    ld t1,  16(sp)
    ld t2,  24(sp)
    ld a0,  32(sp)
    ld a1,  40(sp)
    ld a2,  48(sp)
    ld a3,  56(sp)
    ld a4,  64(sp)
    ld a5,  72(sp)
    ld a6,  80(sp)
    ld a7,  88(sp)

    addi sp, sp, 128
    csrr sp, sscratch

    sret
    "#
);

/// S-mode trap handler for CrabEFI.
///
/// Handles S-mode exceptions (halt with UART diagnostic) and interrupts
/// (mask unexpected ones). CrabEFI's `riscv_trap_handler` will take over
/// once CrabEFI initializes, but this catches traps during the transition.
#[unsafe(no_mangle)]
pub extern "C" fn fstart_smode_trap_handler(scause: u64, stval: u64, sepc: u64) {
    // Direct UART write for diagnostics (works even if logging is broken).
    const UART: *mut u8 = 0x1000_0000 as *mut u8;
    unsafe fn uart_char(c: u8) {
        unsafe { core::ptr::write_volatile(UART, c) };
    }
    unsafe fn uart_hex_u64(val: u64) {
        for i in (0..16).rev() {
            let nibble = ((val >> (i * 4)) & 0xf) as u8;
            let c = if nibble < 10 {
                b'0' + nibble
            } else {
                b'a' + nibble - 10
            };
            unsafe { uart_char(c) };
        }
    }

    let is_interrupt = (scause >> 63) != 0;
    let code = scause & 0x7FFF_FFFF_FFFF_FFFF;

    // Print trap info: "TRAP cause=XXXX stval=XXXX sepc=XXXX\n"
    unsafe {
        for &c in b"\r\nTRAP cause=" {
            uart_char(c);
        }
        uart_hex_u64(scause);
        for &c in b" stval=" {
            uart_char(c);
        }
        uart_hex_u64(stval);
        for &c in b" sepc=" {
            uart_char(c);
        }
        uart_hex_u64(sepc);
        uart_char(b'\r');
        uart_char(b'\n');
    }

    if is_interrupt {
        // Mask the interrupt source in SIE to prevent re-firing.
        let sie_mask: u64 = match code {
            1 => 1 << 1, // SSIE (software)
            5 => 1 << 5, // STIE (timer)
            9 => 1 << 9, // SEIE (external)
            _ => 0,
        };
        if sie_mask != 0 {
            // SAFETY: clearing SIE bits is always safe.
            unsafe {
                core::arch::asm!(
                    "csrc sie, {mask}",
                    mask = in(reg) sie_mask,
                    options(nomem, nostack, preserves_flags)
                );
            }
        } else {
            // Unknown interrupt — halt.
            loop {
                // SAFETY: wfi is always safe.
                unsafe {
                    core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
                }
            }
        }
    } else {
        // Exception — halt. Nothing useful can be done.
        loop {
            // SAFETY: wfi is always safe.
            unsafe {
                core::arch::asm!("wfi", options(nomem, nostack, preserves_flags));
            }
        }
    }
}

/// Boot OpenSBI and resume in S-mode.
///
/// Saves callee-saved registers, creates a `FwDynamicInfo` pointing to
/// a resume trampoline, and jumps to OpenSBI. OpenSBI initializes SBI
/// services (timer, reset, IPI), then `mret`s to the trampoline in
/// S-mode. The trampoline restores registers and returns to the caller.
///
/// After this function returns, fstart is running in S-mode with full
/// SBI services available (timer via rdtime, reset via SBI SRST, etc.).
///
/// # Important: must not be inlined
///
/// The register-save asm captures `ra` (return address). If this function
/// is inlined, `ra` may point to an M-mode instruction (e.g., `csrr mscratch`)
/// from the caller's inline expansion. When the trampoline restores `ra`
/// and `ret`s in S-mode, that M-mode instruction causes an illegal
/// instruction trap. `#[inline(never)]` ensures `ra` = return address
/// to the caller, which is always S-mode-safe code.
///
/// # Safety
///
/// `sbi_addr` must point to a valid OpenSBI fw_dynamic binary loaded in
/// RAM. `dtb_addr` must point to a valid FDT blob.
#[inline(never)]
pub fn boot_opensbi_and_resume(sbi_addr: u64, dtb_addr: u64) {
    // Get the resume trampoline address via inline assembly to avoid
    // Rust function-pointer-to-integer cast issues (the compiler may
    // not resolve extern "C" fn symbols correctly at link time on
    // RISC-V with static relocation model).
    let resume_addr: u64;
    unsafe {
        core::arch::asm!(
            "la {out}, _opensbi_resume_trampoline",
            out = out(reg) resume_addr,
            options(nomem, nostack, preserves_flags)
        );
    }

    // Build the fw_dynamic info struct telling OpenSBI to mret to our
    // resume trampoline in S-mode.
    let info = FwDynamicInfo {
        magic: FwDynamicInfo::MAGIC,
        version: FwDynamicInfo::VERSION,
        next_addr: resume_addr,
        next_mode: FwDynamicInfo::MODE_S,
        options: 0,
        boot_hart: 0,
    };

    // SAFETY: single-threaded firmware context; save area is in .data.
    unsafe {
        // Save callee-saved registers to the static save area.
        // We save ra, sp, s0-s11 so the resume trampoline can restore them.
        core::arch::asm!(
            "la {tmp}, _opensbi_resume_save_area",
            "sd ra,    0({tmp})",
            "sd sp,    8({tmp})",
            "sd s0,   16({tmp})",
            "sd s1,   24({tmp})",
            "sd s2,   32({tmp})",
            "sd s3,   40({tmp})",
            "sd s4,   48({tmp})",
            "sd s5,   56({tmp})",
            "sd s6,   64({tmp})",
            "sd s7,   72({tmp})",
            "sd s8,   80({tmp})",
            "sd s9,   88({tmp})",
            "sd s10,  96({tmp})",
            "sd s11, 104({tmp})",
            tmp = out(reg) _,
            options(nostack)
        );
    }

    // SAFETY: caller guarantees sbi_addr and dtb_addr are valid. The
    // fence + fence.i ensure the SBI binary is visible to the I-cache.
    //
    // IMPORTANT: we do NOT use options(noreturn) here even though the
    // `jr` never falls through. The resume trampoline restores ra/sp
    // from the save area and does `ret`, effectively making this
    // function "return" via an out-of-band path. If we declared
    // noreturn, the compiler would not preserve ra across the save-
    // registers asm block, making the restored ra garbage.
    //
    // The `unimp` after `jr` is unreachable but satisfies the compiler's
    // expectation that inline asm falls through.
    unsafe {
        core::arch::asm!(
            "fence rw, rw",
            "fence.i",
            "jr {sbi}",
            "unimp",  // unreachable — OpenSBI never returns here
            sbi = in(reg) sbi_addr,
            in("a0") boot_hart_id(),
            in("a1") dtb_addr,
            in("a2") &info as *const FwDynamicInfo as u64,
        );
    }
    // Unreachable: the resume trampoline restores registers and does
    // `ret` to our caller. The compiler-generated epilogue here is
    // never executed.
}

/// Return the DTB address when running in S-mode (after OpenSBI resume).
///
/// Return the DTB address in S-mode.
///
/// The S-mode entry point (`entry_smode.rs`) and the resume trampoline
/// both save the DTB address (from OpenSBI's `a1`) into `sscratch`.
pub fn boot_dtb_addr_smode() -> u64 {
    let addr: u64;
    // SAFETY: reading sscratch retrieves the DTB address saved at S-mode entry.
    unsafe {
        core::arch::asm!("csrr {}, sscratch", out(reg) addr);
    }
    addr
}

/// Return the boot hart ID in S-mode.
///
/// The S-mode entry point (`entry_smode.rs`) saves the hart ID (from
/// OpenSBI's `a0`) into a BSS global `_smode_hart_id`.
pub fn boot_hart_id_smode() -> u64 {
    extern "C" {
        static _smode_hart_id: u64;
    }
    // SAFETY: _smode_hart_id is written by entry_smode.rs before fstart_main.
    unsafe { core::ptr::read_volatile(&_smode_hart_id) }
}

// ---------------------------------------------------------------------------
// Basic helpers
// ---------------------------------------------------------------------------

/// Halt the processor.
#[inline(always)]
pub fn halt() -> ! {
    loop {
        // SAFETY: wfi is a hint instruction that is always safe to execute; it waits
        // for the next interrupt.
        unsafe {
            core::arch::asm!("wfi");
        }
    }
}

/// Jump to an address, transferring control unconditionally.
///
/// Used by `StageLoad` and `PayloadLoad` to transfer control to the
/// next stage or payload after loading it into memory.
///
/// # Safety
///
/// The caller must ensure:
/// - `addr` points to valid executable code
/// - The stack and BSS will be set up by the target code (its own `_start`)
/// - This function never returns
#[inline(always)]
pub fn jump_to(addr: u64) -> ! {
    // SAFETY: caller guarantees entry is a valid code address in mapped memory; this
    // is a non-returning jump.
    unsafe {
        core::arch::asm!(
            "jr {0}",
            in(reg) addr,
            options(noreturn),
        );
    }
}

/// Jump to an address, passing a handoff address in `a0`.
///
/// Same as [`jump_to`], plus: `handoff_addr` must point to a valid
/// serialized `StageHandoff` in DRAM (or be 0 for no handoff).
///
/// On RISC-V the convention is to pass the handoff address in `a0`.
/// The next stage's `_start` saves `a0` and makes it available via
/// [`boot_hart_id`] or a dedicated handoff reader.
#[inline(always)]
pub fn jump_to_with_handoff(addr: u64, handoff_addr: usize) -> ! {
    // SAFETY: caller guarantees entry and handoff_ptr are valid addresses in mapped
    // memory.
    unsafe {
        core::arch::asm!(
            "jr {addr}",
            addr = in(reg) addr,
            in("a0") handoff_addr,
            options(noreturn),
        );
    }
}
