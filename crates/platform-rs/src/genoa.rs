// SPDX-License-Identifier: MIT OR Apache-2.0
//! Genoa (SP5, Family 19h models 10h-1Fh, package 4): the PSP's APOB at
//! 7001000h and the reset image's DRAM.
use crate::{Layout, Region, Soc};
use fstart_core::layout::RegionKind;

pub struct Genoa;

/// The DRAM layout: the 8 MiB reset image, the APOB where the PSP writes it,
/// and the firmware's own regions; runtime reservations are checked against
/// the PSP memory map.
pub const LAYOUT: Layout = {
    let early = Region {
        kind: RegionKind::Writable,
        base: 0x0800_0000,
        size: 0x200_0000,
    };
    Layout {
        reset: Region {
            kind: RegionKind::Image,
            base: 0x0a00_0000,
            size: 0x80_0000,
        },
        // The PSP writes up to 3DF58h bytes, more than coreboot's 128 KiB.
        apob: Region {
            kind: RegionKind::Reserved,
            base: 0x0700_1000,
            size: 0x4_0000,
        },
        early,
        pages: Region {
            kind: RegionKind::Reserved,
            base: early.base,
            size: (crate::PDPT_COUNT + 1) * 0x1000,
        },
        data: Region {
            kind: RegionKind::Writable,
            base: 0x0801_0000,
            size: 0xf_0000,
        },
        heap: Region {
            kind: RegionKind::Heap,
            base: 0x0810_0000,
            size: 0x1e0_0000,
        },
        stack: Region {
            kind: RegionKind::Stack,
            base: 0x09f0_0000,
            size: 0x10_0000,
        },
        cache: Region {
            kind: RegionKind::Reserved,
            base: 0x0800_0000,
            size: 0x400_0000,
        },
        payload_heap: Region {
            kind: RegionKind::Heap,
            base: 0x0c00_0000,
            size: 0x400_0000,
        },
        linux_params: Region {
            kind: RegionKind::Reserved,
            base: 0x2_0000,
            size: 0x2000,
        },
    }
};

impl Soc for Genoa {
    const NAME: &'static str = "genoa";
    const LAYOUT: Layout = LAYOUT;
    const SOCKET: &'static str = "SP5 Socket0";
    /// The PSP leaves the ACPI blocks at 400h.
    const PMBASE: u16 = 0x400;
    const SMU_ID_LIMIT: u32 = fstart_driver_amd::smu::v13::GENOA_ID_LIMIT;
    const EQUIVALENCE: u8 = 0xa1;
    const LINUX_REQUIRES_MICROCODE: bool = true;
    const LINUX_CAPACITY: u64 = 0x60_0000;

    /// Not Bergamo/Siena's A0h model class, Milan's 00h or Turin's 1Ah.
    fn is(signature: u32, package: u8) -> bool {
        crate::is_family19(signature, package, (0x10, 0x1f), 4)
    }

    /// The measured, unfused early-Genoa four-IOS topology. A nonzero Genoa
    /// fuse is not read with Siena's packed disable bits.
    #[cfg(all(feature = "bundle-reset", target_os = "none"))]
    fn fabric() -> Result<heapless::Vec<fstart_driver_amd::df::v4::Root, 4>, u8> {
        use fstart_driver_amd::{
            df::v4,
            smn::{Smn, SmnReg},
        };
        use fstart_pci::ecam::GlobalEcam;
        use tock_registers::interfaces::Readable;
        // SAFETY: BSP owns config/SMN index-data; no APs have been released.
        let df = unsafe { v4::Df::new(GlobalEcam) };
        let smn = unsafe { Smn::new(GlobalEcam) };
        let system = df.reg(v4::SYSTEM_CONFIG).extract();
        let disable = smn.reg(v4::GENOA_EARLY_IOS_DISABLE).get();
        fstart_log::info!(
            "genoa: DF4 system={:#x} fused-CPUID={:#x}",
            system.get(),
            smn.reg(v4::FUSED_CPUID).get()
        );
        for address in [0x5d27c, 0x5d280] {
            fstart_log::info!(
                "genoa: IOS fuse SMN {:#x}={:#x}",
                address,
                smn.reg(SmnReg::<()>::new(address)).get()
            );
        }
        if fstart_arch::x86::mp::CpuIdentity::current().signature != 0x00a1_0f10
            || system.get() == u32::MAX
            || system.is_set(v4::SystemConfig::OTHER_SOCKET_PRESENT)
            || disable != 0
        {
            fstart_log::error!("genoa: unsupported CPU/socket/IOS-fuse contract");
            return Err(0xea);
        }
        fstart_platform_amd::rs_discovery::fabric(&v4::PHYSICAL_IOS).map_err(|_| 0xeb)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checks_both_cpu_model_and_package() {
        for signature in [0x00a1_0f10, 0x00a1_0f11, 0x00a1_0f12] {
            assert!(Genoa::is(signature, 4));
            for package in [0, 2, 8, 15] {
                assert!(!Genoa::is(signature, package));
            }
        }
        for signature in [0, 0x0083_0f10, 0x00a0_0f11, 0x00aa_0f02, 0x00b0_0f02] {
            assert!(!Genoa::is(signature, 4));
        }
    }
    #[test]
    fn writable_storage_preserves_the_measured_apob_and_new_reset() {
        crate::tests::layout_is_consistent(&LAYOUT);
        assert_eq!(LAYOUT.reset.end(), Some(0x0a80_0000));
        assert!(LAYOUT.payload_heap.base >= LAYOUT.cache.end().unwrap());
    }
}
