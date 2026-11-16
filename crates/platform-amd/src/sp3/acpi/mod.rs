//! What the ACPI tables of the SP3 generations differ in from the other
//! AMD SoCs' ([`crate::acpi`]), and the tables only SP3 has: the PCCT with
//! `_CPC` for the SMU-served CPPC of Rome and Milan ([`cppc`]) and EINJ,
//! restricted correctable-memory injection through the native SMM backend
//! ([`einj`]). Not built (OS-first): HEST/BERT/ERST, CRAT/CDIT.

use crate::sp3::silicon::Generation;
use fstart_acpi::fadt::{Flags, PmProfile};
use fstart_acpi::platform::{X86Fadt, x86::IAPC_LEGACY_DEVICES};
use fstart_driver_amd::fch::acpimmio;
use fstart_driver_amd::fch::acpimmio::AcpiPorts;

use crate::acpi::{S5_SLEEP_TYPE, SocAcpi, Wake, prt};

pub mod cppc;
pub mod einj;

/// The FCH's UARTs, I2C controllers and GPIO as the FCH tasks left them.
pub use fstart_driver_amd::fch::aml::FchAcpi;

/// The donor FADT flags (pins `fadt-flags-*`): 0x305AD on Naples and Rome,
/// 0x2052D on Milan, which drops RTC_S4 and S4_RTC_STS_VALID. Bits 16/17
/// are S4_RTC_STS_VALID and REMOTE_POWER_ON_CAPABLE (ACPI_SMM §3.1 corrects
/// the earlier PCI_EXP_WAK/USE_PLATFORM_CLOCK reading). RESET_REG_SUP is
/// added by the builder from the CF9 reset register.
pub const fn fadt_flags(generation: Generation) -> u32 {
    let common = Flags::Wbinvd as u32
        | Flags::ProcC1 as u32
        | Flags::PLvl2Up as u32
        | Flags::SlpButton as u32
        | Flags::TmrValExt as u32
        | Flags::RemotePowerOnCapable as u32;
    match generation {
        Generation::Naples | Generation::Rome => {
            common | Flags::RtcS4 as u32 | Flags::S4RtcStsValid as u32
        }
        Generation::Milan => common,
    }
}

const fn soc(generation: Generation) -> SocAcpi {
    let naples = matches!(generation, Generation::Naples);
    SocAcpi {
        // PM1a_EVT 0x800, PM1a_CNT 0x804, PM_TMR 0x808, GPE0 0x820 (8
        // bytes), SCI on IRQ 9 (ACPI_SMM §3.1, FCH §3.2 SCI map).
        ports: AcpiPorts {
            event: acpimmio::PM1_STS as u16,
            control: acpimmio::PM1_CNT as u16,
            timer: acpimmio::PM_TMR as u16,
            gpe: acpimmio::GPE0_STS as u16,
        },
        fadt: X86Fadt {
            pm_profile: PmProfile::EnterpriseServer,
            iapc_boot_arch: IAPC_LEGACY_DEVICES,
            flags: fadt_flags(generation),
            // Donor latencies: C2 100 µs, C3 > 1000 (not supported).
            p_lvl2_lat: 100,
            p_lvl3_lat: 1001,
            duty_offset: 0,
            duty_width: 0,
            pm2_cnt_offset: None,
        },
        bridges: if naples {
            &prt::BRIDGES_ZP
        } else {
            &prt::BRIDGES_SP3
        },
        // The donors' `_PRT`s have no entries for them.
        fch_intx_gsi: None,
        // PME of the internal bridges 7.1 and 8.1 of every NBIO (donor
        // `GPRW(0x0B, 4)`, notified by `\_GPE._L0B`) on Rome and Milan. The
        // Naples donor ASL notifies other objects (CRB names); not modelled
        // there.
        wake: if naples {
            None
        } else {
            Some(Wake {
                gpe: 0x0B,
                bridges: &[(7, 1), (8, 1)],
            })
        },
        // The xHCI behind 8.1 (function 3, donor `XHC0`) on Rome/Milan.
        xhci: if naples { None } else { Some(((8, 1), 3)) },
        sleep_states: &[(0, 0), (5, S5_SLEEP_TYPE)],
        // The donors' SLIT defaults: Naples PCD 0x112 = 0xFF → other die
        // 16; Rome/Milan "auto" (PCD 0x179 ≠ 0) with PCD 0x17A TRUE → same
        // socket 12.
        socket_distance: if naples { 16 } else { 12 },
    }
}

/// What the tables of `generation` differ in.
pub const fn of(generation: Generation) -> &'static SocAcpi {
    match generation {
        Generation::Naples => &const { soc(Generation::Naples) },
        Generation::Rome => &const { soc(Generation::Rome) },
        Generation::Milan => &const { soc(Generation::Milan) },
    }
}

#[cfg(test)]
mod tests;
