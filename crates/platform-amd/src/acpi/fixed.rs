// SPDX-License-Identifier: MIT OR Apache-2.0
//! FADT, MADT and HPET (ACPI_SMM §3.1–§3.3).
//!
//! FADT/MADT/HPET go through fstart-acpi's x86 platform builder; this file
//! only supplies the SoC's values. The IOAPIC list built here is the single
//! GSI model the DSDT `_PRT`s and the IVRS use too.

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::platform::{HpetConfig, IoApicConfig, IsoConfig, X86Config, X86Fadt};
use fstart_driver_amd::fch;
use fstart_driver_amd::nbio::ioapic::{FCH_IOAPIC_PINS, NBIO_IOAPIC_PINS};

use super::{Inputs, RootBridgeAcpi};

/// FCH IOAPIC, GSI 0..23.
pub const FCH_IOAPIC_BASE: u64 = fch::IOAPIC as u64;
pub const LAPIC_BASE: u64 = 0xFEE0_0000;
/// FCH HPET (SbDxe, ACPI_SMM §3.3).
pub const HPET_BASE: u64 = fch::HPET as u64;
/// The FCH's GPE0 block: status and enable of 32 events.
pub const GPE0_LEN: u8 = 8;

/// One MADT IOAPIC with its GSI base, and the root bridge whose NBIO it
/// belongs to, by its index in [`Inputs::root_bridges`] (`None` for the FCH
/// IOAPIC).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GsiWindow {
    pub bridge: Option<usize>,
    pub id: u8,
    pub base: u64,
    pub gsi_base: u32,
}

/// The FCH IOAPIC at GSI 0, then the NBIO IOAPICs sorted by ID with
/// cumulative 32-pin windows (`$APIDATA` + AMI MADT, ACPI_SMM §3.2).
pub fn gsi_windows(fch_ioapic_id: u8, root_bridges: &[RootBridgeAcpi]) -> Vec<GsiWindow> {
    let mut nbio: Vec<_> = root_bridges
        .iter()
        .enumerate()
        .filter_map(|(bridge, rb)| Some((bridge, rb.ioapic?)))
        .collect();
    nbio.sort_unstable_by_key(|(_, ioapic)| ioapic.id);
    core::iter::once(GsiWindow {
        bridge: None,
        id: fch_ioapic_id,
        base: FCH_IOAPIC_BASE,
        gsi_base: 0,
    })
    .chain(
        nbio.into_iter()
            .zip(0..)
            .map(|((bridge, ioapic), n)| GsiWindow {
                bridge: Some(bridge),
                id: ioapic.id,
                base: ioapic.base,
                gsi_base: FCH_IOAPIC_PINS + NBIO_IOAPIC_PINS * n,
            }),
    )
    .collect()
}

/// IRQ0 → GSI2 (conforming) and the SCI level/active-low, the SP3 donors'
/// ISO table.
fn isos() -> Vec<IsoConfig> {
    [(0u8, 2u32, 0u16), (fch::SCI_IRQ, fch::SCI_IRQ.into(), 0xF)]
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
    let soc = inputs.soc;
    let i8042 = if inputs.board.i8042 {
        fstart_acpi::platform::x86::IAPC_8042
    } else {
        0
    };
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
            min_tick: fch::HPET_MIN_TICK,
        }),
        legacy_devices: true,
        sci_irq: fch::SCI_IRQ,
        pmbase: soc.ports.event,
        gpe0_offset: soc.ports.gpe - soc.ports.event,
        gpe0_length: GPE0_LEN,
        acpi_smi: inputs.acpi_smi,
        fadt: X86Fadt {
            iapc_boot_arch: soc.fadt.iapc_boot_arch | i8042,
            ..soc.fadt
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acpi::Ioapic;
    use crate::acpi::tests::pci_root;

    #[test]
    fn gsi_windows_are_cumulative_by_id() {
        let root = pci_root(0..=0x1f, &(0..=0xffff), &[]);
        let rb = |id: Option<u8>| RootBridgeAcpi {
            root,
            fch: false,
            domain: None,
            ioapic: id.map(|id| Ioapic {
                id,
                base: 0x1000 * u64::from(id),
            }),
            ioapic_features: 0,
            iommu: None,
        };
        // NBIO3 (bus 0, the FCH NBIO) has ID 0xF4 on the SP3 donors: GSI
        // 120. A root bridge without an IOAPIC has no window.
        let bridges = [0xF4, 0xF1, 0xF3, 0xF2].map(|id| rb(Some(id)));
        let w = gsi_windows(0xF0, &[&bridges[..2], &[rb(None)], &bridges[2..]].concat());
        let got: Vec<_> = w.iter().map(|w| (w.id, w.gsi_base)).collect();
        assert_eq!(
            got,
            [(0xF0, 0), (0xF1, 24), (0xF2, 56), (0xF3, 88), (0xF4, 120)]
        );
        assert_eq!(w[4].bridge, Some(0));
        assert_eq!(w[2].bridge, Some(4));
    }
}
