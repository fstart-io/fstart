// SPDX-License-Identifier: MIT OR Apache-2.0
//! The AP side of the shared AMD PSP-copied reset entry
//! (`asm/amd_psp_reset.S`), which platforms include with their own cache
//! window and page-table count.
//!
//! The SMU releases the APs into the reset vector the BSP came through.
//! They park there until the MP code arms a route to the SIPI trampoline,
//! then use the same trampoline as for later INIT-SIPI wakeups:
//!
//! ```ignore
//! let route = ApRoute::to(mp::AP_BOOTSTRAP_ADDRESS)?;
//! mp::mp_init_with_launcher(&config, &mut |aps| {
//!     set_ap_route(route); // the trampoline is installed by now
//!     smu_release_threads(aps)
//! })?;
//! set_ap_route(ApRoute::PARK);
//! ```

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

    /// The word the reset entry reads: offset low, segment high.
    pub const fn word(self) -> u32 {
        (self.segment as u32) << 16 | self.offset as u32
    }
}

#[cfg(all(target_os = "none", feature = "amd-psp-reset"))]
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
            crate::x86::writeback_cache_range(word.cast(), 4);
        }
    }

    /// `EAX` at the BSP's release (BIST result, 0 = passed).
    pub fn bsp_bist() -> u32 {
        // SAFETY: written once by the reset entry before any Rust code runs.
        unsafe { (&raw const _sp3_bsp_bist).read_volatile() }
    }
}

#[cfg(all(target_os = "none", feature = "amd-psp-reset"))]
pub use runtime::{bsp_bist, set_ap_route};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_is_a_sipi_style_far_pointer() {
        assert_eq!(ApRoute::to(0x8000).unwrap().word(), 0x0800_0000);
        assert_eq!(ApRoute::PARK.word(), 0);
        for bad in [0, 0x8008, 0x10_0000] {
            assert_eq!(ApRoute::to(bad), Err(RouteError::Unreachable(bad)));
        }
    }
}
