//! IVRS: one IVHD set per NBIO IOMMU (ACPI_SMM §3.6, NBIO_SMU §3.7).
//!
//! Device entries as the AMD IOMMU DXE builds them: the NBIO's buses from
//! `bus:01.0` to its last bus, the NBIO IOAPIC as a special device sourced
//! from `bus:00.1`, and on the FCH NBIO the FCH IOAPIC and HPET sourced
//! from the FCH at `00:14.0` (the IDs Linux's AMD IOMMU driver expects for
//! the southbridge IOAPIC/HPET). Types 10h and 11h on every generation;
//! type 40h (Milan's ACPI-HID UART entries) is not emitted (open item).

extern crate alloc;

use alloc::vec::Vec;

use fstart_acpi::ivrs::{DeviceEntry, Dte, Iommu, IommuRegisters, IvrsConfig, Variety, build_ivrs};
use fstart_driver_amd::sp3::nbio::iommu::IommuInfo;

use super::fixed::GsiWindow;

/// One IOMMU as IVRS needs it.
#[derive(Debug, Clone, Copy)]
pub struct IommuAcpi {
    pub info: IommuInfo,
    /// Capability/MMIO register values read from the enabled IOMMU.
    pub registers: IommuRegisters,
    /// Last bus of the IOMMU's root bridge.
    pub bus_limit: u8,
}

/// FCH device that issues the FCH IOAPIC and HPET requests: 00:14.0.
const FCH_SOURCE: u16 = 0x14 << 3;

const fn bdf(bus: u8, device: u8, function: u8) -> u16 {
    (bus as u16) << 8 | (device as u16) << 3 | function as u16
}

fn devices(iommu: &IommuAcpi, windows: &[GsiWindow]) -> Vec<DeviceEntry> {
    let i = &iommu.info;
    // The FCH IOAPIC passes ExtINT, NMI, INIT and LINT0/1 (legacy 8259 and
    // NMI routing through the IOMMU).
    let legacy = (Dte::INIT_PASS::SET
        + Dte::EXT_INT_PASS::SET
        + Dte::NMI_PASS::SET
        + Dte::LINT0_PASS::SET
        + Dte::LINT1_PASS::SET)
        .value;
    let range = DeviceEntry::Range {
        start: bdf(i.bus, 1, 0),
        end: bdf(iommu.bus_limit, 0x1f, 7),
        dte: 0,
    };
    let nbio_ioapic = windows
        .iter()
        .find(|w| w.nbio == Some((i.socket, i.index)))
        .map(|w| DeviceEntry::Special {
            variety: Variety::IoApic,
            handle: w.id,
            source: bdf(i.bus, 0, 1),
            dte: 0,
        });
    let fch = windows
        .iter()
        .find(|w| w.nbio.is_none())
        .filter(|_| i.fch)
        .into_iter()
        .flat_map(|w| {
            [
                DeviceEntry::Special {
                    variety: Variety::IoApic,
                    handle: w.id,
                    source: FCH_SOURCE,
                    dte: legacy,
                },
                DeviceEntry::Special {
                    variety: Variety::Hpet,
                    handle: 0,
                    source: FCH_SOURCE,
                    dte: 0,
                },
            ]
        });
    core::iter::once(range)
        .chain(nbio_ioapic)
        .chain(fch)
        .collect()
}

/// The IVRS, or `None` without an enabled IOMMU.
pub fn ivrs(iommus: &[IommuAcpi], windows: &[GsiWindow]) -> Option<Vec<u8>> {
    let entries: Vec<Vec<DeviceEntry>> = iommus.iter().map(|m| devices(m, windows)).collect();
    let blocks: Vec<Iommu> = iommus
        .iter()
        .zip(&entries)
        .map(|(m, devices)| Iommu {
            device_id: bdf(m.info.bus, 0, 0) | u16::from(m.info.devfn),
            capability_offset: m.info.cap_offset.into(),
            segment: m.info.segment,
            base: m.info.base,
            registers: m.registers,
            devices,
        })
        .collect();
    build_ivrs(&IvrsConfig {
        iommus: &blocks,
        acpi_hid: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acpi::fixed::gsi_windows;
    use fstart_driver_amd::sp3::nbio::ioapic::IoapicInfo;

    fn iommu(index: u8, bus: u8, fch: bool) -> IommuAcpi {
        IommuAcpi {
            info: IommuInfo {
                socket: 0,
                index,
                bus,
                devfn: 2,
                cap_offset: 0x40,
                base: 0xB000_0000 + (u64::from(index) << 19),
                segment: 0,
                fch,
            },
            registers: IommuRegisters {
                capability: 1 << 27,
                ..Default::default()
            },
            bus_limit: bus + 0x3f,
        }
    }

    #[test]
    fn fch_nbio_gets_the_fch_entries() {
        let windows = gsi_windows(
            0xF0,
            &[
                IoapicInfo {
                    socket: 0,
                    index: 3,
                    id: 0xF4,
                    base: 0xE780_0000,
                    bus: 0,
                },
                IoapicInfo {
                    socket: 0,
                    index: 0,
                    id: 0xF1,
                    base: 0xCF00_0000,
                    bus: 0xC0,
                },
            ],
        );
        let fch = devices(&iommu(3, 0, true), &windows);
        assert_eq!(fch.len(), 4);
        assert!(fch.contains(&DeviceEntry::Special {
            variety: Variety::IoApic,
            handle: 0xF4,
            source: 0x0001,
            dte: 0,
        }));
        assert!(fch.contains(&DeviceEntry::Special {
            variety: Variety::IoApic,
            handle: 0xF0,
            source: 0x00A0,
            dte: 0xC7,
        }));
        let other = devices(&iommu(0, 0xC0, false), &windows);
        assert_eq!(
            other[0],
            DeviceEntry::Range {
                start: 0xC008,
                end: 0xFFFF,
                dte: 0
            }
        );
        assert_eq!(other.len(), 2);

        let t = ivrs(&[iommu(3, 0, true), iommu(0, 0xC0, false)], &windows).unwrap();
        assert_eq!(&t[..4], b"IVRS");
        assert_eq!(t.iter().fold(0u8, |a, &b| a.wrapping_add(b)), 0);
        // 10h + 11h per IOMMU (EFR supported).
        assert_eq!(t[48], 0x10);
        assert!(ivrs(&[], &windows).is_none());
    }
}
