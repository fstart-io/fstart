//! AArch64 platform support.
//!
//! Provides the reset vector entry point, stack setup, BSS clearing,
//! and architecture-specific helpers.
//!
//! Two entry paths are supported:
//!
//! - **Default** (`entry.rs`): Standard AArch64 entry for platforms that
//!   start directly in AArch64 mode (e.g., QEMU virt). Captures the DTB
//!   address from `x0` at reset.
//!
//! - **Sunxi** (`entry_sunxi.rs`, behind `sunxi` feature): Entry for
//!   Allwinner sun50i SoCs (H5, A64) that boot in AArch32 from the BROM.
//!   Implements the ARMv8 RMR warm-reset sequence to switch into AArch64,
//!   with FEL state saving for USB debug mode return.

#![no_std]

use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(not(any(feature = "sunxi", feature = "sbsa")))]
pub mod entry;

#[cfg(feature = "sunxi")]
pub mod entry_sunxi;

#[cfg(feature = "sbsa")]
pub mod entry_sbsa;

// ---------------------------------------------------------------------------
// Boot parameters — written by _start assembly, read by Rust code
// ---------------------------------------------------------------------------

/// DTB address saved from `x0` at reset (written by `_start` assembly).
#[no_mangle]
static BOOT_DTB_ADDR: AtomicU64 = AtomicU64::new(0);

/// Return the DTB address passed by QEMU/firmware at reset (`x0`).
pub fn boot_dtb_addr() -> u64 {
    BOOT_DTB_ADDR.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// ARM Trusted Firmware (ATF) BL31 boot protocol
// ---------------------------------------------------------------------------

/// ATF parameter header — common header for all ATF parameter structs.
///
/// Reference: TF-A `include/common/ep_info.h`
#[repr(C)]
#[derive(Clone, Copy)]
pub struct ParamHeader {
    /// Type of parameter: `PARAM_EP` (1), `PARAM_IMAGE_BINARY` (2), etc.
    pub param_type: u8,
    /// Version of the struct.
    pub version: u8,
    /// Size of the struct in bytes.
    pub size: u16,
    /// Attributes bitfield.
    pub attr: u32,
}

/// Arguments passed to a BL image on entry (x0..x7).
///
/// For BL33 (Linux): `arg0` = DTB pointer, rest are 0.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Aapcs64Params {
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
    pub arg4: u64,
    pub arg5: u64,
    pub arg6: u64,
    pub arg7: u64,
}

/// Entry point information for a BL image.
///
/// Describes where and how to enter a particular firmware image.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct EntryPointInfo {
    /// Header: type = `PARAM_EP` (1), version = 2.
    pub h: ParamHeader,
    /// Entry point address (PC).
    pub pc: u64,
    /// Saved Program Status Register value for the target EL.
    pub spsr: u32,
    /// Padding for alignment.
    pub _pad: u32,
    /// Arguments to pass (x0..x7).
    pub args: Aapcs64Params,
}

/// BL image node in the BL image execution sequence.
///
/// TF-A's `bl_params_node_t` — uses POINTERS to `image_info` and `ep_info`.
/// This differs from `image_desc_t` which embeds them inline.
#[repr(C)]
pub struct BlParamsNode {
    /// Image ID: `BL33_IMAGE_ID` = 5.
    pub image_id: u32,
    pub _pad: u32,
    /// Pointer to `ImageInfo` (null if not needed).
    pub image_info: u64,
    /// Pointer to `EntryPointInfo` for this image.
    pub ep_info: u64,
    /// Pointer to the next `BlParamsNode` (null for last).
    pub next_params_info: u64,
}

/// Top-level BL params structure passed to BL31 in `x0`.
///
/// TF-A's `bl_params_t` — contains a pointer to the head of the
/// `BlParamsNode` linked list.
///
/// Layout: `ParamHeader` (8 bytes) + `head` pointer (8 bytes) = 16 bytes.
/// No padding needed — `head` is naturally aligned at offset 8.
#[repr(C)]
pub struct BlParams {
    /// Header: type = `PARAM_BL_PARAMS` (0x05), version = 2.
    pub h: ParamHeader,
    /// Pointer to the head of the params node list.
    pub head: u64,
}

