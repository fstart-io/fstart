// SPDX-License-Identifier: MIT OR Apache-2.0
//! ACPI tables of the SP7 SoCs: ACPI PM blocks at `pmbase`, direct ACPI mode
//! FADT, and the 24-bridge WH NBIO IOAPIC INTx routing table
//! ([`fstart_driver_amd::nbio::wh::ROUTES`]).
//!
//! `_PRT` and the SMBIOS internal functions use each root bridge's live
//! table, [`bridges_from_devfns`] of its `IOHC_Bridge_DevFn` registers
//! (`sp7::tables`). [`BRIDGES`] (the WH routes with the RS native devfns,
//! not WH's) is only the [`SocAcpi::bridges`] fallback of a root bridge
//! whose IOHC could not be read.

extern crate alloc;

use crate::acpi::{S5_SLEEP_TYPE, SocAcpi, prt::Bridge};
use fstart_acpi::fadt::{Flags, PmProfile};
use fstart_acpi::platform::X86Fadt;
use fstart_driver_amd::dxio::rs::ports::NATIVE_DEVFNS;
use fstart_driver_amd::fch::acpimmio::AcpiPorts;
use fstart_driver_amd::nbio::wh::ROUTES;

/// Build the 24-bridge `_PRT` descriptor array from active `(device, function)`
/// pairs read from `IOHC::IOHC_Bridge_DevFn`, paired with the verified WH
/// IOAPIC INTx routing table (`SilVenicePei` `0x1430_0040..=0x1430_009C`).
pub const fn bridges_from_devfns(devfns: [(u8, u8); 24]) -> [Bridge; 24] {
    let mut bridges = [Bridge {
        device: 0,
        function: 0,
        route: ROUTES[0],
        internal: false,
    }; 24];
    let mut i = 0;
    while i < bridges.len() {
        let (device, function) = devfns[i];
        bridges[i] = Bridge {
            device,
            function,
            route: ROUTES[i],
            internal: device >= 7,
        };
        i += 1;
    }
    bridges
}

/// Default 24-bridge table using RS native DevFn values paired with verified WH
/// IOAPIC INTx routing entries.
pub const BRIDGES: [Bridge; 24] = {
    let mut devfns = [(0u8, 0u8); 24];
    let mut i = 0;
    while i < 24 {
        devfns[i] = (NATIVE_DEVFNS[i] >> 3, NATIVE_DEVFNS[i] & 7);
        i += 1;
    }
    bridges_from_devfns(devfns)
};

/// Bytes of the ACPI table storage (`sp7::tables`): 2P with 1024 threads,
/// 12 root bridges, a shared `_CPC` and every optional table fits with room
/// (`tests::two_sockets_of_b0_fit`, which measures it).
pub const TABLES_SIZE: usize = 0x8_0000;

/// The `_CPC` package every processor shares ([`shared_cpc`]).
pub const CPC_PATH: &str = "\\_SB_.CPCP";

/// The `_CPC` of the threads: one package in `\_SB` and the method each
/// processor device has, which returns it. Empty without CPPC.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedCpc {
    /// `Name(\_SB.CPCP, Package {...})`, for `\_SB`.
    pub package: alloc::vec::Vec<u8>,
    /// `Method(_CPC) { Return(\_SB.CPCP) }`, for every processor device.
    pub reference: alloc::vec::Vec<u8>,
}

/// The shared `_CPC` of `entries`, revision `revision`: the registers are
/// MSRs (FFixedHW), each thread's own, so one package describes every
/// thread; a copy per thread would cost about 450 bytes each, 450 KiB on 2P.
pub fn shared_cpc(revision: u8, entries: &[fstart_acpi::processor::CpcEntry]) -> SharedCpc {
    use fstart_acpi::processor::{cpc_named_aml, cpc_reference_aml};
    if entries.is_empty() {
        return SharedCpc::default();
    }
    SharedCpc {
        package: cpc_named_aml(CPC_PATH, revision, entries),
        reference: cpc_reference_aml(CPC_PATH),
    }
}

