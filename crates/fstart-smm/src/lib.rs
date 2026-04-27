//! Common SMM image ABI and SMRAM layout helpers.
//!
//! This crate intentionally contains only plain data structures and placement
//! math.  Platform-specific code (Q35, Pineview+ICH7) uses it to parse a
//! standalone PIC SMM image, copy precompiled entry stubs into SMRAM, and fill
//! runtime data.  The SMM image crate uses the same definitions when emitting
//! native and optional coreboot-compatible headers.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod header;
pub mod installer;
pub mod layout;
pub mod runtime;

pub use header::{CorebootOffsets, EntryDescriptor, HeaderError, SmmImageHeader};
pub use installer::{
    install_default_relocation_handler, install_default_relocation_table_handler,
    install_pic_image, DefaultRelocationConfig, DefaultRelocationTableConfig, InstallConfig,
    InstallError, InstalledSmmImage,
};
pub use layout::{compute_common_base, compute_cpu_layout, CpuSmmLayout, LayoutError, SmramLayout};
pub use runtime::{
    CorebootModuleArgs, SmmEntryParams, SmmRuntime, SMM_PLATFORM_DATA_ICH_GPE0_STS_OFFSET,
    SMM_PLATFORM_DATA_ICH_PM_BASE, SMM_PLATFORM_INTEL_ICH, SMM_PLATFORM_NONE,
};
