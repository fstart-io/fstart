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

/// Jump to ATF BL31, which then boots Linux at EL2.
///
/// BL31 entry convention:
/// - `x0` = pointer to `BlParams`
/// - BL31 runs at EL3, initialises secure world, then `eret`s to
///   BL33 (Linux) at EL2 with `x0` = DTB pointer.
///
/// # Safety
///
/// The caller must ensure BL31 is loaded at `bl31_addr` and that
/// `params` is valid and will remain so until BL31 reads it.
pub fn boot_linux_atf(bl31_addr: u64, params: &BlParams) -> ! {
    unsafe {
        // Ensure all stores to the params structs (and loaded images) are
        // complete and visible before transferring control to BL31, which
        // may start with caches in a different state.
        //
        // Use explicit register constraint for x0 so the compiler places
        // the params pointer directly — avoids clobbering `{bl31}` if the
        // compiler happened to allocate it to x0.
        core::arch::asm!(
            "dsb sy",   // complete all pending stores
            "isb",      // synchronise the pipeline
            "br {bl31}",
            bl31 = in(reg) bl31_addr,
            in("x0") params as *const BlParams as u64,
            options(noreturn),
        );
    }
}

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