/// The FADT flags both donors' templates set (Dell `0x305AD`, GIGABYTE
/// `0xC5A5`; DEVICES.md §2.11 [S]): WBINVD, C1, sleep button, RTC wake
/// from S4, 32-bit timer (and `RESET_REG_SUP`, set with the reset
/// register), with Dell's `P_LVL2_UP` (C2 on MP). Left out: Dell's
/// `S4_RTC_STS_VALID` and `REMOTE_POWER_ON_CAPABLE`, GIGABYTE's
/// `PCI_EXP_WAK` and `USE_PLATFORM_CLOCK` (the donors disagree; fstart
/// offers no S4 and no PCIe wake), the century register (Dell 0x32,
/// GIGABYTE none) and the IAPC boot flags beyond the legacy devices (Dell
/// `ASPM_NOT_SUPPORTED`, GIGABYTE `8042`).
const FADT_FLAGS: u32 = Flags::Wbinvd as u32
    | Flags::ProcC1 as u32
    | Flags::PLvl2Up as u32
    | Flags::SlpButton as u32
    | Flags::RtcS4 as u32
    | Flags::TmrValExt as u32;

/// The ACPI tables of an SP7 SoC with its ACPI blocks at `pmbase`, and with
/// `legacy` LPC/eSPI devices (e.g. an AST2700 BMC Super I/O).
pub const fn soc(pmbase: u16, legacy: bool) -> SocAcpi {
    SocAcpi {
        ports: AcpiPorts {
            event: pmbase,
            control: pmbase + 4,
            timer: pmbase + 8,
            gpe: pmbase + 0x20,
        },
        fadt: X86Fadt {
            pm_profile: PmProfile::EnterpriseServer,
            iapc_boot_arch: if legacy {
                fstart_acpi::platform::x86::IAPC_LEGACY_DEVICES
            } else {
                0
            },
            flags: FADT_FLAGS,
            p_lvl2_lat: 101,
            p_lvl3_lat: 1001,
            duty_offset: 0,
            duty_width: 0,
            pm2_cnt_offset: None,
        },
        bridges: &BRIDGES,
        fch_intx_gsi: None,
        root_complex_pin: Some(1),
        wake: None,
        xhci: None,
        sleep_states: &[(0, 0), (5, S5_SLEEP_TYPE)],
        // AmdFabricWhDxe's SLIT in its auto mode: 12 within a socket, 32
        // between sockets, as both donors' `PcdAmdFabricSlitAutoRemoteFar`
        // (Dell PEI 0x184, GIGABYTE 0xDF010) = 1 selects (20 with it off;
        // 2P_COMPLETE §2.12).
        socket_distance: 12,
        remote_socket_distance: 32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wh_bridges_match_native_devfns_and_verified_routes() {
        assert_eq!(BRIDGES.len(), 24);
        assert_eq!((BRIDGES[0].device, BRIDGES[0].function), (1, 1));
        assert_eq!(BRIDGES[0].route.group, 0);
        assert_eq!(BRIDGES[6].route.group, 5);
        assert_eq!(BRIDGES[6].route.swizzle, 2);
        assert_eq!(BRIDGES[23].route.map, 4);
        assert!(BRIDGES[22].internal && !BRIDGES[21].internal);

        let venice = soc(0x800, true);
        assert_eq!((venice.ports.event, venice.ports.gpe), (0x800, 0x820));
        assert_eq!(
            (venice.socket_distance, venice.remote_socket_distance),
            (12, 32)
        );
        assert_ne!(venice.fadt.iapc_boot_arch, 0);
        // The donors' common flags with Dell's P_LVL2_UP: 0x1AD (the reset
        // register adds RESET_REG_SUP).
        assert_eq!(venice.fadt.flags, 0x1ad);
    }
}
