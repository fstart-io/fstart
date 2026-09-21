//! Common SMM image ABI and SMRAM layout helpers.
//!
//! This crate intentionally contains only plain data structures and placement
//! math.  The Intel gen1 flow in `fstart_arch::x86::cpu::intel::smm` uses it to parse
//! a standalone PIC SMM image, copy precompiled entry stubs into SMRAM, and
//! fill runtime data.  The SMM image crate uses the same definitions when emitting
//! native and optional coreboot-compatible headers.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod handler;
pub mod header;
pub mod installer;
pub mod layout;
pub mod runtime;
pub mod save_state;
#[cfg(feature = "stage-bin")]
pub mod stage;

pub use handler::*;
pub use header::{CorebootOffsets, EntryDescriptor, HeaderError, SmmImageHeader};
pub use installer::{
    DefaultRelocationCallbackConfig, InstallConfig, InstallError, InstalledSmmImage,
    install_default_relocation_callback_stub, install_pic_image,
};
pub use layout::{
    CpuSmmLayout, LayoutError, SMM_IDENTITY_TABLE_SIZE, SmramLayout, build_identity_tables,
    build_relocation_identity_tables, compute_common_base, compute_cpu_layout,
    compute_page_table_base,
};
pub use runtime::{
    CorebootModuleArgs, HANDLER_CONFIG_ALIGNMENT, HANDLER_CONFIG_CAPACITY, SmmEntryParams,
    SmmRuntime,
};
pub use save_state::{SaveStateError, X86SaveState, X86SaveStateFormat};
#[cfg(feature = "stage-bin")]
pub use stage::{SmmStageBoard, handle};