/// ATF constants.
pub mod atf {
    /// Parameter type: entry point info.
    pub const PARAM_EP: u8 = 0x01;
    /// Parameter type: BL params (PARAM_BL_PARAMS = 0x05 in TF-A).
    pub const PARAM_BL_PARAMS: u8 = 0x05;
    /// Struct version.
    pub const VERSION_2: u8 = 0x02;
    /// Image ID for BL33 (non-secure payload = Linux).
    pub const BL33_IMAGE_ID: u32 = 5;
    /// Attribute: non-secure image (bit 0 of ep_info.h security field).
    pub const EP_NON_SECURE: u32 = 0x1;
    /// Attribute: little-endian (EP_EE_LITTLE from ep_info.h).
    pub const EP_EE_LITTLE: u32 = 0x0;

    /// Compute SPSR for EL2h entry (AArch64).
    ///
    /// SPSR_EL3: M[3:0] = 0b1001 (EL2h), DAIF masked.
    pub const fn spsr_el2h() -> u32 {
        let el2h: u32 = 0b1001; // EL2h mode
        let daif: u32 = 0xF << 6; // mask D, A, I, F
        el2h | daif
    }
}

/// Build BL params for booting Linux via ATF BL31.
///
/// All output structs must live as long as the `BlParams` is used (all
/// are typically stack-allocated before the jump).
///
/// # Arguments
/// - `kernel_addr` — Linux kernel entry point
/// - `dtb_addr` — Patched DTB address (passed as x0 to Linux)
/// - `bl33_ep` — Output: filled-in EntryPointInfo for BL33
/// - `bl33_node` — Output: filled-in BlParamsNode for BL33
/// - `bl_params` — Output: filled-in BlParams
pub fn prepare_bl_params(
    kernel_addr: u64,
    dtb_addr: u64,
    bl33_ep: &mut EntryPointInfo,
    bl33_node: &mut BlParamsNode,
    bl_params: &mut BlParams,
) {
    // Fill in BL33 entry point info
    *bl33_ep = EntryPointInfo {
        h: ParamHeader {
            param_type: atf::PARAM_EP,
            version: atf::VERSION_2,
            size: core::mem::size_of::<EntryPointInfo>() as u16,
            attr: atf::EP_NON_SECURE | atf::EP_EE_LITTLE,
        },
        pc: kernel_addr,
        spsr: atf::spsr_el2h(),
        _pad: 0,
        args: Aapcs64Params {
            arg0: dtb_addr, // x0 = DTB pointer for Linux
            ..Default::default()
        },
    };

    // Fill in BL33 params node (TF-A uses pointers, not embedded structs)
    bl33_node.image_id = atf::BL33_IMAGE_ID;
    bl33_node._pad = 0;
    bl33_node.image_info = 0; // no image info needed
    bl33_node.ep_info = bl33_ep as *mut EntryPointInfo as u64;
    bl33_node.next_params_info = 0; // no more images

    // Fill in top-level bl_params
    bl_params.h = ParamHeader {
        param_type: atf::PARAM_BL_PARAMS,
        version: atf::VERSION_2,
        size: core::mem::size_of::<BlParams>() as u16,
        attr: 0,
    };
    bl_params.head = bl33_node as *mut BlParamsNode as u64;
}

/// FSTART_BOOT_BL31 SMC function ID.
///
/// Issues an SMC to the EL3 handler which branches to BL31 at EL3.
/// Convention: x0 = function ID, x1 = BL31 address, x2 = &BlParams.
const FSTART_BOOT_BL31: u64 = 0xC200_0002;

