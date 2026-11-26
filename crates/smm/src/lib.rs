//! Common SMM image ABI and SMRAM layout helpers.
//!
//! This crate intentionally contains only plain data structures and placement
//! math.  The Intel gen1 flow in `fstart_arch::x86::cpu::intel::smm` and the
//! AMD64 one in `fstart_arch::x86::cpu::amd::smm` use it to parse a standalone
//! PIC SMM image, copy precompiled entry stubs into SMRAM, and fill runtime
//! data.  The SMM image crate uses the same definitions when emitting
//! native and optional coreboot-compatible headers.

#![cfg_attr(not(feature = "std"), no_std)]

pub mod buffer;
pub mod handler;
pub mod header;
pub mod installer;
pub mod layout;
pub mod runtime;
pub mod save_state;
#[cfg(feature = "stage-bin")]
pub mod stage;

pub use buffer::Region;
pub use handler::*;
pub use header::{CorebootOffsets, EntryDescriptor, HeaderError, SmmImageHeader};
pub use installer::{
    DefaultRelocationCallbackConfig, InstallConfig, InstallError, InstalledSmmImage,
    install_default_relocation_callback_stub, install_pic_image,
};
pub use layout::{
    CpuSmmLayout, LayoutError, MAPPED_LIMIT, MappedRange, SMM_IDENTITY_TABLE_SIZE,
    SMM_MAPPED_TABLES_MAX, SmramLayout, build_identity_tables, build_identity_tables_with,
    build_relocation_identity_tables, compute_common_base, compute_cpu_layout,
    compute_page_table_base, identity_table_size,
};
pub use runtime::{
    CorebootModuleArgs, HANDLER_CONFIG_ALIGNMENT, HANDLER_CONFIG_CAPACITY, SmmEntryParams,
    SmmRuntime,
};
pub use save_state::{SaveStateError, X86SaveState, X86SaveStateFormat};
#[cfg(feature = "stage-bin")]
pub use stage::handle;
