//! Common fixed-flow stage glue.
//!
//! Board crates expose a [`FirmwareBoard`] type whose [`StageRecipe`] owns the
//! handwritten stage sequence. This crate provides shared anchor/allocation
//! linkage and selected-board dispatch.

#![no_std]

#[cfg(any(feature = "acpi", feature = "ffs", feature = "crabefi"))]
extern crate fstart_alloc;

extern crate fstart_runtime;

// Link the platform crate so its entry-point/global asm reaches the stage
// binary; the generated wrapper must not name platforms.
#[cfg(feature = "x86_64")]
extern crate fstart_platform_x86_64;

#[cfg(feature = "crabefi")]
pub extern crate fstart_crabefi as crabefi;

pub mod fixed_helpers;
pub mod payload;

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

/// Stage selected by build glue for a firmware entry point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageKind {
    /// Single-stage/monolithic firmware image.
    Monolithic,
    /// Named stage in a multi-stage image.
    Named(&'static str),
}

impl StageKind {
    /// Convert the optional `FSTART_STAGE_NAME` value passed by generated wrappers.
    #[must_use]
    pub const fn from_option(name: Option<&'static str>) -> Self {
        match name {
            Some(name) => Self::Named(name),
            None => Self::Monolithic,
        }
    }

    /// Return whether this is the named stage.
    #[must_use]
    pub fn is_named(self, expected: &str) -> bool {
        matches!(self, Self::Named(name) if name == expected)
    }
}

/// Static typed firmware board selected by build glue.
pub trait FirmwareBoard: Sized + 'static {
    /// Recipe that owns this board family's handwritten stage flow.
    type Recipe: StageRecipe<Self>;

    /// Stable fstart board name.
    const NAME: &'static str;
    /// Runtime platform for this board.
    const PLATFORM: fstart_types::Platform;
}

/// Handwritten stage flow selected by a [`FirmwareBoard`].
pub trait StageRecipe<B: FirmwareBoard> {
    /// Run the selected stage.
    fn run(stage: StageKind, handoff: usize) -> !;
}

/// Dispatch to the selected board's recipe.
pub fn run_board<B: FirmwareBoard>(stage: StageKind, handoff: usize) -> ! {
    <B::Recipe as StageRecipe<B>>::run(stage, handoff)
}
