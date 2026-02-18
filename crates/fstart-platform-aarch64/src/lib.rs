//! AArch64 platform support.
//!
//! Provides the reset vector entry point, stack setup, BSS clearing,
//! and architecture-specific helpers. Captures the DTB address passed
//! by QEMU at reset.

#![no_std]

use core::sync::atomic::{AtomicU64, Ordering};

pub mod entry;

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

/// Descriptor for a single BL image in the params list.
#[repr(C)]
pub struct ImageDesc {
    /// Image ID: `BL33_IMAGE_ID` = 5.
    pub image_id: u32,
    pub _pad: u32,
    /// Entry point info for this image.
    pub ep_info: EntryPointInfo,
    /// Pointer to the next descriptor (null for last).
    pub next: u64,
}

/// Top-level BL params structure passed to BL31 in `x0`.
///
/// Contains the linked list of image descriptors that BL31 uses
/// to determine where to jump after initialization.
#[repr(C)]
pub struct BlParams {
    /// Header: type = `PARAM_BL_PARAMS` (3), version = 2.
    pub h: ParamHeader,
    /// Pointer to the head of the image descriptor list.
    pub head: u64,
}

/// ATF constants.
pub mod atf {
    /// Parameter type: entry point info.
    pub const PARAM_EP: u8 = 0x01;
    /// Parameter type: BL params.
    pub const PARAM_BL_PARAMS: u8 = 0x03;
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
/// `image_desc` must live as long as the `BlParams` is used (both
/// are typically stack-allocated before the jump).
///
/// # Arguments
/// - `kernel_addr` — Linux kernel entry point
/// - `dtb_addr` — Patched DTB address (passed as x0 to Linux)
/// - `image_desc` — Output: filled-in ImageDesc for BL33
/// - `bl_params` — Output: filled-in BlParams
pub fn prepare_bl_params(
    kernel_addr: u64,
    dtb_addr: u64,
    image_desc: &mut ImageDesc,
    bl_params: &mut BlParams,
) {
    // Fill in BL33 entry point info
    image_desc.image_id = atf::BL33_IMAGE_ID;
    image_desc._pad = 0;
    image_desc.ep_info = EntryPointInfo {
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
    image_desc.next = 0; // no more images

    // Fill in top-level bl_params
    bl_params.h = ParamHeader {
        param_type: atf::PARAM_BL_PARAMS,
        version: atf::VERSION_2,
        size: core::mem::size_of::<BlParams>() as u16,
        attr: 0,
    };
    bl_params.head = image_desc as *mut ImageDesc as u64;
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
        // Use explicit register constraint for x0 so the compiler places
        // the params pointer directly — avoids clobbering `{bl31}` if the
        // compiler happened to allocate it to x0.
        core::arch::asm!(
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
