//! Reset entry of the PSP-copied reset image (FIRMWARE_IMAGE §5, CCX §3.7).
//!
//! The PSP copies BIOS directory entry `0x62` to DRAM and releases the BSP
//! at its last 16 bytes (`0x75ff_fff0`, 16-bit, CS base `0x75ff_0000`); the
//! SMU later releases each AP into the same vector. `asm/reset.S` (compiled
//! here when the build unit selects `fstart_entry = "amd-psp-reset"`, which
//! also compiles out the arch crate's own reset vector):
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

// fbuild passes `--check-cfg` for `fstart_entry` with each unit's schema;
// plain cargo builds of this crate do not know the name yet (a build.rs
// `rustc-check-cfg` line belongs to the crate's foundation files).

// The reset-image bundle links the stage runtime (allocator, anchor, the
// arch entry `_start_ram` jumps into) even before the flow calls into it.
#[cfg(feature = "bundle-reset")]
use fstart_stage as _;

/// `fstart_entry` value a reset-image build unit selects.
pub const ENTRY: &str = "amd-psp-reset";

#[cfg(all(target_os = "none", fstart_entry = "amd-psp-reset"))]
core::arch::global_asm!(
    include_str!("../asm/reset.S"),
    cache_base = const crate::layout::CACHE_WINDOW.base,
    cache_mask = const !(crate::layout::CACHE_WINDOW.size - 1) & 0xffff_ffff,
    pdpts = const crate::layout::PDPT_COUNT,
    options(att_syntax),
);

/// A real-mode far pointer (`m16:16`) APs at the reset vector jump through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApRoute {
    offset: u16,
    segment: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteError {
    /// Real-mode targets must be below 1 MiB, 16-byte aligned and not 0.
    Unreachable(u64),
}

impl ApRoute {
    /// Leave APs parked at the reset vector.
    pub const PARK: Self = Self {
        offset: 0,
        segment: 0,
    };

    /// Enter `address` with `CS = address >> 4`, `IP = 0`: the convention
    /// of a SIPI vector, so the arch trampoline runs unchanged.
    pub const fn to(address: u64) -> Result<Self, RouteError> {
        if address == 0 || address >= 0x10_0000 || !address.is_multiple_of(16) {
            return Err(RouteError::Unreachable(address));
        }
        Ok(Self {
            offset: 0,
            segment: (address >> 4) as u16,
        })
    }

    /// The word `reset.S` reads: offset low, segment high.
    pub const fn word(self) -> u32 {
        (self.segment as u32) << 16 | self.offset as u32
    }
}

#[cfg(all(target_os = "none", fstart_entry = "amd-psp-reset"))]
mod runtime {
    use super::ApRoute;

    unsafe extern "C" {
        static mut _sp3_ap_route: u32;
        static _sp3_bsp_bist: u32;
    }

    /// Arm (or disarm with [`ApRoute::PARK`]) the AP path of the reset
    /// vector. Call before the SMU releases APs: they fetch with caches
    /// off, so the word is written back to DRAM here.
    pub fn set_ap_route(route: ApRoute) {
        let word = &raw mut _sp3_ap_route;
        // SAFETY: `_sp3_ap_route` is an aligned u32 in the reset image
        // (DRAM, writable); only the BSP writes it, APs only read it.
        unsafe {
            word.write_volatile(route.word());
            fstart_arch::x86::writeback_cache_range(word.cast(), 4);
        }
    }

    /// `EAX` at the BSP's release (BIST result, 0 = passed).
    pub fn bsp_bist() -> u32 {
        // SAFETY: written once by reset.S before any Rust code runs.
        unsafe { (&raw const _sp3_bsp_bist).read_volatile() }
    }
}

#[cfg(all(target_os = "none", fstart_entry = "amd-psp-reset"))]
pub use runtime::{bsp_bist, set_ap_route};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_is_a_sipi_style_far_pointer() {
        let r = ApRoute::to(0x8000).unwrap();
        assert_eq!(r.word(), 0x0800_0000);
        assert_eq!(ApRoute::PARK.word(), 0);
        for bad in [0, 0x8008, 0x10_0000] {
            assert_eq!(ApRoute::to(bad), Err(RouteError::Unreachable(bad)));
        }
    }
}