/// Jump to ATF BL31 via SMC, which then boots the BL33 payload at EL2.
///
/// This function issues an SMC to the EL3 handler, which branches to
/// BL31 at EL3. BL31 initialises the secure world (GIC, PSCI, etc.)
/// then `eret`s to the BL33 entry specified in `params` at EL2.
///
/// # Safety
///
/// The caller must ensure BL31 is loaded at `bl31_addr` and that
/// `params` is valid and will remain so until BL31 reads it.
pub fn boot_linux_atf(bl31_addr: u64, params: &BlParams) -> ! {
    unsafe {
        // Flush all pending stores before handing off to BL31.
        // SMC traps to EL3 where the handler branches to BL31.
        core::arch::asm!(
            "dsb sy",
            "isb",
            "smc #0",
            in("x0") FSTART_BOOT_BL31,
            in("x1") bl31_addr,
            in("x2") params as *const BlParams as u64,
            options(noreturn),
        );
    }
}

// ---------------------------------------------------------------------------
// BL31 boot-and-resume (for UEFI / non-terminal BL31 integration)
// ---------------------------------------------------------------------------

/// Saved callee-saved register context for BL31 resume.
///
/// Layout: x19-x29 (11 regs), x30/LR (1 reg), SP (1 reg) = 13 × 8 = 104 bytes.
#[repr(C, align(16))]
struct ResumeContext {
    regs: [u64; 13],
}

/// Static context for the BL31 resume trampoline.
///
/// Written by [`boot_bl31_and_resume`] before the SMC, read by the
/// trampoline after BL31 ERETs to BL33.
#[no_mangle]
static mut BL31_RESUME_CTX: ResumeContext = ResumeContext { regs: [0; 13] };

/// Static BL params for BL31 boot-and-resume.
///
/// Must be static because the SMC handler reads them after the caller's
/// stack frame is conceptually gone (BL31 runs asynchronously from the
/// caller's perspective).
#[no_mangle]
static mut BL31_RESUME_EP: EntryPointInfo = EntryPointInfo {
    h: ParamHeader {
        param_type: atf::PARAM_EP,
        version: atf::VERSION_2,
        size: core::mem::size_of::<EntryPointInfo>() as u16,
        attr: atf::EP_NON_SECURE | atf::EP_EE_LITTLE,
    },
    pc: 0,
    spsr: 0,
    _pad: 0,
    args: Aapcs64Params {
        arg0: 0,
        arg1: 0,
        arg2: 0,
        arg3: 0,
        arg4: 0,
        arg5: 0,
        arg6: 0,
        arg7: 0,
    },
};

#[no_mangle]
static mut BL31_RESUME_NODE: BlParamsNode = BlParamsNode {
    image_id: atf::BL33_IMAGE_ID,
    _pad: 0,
    image_info: 0,
    ep_info: 0,
    next_params_info: 0,
};

#[no_mangle]
static mut BL31_RESUME_PARAMS: BlParams = BlParams {
    h: ParamHeader {
        param_type: atf::PARAM_BL_PARAMS,
        version: atf::VERSION_2,
        size: core::mem::size_of::<BlParams>() as u16,
        attr: 0,
    },
    head: 0,
};

/// Boot BL31 and resume execution after BL31 initialises.
///
/// This function appears to "return" from the caller's perspective,
/// but control actually flows through BL31:
///
/// 1. Saves callee-saved registers (x19-x30, SP) to a static.
/// 2. Prepares BL params with BL33 = resume trampoline.
/// 3. Issues `SMC FSTART_BOOT_BL31` → EL3 handler → BL31 at EL3.
/// 4. BL31 initialises GIC, PSCI, secure world.
/// 5. BL31 ERETs to the resume trampoline at EL2h (Non-Secure).
/// 6. Trampoline restores registers and returns to the caller.
///
/// After return, the caller runs at EL2 Non-Secure with a fully
/// initialised GIC and PSCI implementation.
pub fn boot_bl31_and_resume(bl31_addr: u64, dtb_addr: u64) {
    extern "C" {
        fn _bl31_resume_trampoline();
        fn _bl31_save_and_smc(func_id: u64, bl31_addr: u64, params: u64);
    }

    // SAFETY: firmware boot is single-threaded. The statics are only
    // written here and read by the trampoline (which runs after BL31
    // returns control). No concurrent access.
    unsafe {
        // Fill BL33 entry point: target = trampoline, mode = EL2h NS
        BL31_RESUME_EP.pc = _bl31_resume_trampoline as u64;
        BL31_RESUME_EP.spsr = atf::spsr_el2h();
        BL31_RESUME_EP.args.arg0 = dtb_addr;

        // Link the params chain
        BL31_RESUME_NODE.ep_info = &BL31_RESUME_EP as *const EntryPointInfo as u64;
        BL31_RESUME_PARAMS.head = &BL31_RESUME_NODE as *const BlParamsNode as u64;

        // Save callee-saved registers, SMC to BL31, resume via trampoline.
        _bl31_save_and_smc(
            FSTART_BOOT_BL31,
            bl31_addr,
            &BL31_RESUME_PARAMS as *const BlParams as u64,
        );
    }
}

