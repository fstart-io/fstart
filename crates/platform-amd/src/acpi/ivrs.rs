// SPDX-License-Identifier: MIT OR Apache-2.0
//! IVRS: one IVHD set per NBIO IOMMU (ACPI_SMM §3.6, NBIO_SMU §3.7).
//!
//! Device entries as the AMD IOMMU DXE builds them: the root bridge's
//! buses from `bus:01.0` to its last bus, the NBIO IOAPIC as a special
//! device sourced from `bus:00.1`, and on the root bridge with the FCH the
//! FCH IOAPIC and HPET sourced from the FCH at `00:14.0` (the IDs Linux's
//! AMD IOMMU driver expects for the southbridge IOAPIC/HPET). Types 10h
//! and 11h on every SoC; type 40h only for an ACPI-only eMMC controller
//! (Milan's ACPI-HID UART entries are an open item).

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::ivrs::{DeviceEntry, Dte, IvrsConfig, Variety, build_ivrs};
use fstart_driver_amd::nbio::iommu::CAPABILITY;
use fstart_pci::bdf;

use super::fixed::GsiWindow;
use super::{Inputs, Iommu, RootBridgeAcpi};

/// FCH device that issues the FCH IOAPIC and HPET requests: 00:14.0.
const FCH_SOURCE: u16 = bdf(0, 0x14, 0);
/// The eMMC controller, which is not a PCI function.
const EMMC_HID: [u8; 8] = *b"AMDI0040";

/// An ACPI-only eMMC controller is among the SoC's devices.
fn emmc(inputs: &Inputs) -> bool {
    inputs
        .devices
        .iter()
        .any(|device| device.hid.as_bytes() == EMMC_HID)
}

/// The device entries of the IOMMU of root bridge `index`.
fn devices(inputs: &Inputs, index: usize, windows: &[GsiWindow]) -> Vec<DeviceEntry> {
    let rb = &inputs.root_bridges[index];
    let buses = rb.root.buses();
    // The FCH IOAPIC passes ExtINT, NMI, INIT and LINT0/1 (legacy 8259 and
    // NMI routing through the IOMMU).
    let legacy = Dte::INIT_PASS::SET
        + Dte::EXT_INT_PASS::SET
        + Dte::NMI_PASS::SET
        + Dte::LINT0_PASS::SET
        + Dte::LINT1_PASS::SET;
    let range = DeviceEntry::Range {
        start: bdf(*buses.start(), 1, 0),
        end: bdf(*buses.end(), 0x1f, 7),
        dte: 0,
    };
    let nbio_ioapic =
        windows
            .iter()
            .find(|w| w.bridge == Some(index))
            .map(|w| DeviceEntry::Special {
                variety: Variety::IoApic,
                handle: w.id,
                source: bdf(*buses.start(), 0, 1),
                dte: 0,
            });
    let fch = windows
        .iter()
        .find(|w| w.bridge.is_none())
        .filter(|_| rb.fch)
        .into_iter()
        .flat_map(|w| {
            [
                DeviceEntry::Special {
                    variety: Variety::IoApic,
                    handle: w.id,
                    source: FCH_SOURCE,
                    dte: legacy.value,
                },
                DeviceEntry::Special {
                    variety: Variety::Hpet,
                    handle: 0,
                    source: FCH_SOURCE,
                    dte: 0,
                },
            ]
        });
    // The reference assigns the ACPI-only eMMC a synthetic requester ID
    // 00:13.1. It is not an enumerated PCI function.
    let emmc = (rb.fch && emmc(inputs)).then(|| DeviceEntry::AcpiHid {
        device: bdf(0, 0x13, 1),
        hid: EMMC_HID,
        dte: (legacy + Dte::SYS_MGT::Translate).value,
    });
    core::iter::once(range)
        .chain(nbio_ioapic)
        .chain(fch)
        .chain(emmc)
        .collect()
}

/// The IVRS, or `None` without an enabled IOMMU.
pub fn ivrs(inputs: &Inputs, windows: &[GsiWindow]) -> Option<Vec<u8>> {
    let iommus: Vec<(usize, &RootBridgeAcpi, Iommu)> = inputs
        .root_bridges
        .iter()
        .enumerate()
        .filter_map(|(index, rb)| Some((index, rb, rb.iommu?)))
        .collect();
    let entries: Vec<Vec<DeviceEntry>> = iommus
        .iter()
        .map(|&(index, ..)| devices(inputs, index, windows))
        .collect();
    let blocks: Vec<fstart_acpi::ivrs::Iommu> = iommus
        .iter()
        .zip(&entries)
        .map(|(&(_, rb, iommu), devices)| fstart_acpi::ivrs::Iommu {
            device_id: bdf(*rb.root.buses().start(), 0, 2),
            capability_offset: CAPABILITY,
            segment: inputs.pci.segment,
            base: iommu.base,
            registers: iommu.registers,
            devices,
        })
        .collect();
    build_ivrs(&IvrsConfig {
        iommus: &blocks,
        acpi_hid: emmc(inputs),
    })
}
