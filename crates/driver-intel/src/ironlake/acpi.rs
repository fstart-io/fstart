//! Ironlake ACPI: coreboot `northbridge/intel/ironlake/acpi/{ironlake,
//! hostbridge,uncore}.asl`, the GMA node and the model 2065x processor
//! objects (`generate_cpu_entries()`), plus the MCFG.
//!
//! The `MCHC` CTDP methods coreboot inherits from Sandy Bridge are left
//! out: Ironlake has no configurable TDP registers behind them.

extern crate alloc;

use super::{IntelIronlake, IntelIronlakeConfig};
use alloc::vec::Vec;
use fstart_acpi::device::AcpiDevice;
use fstart_acpi_macros::acpi_dsl;

impl AcpiDevice for IntelIronlake {
    type Config = IntelIronlakeConfig;

    fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
        let qpi_bus = u32::from(config.quickpath_bus());
        let mut pci0: Vec<u8> = acpi_dsl! {
            Name("_HID", EisaId("PNP0A08"));
            Name("_CID", EisaId("PNP0A03"));
            Name("_SEG", 0u32);
            Name("_BBN", 0u32);
            Name("_UID", 0u32);
            Device("MCHC") {
                Name("_ADR", 0u32);
                OperationRegion("MCHP", PciConfig, 0x00u32, 0x100u32);
                Field("MCHP", DWordAcc, NoLock, Preserve) {
                    Offset(0x40),
                    EPEN, 1,
                    , 11,
                    EPBR, 24,
                    Offset(0x48),
                    MHEN, 1,
                    , 13,
                    MHBR, 22,
                    Offset(0x60),
                    PXEN, 1,
                    PXSZ, 2,
                    , 23,
                    PXBR, 10,
                    Offset(0x68),
                    DMEN, 1,
                    , 11,
                    DMBR, 24,
                    Offset(0xA0),
                    TOM_, 16,
                    TUUD, 16,
                    Offset(0xB0),
                    TLUD, 16,
                }
            }
        }
        .into();
        // PCI0's `_CRS` is the platform's: from the root bridge it enumerated.
        let gttmmio = self.igd_bars.map_or(0, |bars| bars.gtt_mmio);
        pci0.extend(crate::igd::gfx0_acpi_node_with::<Self>(
            gttmmio,
            crate::igd::GfxBacklight::Pch,
        ));
        let mut aml = fstart_acpi::aml_linker::scope_vec("\\_SB_", &{
            let mut sb = fstart_acpi::aml_linker::device_vec("PCI0", &pci0).expect("PCI0 emission");
            // uncore.asl: the QuickPath bus with the SAD's PAM registers.
            let uncore: Vec<u8> = acpi_dsl! {
                Device("UNCR") {
                    Name("_BBN", #{dword qpi_bus});
                    Name("RID_", 0u32);
                    Name("_HID", EisaId("PNP0A03"));
                    Name("_CRS", ResourceTemplate {
                        WordBusNumber(#{word qpi_bus as u16}, #{word qpi_bus as u16});
                    });
                    // Linux disables ASPM globally when this second root's
                    // optional _OSC is absent, even after PCI0 granted it.
                    Method("_OSC", 4, NotSerialized) {
                        If (Arg0 == ToUUID("33DB4D5B-1FF7-401C-9657-7441C03DD766")) {
                            Return(Arg3);
                        }
                        CreateDwordField(Arg3, 0u32, "CDW1");
                        CDW1 = CDW1 | 4u32;
                        Return(Arg3);
                    }
                    Device("SAD_") {
                        Name("_ADR", 1u32);
                        Name("RID_", 0u32);
                        OperationRegion("SADC", PciConfig, 0x00u32, 0x100u32);
                        Field("SADC", DWordAcc, NoLock, Preserve) {
                            Offset(0x40),
                            PAM0, 8,
                            PAM1, 8,
                            PAM2, 8,
                            PAM3, 8,
                            PAM4, 8,
                            PAM5, 8,
                            PAM6, 8,
                        }
                    }
                }
            }
            .into();
            sb.extend(uncore);
            sb
        })
        .expect("\\_SB emission");

        // generate_cpu_entries(): processor devices, PPKG and CNOT.
        #[cfg(target_os = "none")]
        let (count, limits) = (
            usize::from(fstart_arch::x86::mp::online_cpus()),
            crate::cpu::model_2065x_aml::RatioLimits::current(),
        );
        #[cfg(not(target_os = "none"))]
        let (count, limits) = (
            4,
            crate::cpu::model_2065x_aml::RatioLimits {
                min: 9,
                max: 18,
                turbo: None,
                sw_coordination: false,
                mwait_substates: 0x1120,
                fine_throttling: false,
            },
        );
        let cpus = crate::cpu::model_2065x_aml::cpu_devices_aml(count, &limits);
        aml.extend(fstart_acpi::aml_linker::scope_vec("\\", &cpus).expect("CPU scope"));
        aml
    }

    fn extra_tables(&self, config: &Self::Config) -> Vec<Vec<u8>> {
        let mut mcfg = fstart_acpi::mcfg::MCFG::new(
            fstart_acpi::OEM_ID,
            fstart_acpi::OEM_TABLE_ID,
            fstart_acpi::OEM_REVISION,
        );
        mcfg.add_ecam(config.ecam_base, 0, 0, (config.ecam_buses - 1) as u8);
        let mut bytes = Vec::new();
        fstart_acpi::Aml::to_aml_bytes(&mcfg, &mut bytes);
        alloc::vec![bytes]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntelNorthbridgeDriver;

    #[test]
    fn the_ecam_reservation_includes_quickpath() {
        for buses in [64u16, 128, 256] {
            let config = alloc::boxed::Box::leak(alloc::boxed::Box::new(IntelIronlakeConfig {
                ecam_buses: buses,
                ..IntelIronlakeConfig::new()
            }));
            let driver = IntelIronlake::new_from_config(config).unwrap();
            let root = driver.pci_root().unwrap();
            // PCI0 stops below the QuickPath bus; the motherboard resource
            // is the whole window, as in the MCFG.
            assert_eq!(usize::from(*root.buses().end()) + 2, usize::from(buses));
            let ecam = (config.ecam_base, u64::from(buses) << 20);
            assert!(
                root.motherboard()
                    .iter()
                    .any(|range| (range.base, range.size) == ecam)
            );
            assert!(
                root.forwarded()
                    .iter()
                    .all(|range| range.end() <= ecam.0 || range.base >= ecam.0 + ecam.1)
            );
        }
    }

    #[test]
    fn uncore_root_acknowledges_pcie_osc() {
        static CONFIG: IntelIronlakeConfig = IntelIronlakeConfig::new();
        let driver = IntelIronlake::new_from_config(&CONFIG).unwrap();
        let aml = driver.dsdt_aml(&CONFIG);
        let uncore = aml.windows(4).position(|name| name == b"UNCR").unwrap();
        let sad = aml[uncore..]
            .windows(4)
            .position(|name| name == b"SAD_")
            .unwrap()
            + uncore;
        assert!(aml[uncore..sad].windows(4).any(|name| name == b"_OSC"));
    }
}
