//! Reset entry of the PSP-copied reset image (FIRMWARE_IMAGE §5, CCX §3.7).
//!
//! The PSP copies BIOS directory entry `0x62` to DRAM and releases the BSP
//! at its last 16 bytes (`0x75ff_fff0`, 16-bit, CS base `0x75ff_0000`); the
//! SMU later releases each AP into the same vector. The arch crate's
//! shared `asm/amd_psp_reset.S` (compiled here when the build unit selects
//! `feature = "bundle-stage"`, which also compiles out the arch crate's own
//! reset vector):
//!
//! 1. splits BSP and APs on `APIC_BAR.BSP`, as the vendor SEC does;
//! 2. BSP: saves the BIST (`EAX`), loads its GDT, enters protected mode,
//!    opens the vendor's 16 MiB write-back MTRR window
//!    ([`layout::CACHE_WINDOW`]), sets SYSCFG `MtrrFixDramEn|MtrrVarDramEn`
//!    and clears `MtrrFixDramModEn` like the vendor, enables caching, builds
//!    1 GiB identity page tables over the lower canonical half
//!    ([`layout::PAGE_TABLES`]) and enters long mode at the arch crate's
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
//! [`layout::CACHE_WINDOW`]: crate::layout::CACHE_WINDOW
//! [`layout::PAGE_TABLES`]: crate::layout::PAGE_TABLES

// The stage bundle links the stage runtime (allocator, anchor, the
// arch entry `_start_ram` jumps into) even before the flow calls into it.
#[cfg(feature = "bundle-stage")]
use fstart_stage as _;

#[cfg(all(target_os = "none", feature = "bundle-stage"))]
core::arch::global_asm!(
    include_str!("../../arch/asm/amd_psp_reset.S"),
    cache_base = const crate::layout::CACHE_WINDOW.base,
    cache_mask = const !(crate::layout::CACHE_WINDOW.size - 1) & 0xffff_ffff,
    pdpts = const crate::layout::PDPT_COUNT,
    options(att_syntax),
);

pub use fstart_arch::x86::psp_reset::{ApRoute, RouteError};
#[cfg(all(target_os = "none", feature = "bundle-stage"))]
pub use fstart_arch::x86::psp_reset::{bsp_bist, set_ap_route};
