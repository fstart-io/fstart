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
        let mchbar = config.mchbar as u32;
        let dmibar = config.dmibar as u32;
        let epbar = config.epbar as u32;
        let rcba = config.rcba as u32;
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
            // ironlake.asl: chipset MMIO the OS must not reallocate.
            Device("PDRC") {
                Name("_HID", EisaId("PNP0C02"));
                Name("_UID", 1u32);
                Name("_CRS", ResourceTemplate {
                    Memory32Fixed(ReadWrite, #{dword rcba}, 0x4000u32);
                    Memory32Fixed(ReadWrite, #{dword mchbar}, 0x8000u32);
                    Memory32Fixed(ReadWrite, #{dword dmibar}, 0x1000u32);
                    Memory32Fixed(ReadWrite, #{dword epbar}, 0x1000u32);
                    Memory32Fixed(ReadWrite, 0xFED20000u32, 0x00020000u32);
                    Memory32Fixed(ReadWrite, 0xFED40000u32, 0x00005000u32);
                    Memory32Fixed(ReadWrite, 0xFED45000u32, 0x0004B000u32);
                });
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
            // The ECAM window, QuickPath bus included, is the host bridge's
            // own: a motherboard resource next to PCI0, outside its `_CRS`.
            sb.extend(self.host_bridge().ecam_reservation());
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
    fn the_ecam_reservation_includes_quickpath_and_is_outside_pci0() {
        for buses in [64, 128, 256] {
            let config = alloc::boxed::Box::leak(alloc::boxed::Box::new(IntelIronlakeConfig {
                ecam_buses: buses,
                ..IntelIronlakeConfig::new()
            }));
            let driver = IntelIronlake::new_from_config(config).unwrap();
            let aml = driver.dsdt_aml(config);
            let position = |name: &[u8; 4]| aml.windows(4).position(|n| n == name).unwrap();
            // \_SB_.PERC follows the PCI0 device and precedes the uncore.
            let (gfx0, perc, uncore) = (position(b"GFX0"), position(b"PERC"), position(b"UNCR"));
            assert!(gfx0 < perc && perc < uncore);
            let mut resource = alloc::vec![0x86, 9, 0, 1]; // Memory32Fixed, ReadWrite
            resource.extend_from_slice(&(config.ecam_base as u32).to_le_bytes());
            resource.extend_from_slice(&(u32::from(config.ecam_buses) << 20).to_le_bytes());
            let holds = |aml: &[u8]| aml.windows(resource.len()).any(|w| w == resource);
            assert!(holds(&aml[perc..uncore]));
            assert!(!holds(&aml[..perc]));
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
