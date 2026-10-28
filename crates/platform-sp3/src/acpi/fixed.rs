//! FADT, MADT, HPET and MCFG (ACPI_SMM §3.1–§3.3).
//!
//! FADT/MADT/HPET go through fstart-acpi's x86 platform builder; this file
//! only supplies the SP3 values. The IOAPIC list built here is the single
//! GSI model the DSDT `_PRT`s use too.

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::fadt::{Flags, PmProfile};
use fstart_acpi::mcfg::MCFG;
use fstart_acpi::platform::{HpetConfig, IoApicConfig, IsoConfig, X86Config, X86Fadt};
use fstart_driver_amd::sp3::nbio::ioapic::{IoapicInfo, gsi_layout};
use fstart_driver_amd::sp3::soc::Generation;
use fstart_driver_amd::sp3::topology::MAX_ROOT_BRIDGES;

use super::Inputs;

/// FCH IOAPIC, GSI 0..23.
pub const FCH_IOAPIC_BASE: u64 = 0xFEC0_0000;
pub const LAPIC_BASE: u64 = 0xFEE0_0000;
/// FCH HPET (SbDxe, ACPI_SMM §3.3).
pub const HPET_BASE: u64 = 0xFED0_0000;
/// Minimum tick the donors report (SbDxe immediate).
pub const HPET_MIN_TICK: u16 = 0x37EE;

/// FCH ACPI fixed hardware: PM1a_EVT 0x800, PM1a_CNT 0x804, PM_TMR 0x808,
/// GPE0 0x820 (8 bytes), SCI on IRQ 9 (ACPI_SMM §3.1, FCH §3.2 SCI map).
pub const PM_BASE: u16 = 0x800;
pub const GPE0_OFFSET: u16 = 0x20;
pub const GPE0_LEN: u8 = 8;
pub const SCI_IRQ: u8 = 9;

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

/// One MADT IOAPIC with its GSI base, and the NBIO it belongs to (`None`
/// for the FCH IOAPIC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GsiWindow {
    pub nbio: Option<(u8, u8)>,
    pub id: u8,
    pub base: u64,
    pub gsi_base: u32,
}

/// The FCH IOAPIC at GSI 0, then the NBIO IOAPICs sorted by ID with
/// cumulative 32-pin windows (`$APIDATA` + AMI MADT, ACPI_SMM §3.2).
pub fn gsi_windows(fch_ioapic_id: u8, ioapics: &[IoapicInfo]) -> Vec<GsiWindow> {
    core::iter::once(GsiWindow {
        nbio: None,
        id: fch_ioapic_id,
        base: FCH_IOAPIC_BASE,
        gsi_base: 0,
    })
    .chain(
        gsi_layout::<MAX_ROOT_BRIDGES>(ioapics)
            .into_iter()
            .map(|(io, gsi_base)| GsiWindow {
                nbio: Some((io.socket, io.index)),
                id: io.id,
                base: io.base,
                gsi_base,
            }),
    )
    .collect()
}

/// IRQ0 → GSI2 (conforming) and IRQ9 → GSI9 level/active-low (the SCI),
/// the donors' ISO table.
fn isos() -> Vec<IsoConfig> {
    [(0u8, 2u32, 0u16), (9, 9, 0xF)]
        .iter()
        .map(|&(source, gsi, flags)| IsoConfig {
            bus: 0,
            source,
            gsi,
            flags,
        })
        .collect()
}

/// The x86 platform description: MADT, HPET and the FADT fields.
pub fn x86_config(inputs: &Inputs, windows: &[GsiWindow]) -> X86Config {
    X86Config {
        apic_ids: inputs.cpus.iter().map(|c| c.apic_id).collect(),
        lapic_base: LAPIC_BASE,
        // One all-processor NMI on LINT1, as the donors.
        lapic_nmi_lint: Some(1),
        ioapics: windows
            .iter()
            .map(|w| IoApicConfig {
                id: w.id,
                base: w.base,
                gsi_base: w.gsi_base,
            })
            .collect(),
        isos: isos(),
        hpet: Some(HpetConfig {
            base: HPET_BASE,
            timer_block_id: inputs.hpet_block_id,
            min_tick: HPET_MIN_TICK,
        }),
        legacy_devices: true,
        sci_irq: SCI_IRQ,
        pmbase: PM_BASE,
        gpe0_offset: GPE0_OFFSET,
        gpe0_length: GPE0_LEN,
        acpi_smi: inputs.acpi_smi,
        fadt: X86Fadt {
            pm_profile: PmProfile::EnterpriseServer,
            iapc_boot_arch: fstart_acpi::platform::x86::IAPC_LEGACY_DEVICES
                | if inputs.board.i8042 {
                    fstart_acpi::platform::x86::IAPC_8042
                } else {
                    0
                },
            flags: fadt_flags(inputs.generation),
            // Donor latencies: C2 100 µs, C3 > 1000 (not supported).
            p_lvl2_lat: 100,
            p_lvl3_lat: 1001,
            duty_offset: 0,
        },
    }
}

/// MCFG over the generation's ECAM window, segment 0.
pub fn mcfg(generation: Generation) -> Vec<u8> {
    let (base, buses) = generation.ecam();
    let mut mcfg = MCFG::new(
        fstart_acpi::OEM_ID,
        fstart_acpi::OEM_TABLE_ID,
        fstart_acpi::OEM_REVISION,
    );
    mcfg.add_ecam(base, 0, 0, (buses - 1) as u8);
    let mut bytes = Vec::new();
    fstart_acpi::Aml::to_aml_bytes(&mcfg, &mut bytes);
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fadt_flags_match_the_donor_pins() {
        let rr = Flags::ResetRegSup as u32;
        assert_eq!(fadt_flags(Generation::Naples) | rr, 0x305AD);
        assert_eq!(fadt_flags(Generation::Rome) | rr, 0x305AD);
        assert_eq!(fadt_flags(Generation::Milan) | rr, 0x2052D);
    }

    #[test]
    fn gsi_windows_are_cumulative_by_id() {
        let io = |index, id| IoapicInfo {
            socket: 0,
            index,
            id,
            base: 0x1000 * u64::from(index),
            bus: 0,
        };
        // NBIO3 (bus 0, the FCH NBIO) has ID 0xF4 on the donors: GSI 120.
        let w = gsi_windows(0xF0, &[io(3, 0xF4), io(0, 0xF1), io(2, 0xF3), io(1, 0xF2)]);
        let got: Vec<_> = w.iter().map(|w| (w.id, w.gsi_base)).collect();
        assert_eq!(
            got,
            [(0xF0, 0), (0xF1, 24), (0xF2, 56), (0xF3, 88), (0xF4, 120)]
        );
        assert_eq!(w[4].nbio, Some((0, 3)));
    }

    #[test]
    fn mcfg_per_generation() {
        let m = mcfg(Generation::Naples);
        assert_eq!(&m[..4], b"MCFG");
        assert_eq!(
            u64::from_le_bytes(m[44..52].try_into().unwrap()),
            0xF000_0000
        );
        assert_eq!(m[55], 127);
        let m = mcfg(Generation::Milan);
        assert_eq!(
            u64::from_le_bytes(m[44..52].try_into().unwrap()),
            0xE000_0000
        );
        assert_eq!(m[55], 255);
    }
}
