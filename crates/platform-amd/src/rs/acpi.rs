// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the ACPI tables of the RS SoCs differ in from the other AMD SoCs'
//! ([`crate::acpi`]): the ACPI blocks at the SoC's PM base, the FADT the
//! native flow hands over (direct ACPI mode: SCI_EN set, no SMI command
//! port), and the 24 bridges of an IOS with the routes the NBIO tables
//! program ([`fstart_driver_amd::nbio::rs::ROUTES`]).
use crate::acpi::{S5_SLEEP_TYPE, SocAcpi, prt::Bridge};
use fstart_acpi::fadt::{Flags, PmProfile};
use fstart_acpi::platform::X86Fadt;
use fstart_driver_amd::dxio::rs::ports::NATIVE_DEVFNS;
use fstart_driver_amd::fch::acpimmio::AcpiPorts;
use fstart_driver_amd::nbio::ioapic::nbio_ioapic_id;
use fstart_driver_amd::nbio::rs::ROUTES;

/// The ID the flow gives the FCH's IOAPIC.
pub const FCH_IOAPIC_ID: u8 = 0;

/// The IOAPIC ID of IOS `ios`: 0xF1 + IOS, as both vendors number them.
pub const fn ioapic_id(ios: u8) -> u8 {
    nbio_ioapic_id(fstart_driver_amd::cpu::Silicon::Gn, ios)
}

/// An IOS's bridges: its root-port slots at their native devfns, the
/// internal ones (devices 7 and 8) with the SoC's own controllers.
pub const BRIDGES: [Bridge; 24] = {
    let mut bridges = [Bridge {
        device: 0,
        function: 0,
        route: ROUTES[0],
        internal: false,
    }; 24];
    let mut i = 0;
    while i < bridges.len() {
        let devfn = NATIVE_DEVFNS[i];
        bridges[i] = Bridge {
            device: devfn >> 3,
            function: devfn & 7,
            route: ROUTES[i],
            internal: devfn >> 3 >= 7,
        };
        i += 1;
    }
    bridges
};

/// WBINVD, architectural C1, no fixed sleep button, a 32-bit PM timer: no
/// promise of S4 RTC wake, S3 or a fixed power button the flow does not
/// set up.
const FADT_FLAGS: u32 =
    Flags::Wbinvd as u32 | Flags::ProcC1 as u32 | Flags::SlpButton as u32 | Flags::TmrValExt as u32;

/// The tables of an RS SoC with its ACPI blocks at `pmbase`, and with
/// `legacy` LPC or eSPI devices (a Super I/O).
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
            // No C2 or C3 through the P_LVL ports: the C-states are `_CST`.
            p_lvl2_lat: 101,
            p_lvl3_lat: 1001,
            duty_offset: 0,
            duty_width: 0,
            pm2_cnt_offset: None,
            pm1_evt_word_access: false,
            gpe0_byte_access: false,
        },
        bridges: &BRIDGES,
        fch_intx_gsi: None,
        // The RCEC's INTA: pin 1 of every IOS, as the vendor DSDTs of
        // Genoa (`AR00`-`AR03`, `0xFFFF` entries), Siena and Turin have it.
        root_complex_pin: Some(1),
        wake: None,
        xhci: None,
        sleep_states: &[(0, 0), (5, S5_SLEEP_TYPE)],
        socket_distance: 12,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridges_are_the_native_slots() {
        assert_eq!((BRIDGES[0].device, BRIDGES[0].function), (1, 1));
        assert_eq!((BRIDGES[22].device, BRIDGES[22].function), (7, 1));
        assert!(BRIDGES[22].internal && !BRIDGES[21].internal);
        let siena = soc(0x800, true);
        assert_eq!((siena.ports.event, siena.ports.gpe), (0x800, 0x820));
    }
}
