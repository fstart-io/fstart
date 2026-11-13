// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the Zeppelin ACPI tables differ in.
use crate::acpi::{S5_SLEEP_TYPE, SocAcpi, Wake, prt};
use fstart_acpi::{
    fadt::{Flags, PmProfile},
    platform::{X86Fadt, x86::IAPC_LEGACY_DEVICES},
};
use fstart_driver_amd::fch::taishan;

pub const ACPI: SocAcpi = SocAcpi {
    ports: taishan::ACPI_PORTS,
    fadt: X86Fadt {
        // The default SMBIOS enclosure is a desktop.
        pm_profile: PmProfile::Desktop,
        iapc_boot_arch: IAPC_LEGACY_DEVICES,
        flags: Flags::Wbinvd as u32
            | Flags::ProcC1 as u32
            | Flags::SlpButton as u32
            | Flags::RtcS4 as u32
            | Flags::TmrValExt as u32
            | Flags::PciExpWak as u32
            | Flags::UsePlatformClock as u32
            | Flags::S4RtcStsValid as u32
            | Flags::RemotePowerOnCapable as u32,
        // C2/C3 not supported here, _CST would describe them.
        p_lvl2_lat: 101,
        p_lvl3_lat: 1001,
        duty_offset: 0,
        duty_width: 0,
        pm2_cnt_offset: None,
    },
    bridges: &prt::BRIDGES_ZP,
    fch_intx_gsi: Some([16, 17, 18, 19]),
    // PME of the xHCI bridge 7.1 (the vendor's _PRW(0x0B, 4) and _L0B).
    wake: Some(Wake {
        gpe: 0x0b,
        bridges: &[(7, 1)],
    }),
    xhci: None,
    // Without ACPI resume there is no S3.
    sleep_states: &[(0, 0), (1, 1), (4, 4), (5, S5_SLEEP_TYPE)],
    // The vendor's SLIT: 16 between the dies of the socket.
    socket_distance: 16,
};
