//! Common static-stage glue for board-owned fixed-flow stage binaries.
//!
//! Board crates own concrete [`fstart_stage_runtime::StaticBoard`] adapters and
//! binary entry points. This crate provides shared anchor/allocation linkage and
//! the fixed-flow runner; it does not select boards.

#![no_std]

#[cfg(any(
    feature = "acpi",
    feature = "ffs",
    feature = "pci-ecam",
    feature = "q35-hostbridge",
    feature = "crabefi"
))]
extern crate fstart_alloc;

extern crate fstart_runtime;

pub mod fixed_helpers;

/// Fixed FFS anchor placeholder for handwritten stage flow.
///
/// `xtask assemble` patches this block in the flat stage binary after laying out
/// the complete firmware image.
#[used]
#[cfg_attr(target_os = "none", link_section = ".fstart.anchor")]
pub static FSTART_ANCHOR: fstart_types::ffs::AnchorBlock =
    fstart_types::ffs::AnchorBlock::placeholder();

#[must_use]
pub fn fstart_anchor_bytes() -> &'static [u8] {
    // SAFETY: FSTART_ANCHOR is a repr(C) static placed in `.fstart.anchor` and
    // has exactly ANCHOR_SIZE initialized bytes.
    unsafe {
        core::slice::from_raw_parts(
            (&FSTART_ANCHOR as *const fstart_types::ffs::AnchorBlock).cast::<u8>(),
            fstart_types::ffs::ANCHOR_SIZE,
        )
    }
}

pub fn run_static_board<B: fstart_stage_runtime::StaticBoard>() -> ! {
    fstart_stage_runtime::run_fixed_flow::<B>()
}
