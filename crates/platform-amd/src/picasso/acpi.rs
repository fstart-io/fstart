// SPDX-License-Identifier: MIT OR Apache-2.0
//! Picasso interrupt routing, matching the native NBIO bridge table.
use crate::acpi::{ABCD, CDAB, Route, SocAcpi};
use fstart_acpi::{
    fadt::{Flags, PmProfile},
    platform::{X86Fadt, x86::IAPC_LEGACY_DEVICES},
};

pub const ACPI: SocAcpi = SocAcpi {
    ports: fstart_driver_amd::fch::sandstone::ACPI_PORTS,
    fadt: X86Fadt {
        pm_profile: PmProfile::Mobile,
        iapc_boot_arch: IAPC_LEGACY_DEVICES,
        flags: Flags::Wbinvd as u32
            | Flags::ProcC1 as u32
            | Flags::SlpButton as u32
            | Flags::RtcS4 as u32
            | Flags::TmrValExt as u32
            | Flags::PciExpWak as u32
            | Flags::UsePlatformClock as u32
            | Flags::S4RtcStsValid as u32,
        p_lvl2_lat: 101,
        p_lvl3_lat: 1001,
        duty_offset: 0,
    },
    bridge_routes: &[
        Route::new(1, 1, 0, ABCD, 0x10),
        Route::new(1, 2, 1, ABCD, 0x11),
        Route::new(1, 3, 2, ABCD, 0x12),
        Route::new(1, 4, 3, ABCD, 0x13),
        Route::new(1, 5, 4, ABCD, 0x10),
        Route::new(1, 6, 5, ABCD, 0x11),
        Route::new(1, 7, 6, ABCD, 0x12),
        Route::new(8, 1, 7, ABCD, 0x13),
        Route::new(8, 2, 7, CDAB, 0x0c),
    ],
    fch_intx_gsi: [16, 17, 18, 19],
};
