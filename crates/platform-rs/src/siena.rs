// SPDX-License-Identifier: MIT OR Apache-2.0
//! Siena (SP6, Family 19h models A0h-AFh, package 8): the reset image and
//! APOB destinations the PSP uses, and the IOS fuses (IOS1 is fused off on
//! some parts).
use crate::{Layout, Region, Soc};
use fstart_core::layout::RegionKind;

pub struct Siena;

/// The DRAM layout: the reset image and the APOB where the BIOS directory
/// puts them, and the firmware's own regions below.
pub const LAYOUT: Layout = {
    let early = Region {
        kind: RegionKind::Writable,
        base: 0x7500_0000,
        size: 0x80_0000,
    };
    Layout {
        reset: Region {
            kind: RegionKind::Image,
            base: 0x75a0_0000,
            size: 0x60_0000,
        },
        apob: Region {
            kind: RegionKind::Reserved,
            base: 0x7590_0000,
            size: 0x10_0000,
        },
        early,
        pages: Region {
            kind: RegionKind::Reserved,
            base: early.base,
            size: (crate::PDPT_COUNT + 1) * 0x1000,
        },
        data: Region {
            kind: RegionKind::Writable,
            base: 0x7501_0000,
            size: 0xf_0000,
        },
        heap: Region {
            kind: RegionKind::Heap,
            base: 0x7510_0000,
            size: 0x60_0000,
        },
        stack: Region {
            kind: RegionKind::Stack,
            base: 0x7570_0000,
            size: 0x10_0000,
        },
        cache: Region {
            kind: RegionKind::Reserved,
            base: 0x7500_0000,
            size: 0x100_0000,
        },
        payload_heap: Region {
            kind: RegionKind::Heap,
            base: 0x7600_0000,
            size: 0x400_0000,
        },
        linux_params: Region {
            kind: RegionKind::Reserved,
            base: 0x9_0000,
            size: 0x2000,
        },
    }
};

impl Soc for Siena {
    const NAME: &'static str = "siena";
    const LAYOUT: Layout = LAYOUT;
    const SOCKET: &'static str = "SP6 Socket0";
    /// NFX FchSongshanPei (ffa512d8..).
    const PMBASE: u16 = 0x800;
    const SMU_ID_LIMIT: u32 = fstart_driver_amd::smu::v13::ID_LIMIT;
    const EQUIVALENCE: u8 = 0xaa;
    const LINUX_REQUIRES_MICROCODE: bool = false;
    const LINUX_CAPACITY: u64 = LAYOUT.reset.size;

    /// Bergamo/SP5 shares the model class but has package 4.
    fn is(signature: u32, package: u8) -> bool {
        crate::is_family19(signature, package, (0xa0, 0xaf), 8)
    }

    #[cfg(all(feature = "bundle-reset", target_os = "none"))]
    fn fabric() -> Result<heapless::Vec<fstart_driver_amd::df::v4::Root, 4>, u8> {
        use fstart_driver_amd::{df::v4, smn::Smn};
        use fstart_pci::ecam::GlobalEcam;
        use tock_registers::interfaces::Readable;
        // SAFETY: BSP owns config/SMN index/data; APs remain parked. Only
        // broadcast DF reads and the typed, read-only fuse queries below.
        let df = unsafe { v4::Df::new(GlobalEcam) };
        let smn = unsafe { Smn::new(GlobalEcam) };
        let system = df.reg(v4::SYSTEM_CONFIG).extract();
        let fused_cpuid = smn.reg(v4::FUSED_CPUID).get();
        let disable = smn.reg(v4::SIENA_IOS_DISABLE).get();
        fstart_log::info!(
            "siena: DF4 system={:#x} fused-CPUID={:#x} IOS-disable={:#x}",
            system.get(),
            fused_cpuid,
            disable
        );
        if system.get() == u32::MAX
            || !Self::is(fused_cpuid, 8)
            || system.is_set(v4::SystemConfig::OTHER_SOCKET_PRESENT)
        {
            return Err(0xea);
        }
        let ios = v4::siena_ios(disable).map_err(|_| 0xea)?;
        fstart_platform_amd::rs_discovery::fabric(&ios).map_err(|_| 0xeb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_sp3_genoa_and_turin_signatures() {
        assert!(Siena::is(0x00aa_0f02, 8));
        assert!(!Siena::is(0x00aa_0f02, 4));
        for signature in [0x0083_0f10, 0x00a0_0f11, 0x00a1_0f12, 0x00b0_0f02, 0] {
            assert!(!Siena::is(signature, 8));
        }
    }
    #[test]
    fn no_writable_region_can_overwrite_the_nfx_apob_or_reset_image() {
        crate::tests::layout_is_consistent(&LAYOUT);
        assert!(LAYOUT.apob.base >= LAYOUT.cache.base);
        assert_eq!(LAYOUT.reset.end(), Some(0x7600_0000));
        assert_eq!(LAYOUT.payload_heap.base, LAYOUT.cache.end().unwrap());
    }
}
