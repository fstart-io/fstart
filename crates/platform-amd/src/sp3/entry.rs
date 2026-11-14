//! Reset entry of the PSP-copied reset image (FIRMWARE_IMAGE §5, CCX §3.7).
//!
//! The PSP copies BIOS directory entry `0x62` to DRAM and releases the BSP
//! at its last 16 bytes (`0x75ff_fff0`, 16-bit, CS base `0x75ff_0000`); the
//! SMU later releases each AP into the same vector. The arch crate's
//! shared `asm/amd_psp_reset.S` (its `amd-psp-reset` feature, which the
//! stage bundle selects; the linker script gives it the window and the
//! page-table count):
//!
//! 1. splits BSP and APs on `APIC_BAR.BSP`, as the vendor SEC does;
//! 2. BSP: saves the BIST (`EAX`), loads its GDT, enters protected mode,
//!    opens the vendor's 16 MiB write-back MTRR window
//!    ([`Layout::cache_window`]), sets SYSCFG `MtrrFixDramEn|MtrrVarDramEn`
//!    and clears `MtrrFixDramModEn` like the vendor, enables caching, builds
//!    1 GiB identity page tables over the lower canonical half
//!    ([`Layout::pages`]) and enters long mode at the arch crate's
//!    `_start_ram` (stack, BSS, IDT, `fstart_main(0)`, i.e. the board's reset
//!    `main`);
//! 3. AP: far-jumps through [`ApRoute`] when the MP code armed it, else
//!    parks in `cli; hlt` (only INIT/NMI/SMI wake it).
//!
//! The MP code uses the arch crate's SIPI trampoline for both the first
//! (SMU) release and later INIT-SIPI wakeups:
//!
//! ```ignore
//! let route = entry::ApRoute::to(mp::AP_BOOTSTRAP_ADDRESS).expect("SIPI page");
//! fstart_arch::x86::mp::mp_init_with_launcher(&config, &mut |aps| {
//!     entry::set_ap_route(route); // trampoline is installed by now
//!     smu_release_threads(aps)    // per CCD/die, CCX §3.7
//! })?;
//! entry::set_ap_route(entry::ApRoute::PARK);
//! ```
//!
//! [`Layout::cache_window`]: crate::layout::Layout::cache_window
//! [`Layout::pages`]: crate::layout::Layout::pages

// The stage bundle links the stage runtime (allocator, anchor, the
// arch entry `_start_ram` jumps into) even before the flow calls into it.
#[cfg(feature = "bundle")]
use fstart_stage as _;

pub use fstart_arch::x86::psp_reset::{ApRoute, RouteError};
#[cfg(all(target_os = "none", feature = "bundle"))]
pub use fstart_arch::x86::psp_reset::{bsp_bist, set_ap_route};