// The save-context/SMC/trampoline is written in global_asm to avoid
// complex inline asm clobber lists.  The trampoline is the BL33 entry
// that BL31 ERETs to — it restores callee-saved registers and returns
// to the caller of boot_bl31_and_resume().
use core::arch::global_asm;
global_asm!(
    r#"
    .section .text
    .global _bl31_save_and_smc
    .type _bl31_save_and_smc, @function

    // _bl31_save_and_smc(x0=func_id, x1=bl31_addr, x2=&BlParams)
    //
    // Saves callee-saved registers (x19-x30, SP) to BL31_RESUME_CTX,
    // then issues SMC FSTART_BOOT_BL31.  Control never falls through —
    // the EL3 handler branches to BL31 which eventually ERETs to the
    // trampoline below.
_bl31_save_and_smc:
    adrp x3, BL31_RESUME_CTX
    add  x3, x3, :lo12:BL31_RESUME_CTX
    stp x19, x20, [x3, #0]
    stp x21, x22, [x3, #16]
    stp x23, x24, [x3, #32]
    stp x25, x26, [x3, #48]
    stp x27, x28, [x3, #64]
    stp x29, x30, [x3, #80]
    mov x4, sp
    str x4, [x3, #96]

    dsb sy
    isb
    smc #0
    // --- unreachable: EL3 handler branches to BL31 ---

    // BL31 resume trampoline — BL33 entry point.
    // BL31 ERETs here at EL2h NS after completing init.
    // Restores callee-saved registers and returns to the caller
    // of boot_bl31_and_resume().
    .global _bl31_resume_trampoline
    .type _bl31_resume_trampoline, @function
_bl31_resume_trampoline:
    adrp x3, BL31_RESUME_CTX
    add  x3, x3, :lo12:BL31_RESUME_CTX
    ldp x19, x20, [x3, #0]
    ldp x21, x22, [x3, #16]
    ldp x23, x24, [x3, #32]
    ldp x25, x26, [x3, #48]
    ldp x27, x28, [x3, #64]
    ldp x29, x30, [x3, #80]
    ldr x4, [x3, #96]
    mov sp, x4
    ret   // returns to caller of boot_bl31_and_resume via saved LR
"#
);

// ---------------------------------------------------------------------------
// Basic helpers
// ---------------------------------------------------------------------------

/// Halt the processor.
#[inline(always)]
pub fn halt() -> ! {
    loop {
        unsafe {
            core::arch::asm!("wfe");
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
    unsafe {
        core::arch::asm!(
            "br {0}",
            in(reg) addr,
            options(noreturn),
        );
    }
}

/// Jump to the next stage, passing the handoff address in x0.
///
/// # Safety
///
/// The caller must ensure:
/// - `addr` points to valid AArch64 executable code
/// - `handoff_addr` points to a valid serialized `StageHandoff` (or 0)
/// - This function never returns
#[inline(always)]
pub fn jump_to_with_handoff(addr: u64, handoff_addr: usize) -> ! {
    unsafe {
        core::arch::asm!(
            "br {addr}",
            addr = in(reg) addr,
            in("x0") handoff_addr,
            options(noreturn),
        );
    }
}
