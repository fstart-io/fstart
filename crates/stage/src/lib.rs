//! Board-owned stage entry and shared boot services.
//!
//! Platform phases authenticate bootstrap stages through [`root`] and [`boot`].
//! Payload launchers live in `fstart-payload`; this crate supplies their mounted
//! directory, verified file loading, allocation and handoff support.

#![no_std]

// The stage's allocator, when it owns the binary. Host builds (tools) keep
// the host's allocator; host tests exercise the heap.
#[cfg(all(feature = "runtime", feature = "alloc", any(target_os = "none", test)))]
mod alloc;
// Cargo cannot select the allocator's crates on that condition, only on the
// `alloc` feature.
#[cfg(all(
    feature = "alloc",
    not(all(feature = "runtime", any(target_os = "none", test)))
))]
use {linked_list_allocator as _, spin as _};

// A heapless stage's allocator: linking fails if it can allocate.
#[cfg(all(feature = "runtime", not(feature = "alloc"), target_os = "none"))]
mod no_heap;
#[cfg(feature = "runtime")]
mod runtime;

#[cfg(feature = "alloc")]
extern crate alloc as heap;

// The arch crate supplies entry code and the early FFS anchor.
use fstart_arch as _;

#[cfg(feature = "bootstrap")]
pub mod boot;
#[cfg(feature = "bootstrap")]
mod fdt_workspace;
#[cfg(feature = "fdt")]
pub(crate) use fdt_workspace::fdt_prepare_platform;
#[cfg(feature = "bootstrap")]
pub use fdt_workspace::{configure_fdt_workspace, copy_fdt_to_workspace, fdt_workspace_capacity};
#[cfg(feature = "ffs")]
pub mod directory;
#[cfg(feature = "ffs")]
mod loaded;
#[cfg(feature = "bootstrap")]
pub mod root;

pub mod fixed_helpers;
pub mod layout;
#[cfg(feature = "tcg")]
pub mod tcg;
pub mod timestamps;

#[cfg(feature = "fit")]
pub mod fit;

#[cfg(feature = "handoff")]
pub mod handoff;

pub mod next_stage;

#[cfg(all(test, feature = "ffs"))]
mod ram_locator_tests;

#[cfg(feature = "ffs")]
mod ffs_helpers;
#[cfg(feature = "ffs")]
pub(crate) use ffs_helpers::{load_ffs_file_by_name, load_file_segments_from_media};
#[cfg(feature = "ffs")]
pub use ffs_helpers::{load_ffs_file_measured, microcode_blob};

pub mod anchor;
pub use anchor::{fstart_anchor_bytes, fstart_trust_bytes};

/// Stop the boot: log what failed and halt.
#[inline(never)]
pub fn fail(what: &str) -> ! {
    fstart_log::error!("{} failed", what);
    fstart_arch::halt()
}

/// Run one named boot step, retaining its error in the failure log.
pub fn step<T, E: ufmt::uDebug>(name: &str, f: impl FnOnce() -> Result<T, E>) -> T {
    fstart_log::info!("{}", name);
    match f() {
        Ok(value) => value,
        Err(error) => {
            fstart_log::error!("{} failed: {:?}", name, error);
            fstart_arch::halt()
        }
    }
}

/// Stage setup before the board's `main`: hand the heap to the allocator.
#[doc(hidden)]
#[inline(always)]
pub fn enter() {
    #[cfg(all(feature = "runtime", feature = "alloc", target_os = "none"))]
    alloc::init();
}

/// Bind a board's stage `main` to the stage entry (`fstart_main`). Platform
/// `entry!` macros expand to this.
///
/// Platforms that offer the CrabEFI payload add `sbi_resume` so RISC-V
/// stages export the SBI resume entry. Only a CrabEFI launch has OpenSBI
/// return there; the platforms' resume halts in other builds.
#[macro_export]
macro_rules! stage_bin {
    // A board-owned stage `main`: the binary is the flow.
    (main: $main:path) => {
        #[unsafe(no_mangle)]
        pub extern "Rust" fn fstart_main(_handoff: usize) -> ! {
            $crate::enter();
            $main()
        }
        #[used]
        #[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.keep"))]
        static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
    };
    // A board-owned `main` that takes the predecessor's handoff address.
    (main_handoff: $main:path) => {
        #[unsafe(no_mangle)]
        pub extern "Rust" fn fstart_main(handoff: usize) -> ! {
            $crate::enter();
            $main(handoff)
        }
        #[used]
        #[cfg_attr(target_os = "none", unsafe(link_section = ".fstart.keep"))]
        static FSTART_MAIN_KEEP: extern "Rust" fn(usize) -> ! = fstart_main;
    };
    // A board-owned `main` plus the function OpenSBI returns to before a
    // RISC-V CrabEFI launch.
    (main: $main:path, sbi_resume: $resume:path) => {
        $crate::stage_bin!(main: $main);
        #[cfg(target_arch = "riscv64")]
        #[unsafe(no_mangle)]
        pub extern "C" fn fstart_sbi_resume(hart_id: u64, dtb_addr: u64) -> ! {
            $resume(hart_id, dtb_addr)
        }
        #[cfg(not(target_arch = "riscv64"))]
        const _: fn(u64, u64) -> ! = $resume;
    };
}
