//! AMD SP3 platform: the fixed boot flows of Naples, Rome and Milan, their
//! memory map, MP, SMM, PCI root bridges, ACPI tables and flash image.
//!
//! One firmware build serves one generation (one flash half); the flow
//! identifies the CPU at runtime and halts on anything else.
//!
//! A board crate implements [`board::Board`] (and
//! [`layout::Sp3BoardFacts`]) and declares its binary with
//! `fstart_platform_sp3::stage_bin!(Board);`. The flow is
//! [`flow`](crate::flow) (feature `bundle-reset`).
#![cfg_attr(not(any(test, feature = "host")), no_std)]

extern crate ufmt;

#[cfg(feature = "acpi")]
pub mod acpi;
pub mod board;
pub mod entry;
#[cfg(feature = "bundle-reset")]
mod flow;
#[cfg(feature = "host")]
pub mod host;
#[cfg(feature = "host")]
pub use host::Plan;
#[cfg(target_arch = "x86_64")]
pub mod hw;
pub mod layout;
pub mod memory;
#[cfg(target_arch = "x86_64")]
pub mod mp;
pub mod pci;
pub mod smm;

#[cfg(feature = "smm")]
pub use fstart_smm as smm_runtime;
#[cfg(feature = "bundle-reset")]
#[doc(hidden)]
pub use fstart_stage as stage_runtime;

/// The board's stage program: the reset image runs one fixed flow per
/// generation, from the PSP's release of the BSP to the payload.
#[cfg(feature = "bundle-reset")]
pub struct Program<B>(core::marker::PhantomData<B>);

#[cfg(feature = "bundle-reset")]
impl<B: board::Board> fstart_stage::StageProgram for Program<B> {
    fn run_stage(_handoff: usize) -> ! {
        flow::run::<B>()
    }
}

/// Declare the board's reset-image binary; dependency names stay inside
/// the platform.
#[cfg(feature = "bundle-reset")]
#[macro_export]
macro_rules! stage_bin {
    ($board:ty) => {
        $crate::stage_runtime::stage_bin!(program: $crate::Program<$board>);
    };
}

/// Without the reset unit's runtime there is no binary to declare.
#[cfg(not(feature = "bundle-reset"))]
#[macro_export]
macro_rules! stage_bin {
    ($board:ty) => {
        ::core::compile_error!(
            "the SP3 reset image needs fstart-platform-sp3/bundle-reset (fbuild enables it)"
        );
    };
}
