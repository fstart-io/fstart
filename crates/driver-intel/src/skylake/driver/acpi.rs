// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native client SA namespace and tables. Unlike legacy GMCH, no guessed RAM
//! geometry, above-4G pool, OpRegion, P-states or resumed-memory state.
//! Reference acpi/systemagent.asl SHA256:
//! 3bdc4ebf3c0fb397b9711cdab356edb1876aa7e976969064cf81a48350b25866
use super::*;
use crate::IntelNorthbridgeDriver;
use alloc::vec::Vec;
use fstart_acpi::{Aml, device::AcpiDevice};
use fstart_acpi_macros::acpi_dsl;

impl IntelSkylake {
    fn acpi_ready(&self, config: &Config) {
        #[cfg(not(target_os = "none"))]
        if self.host_baseline {
            assert!(core::ptr::eq(config, self.config));
            return;
        }
        assert!(
            core::ptr::eq(config, self.config)
                && self.state == State::Observed
                && self.sa_phase == SaPhase::Locked
                && self.failure.is_none(),
            "native SA ACPI before authenticated initialization/lock"
        );
        self.live_resources()
            .expect("native SA ACPI live resources");
    }
}
impl AcpiDevice for IntelSkylake {
    type Config = Config;
    fn dsdt_aml(&self, config: &Config) -> Vec<u8> {
        self.acpi_ready(config);
        // The platform renders _CRS/PDRC from the allocator's canonical
        // PciRoot. Do not duplicate resource ownership in the SA namespace.
        // Publish standard Windows _OSI detection in \_SB._INI so OSPM
        // recognizes Win8+ support (e.g. Linux native intel_backlight).
        let mut bytes: Vec<u8> = acpi_dsl! {
            Scope("\\") {
                Name("OSYS", 0x07d0u32);
            }
            Method("_INI", 0, NotSerialized) {
                If (CondRefOf(_OSI)) {
                    If (_OSI("Windows 2001")) { Store(0x07d1u32, #{const "\\OSYS"}); }
                    If (_OSI("Windows 2006")) { Store(0x07d6u32, #{const "\\OSYS"}); }
                    If (_OSI("Windows 2009")) { Store(0x07d9u32, #{const "\\OSYS"}); }
                    If (_OSI("Windows 2012")) { Store(0x07dcu32, #{const "\\OSYS"}); }
                    If (_OSI("Windows 2013")) { Store(0x07ddu32, #{const "\\OSYS"}); }
                    If (_OSI("Windows 2015")) { Store(0x07dfu32, #{const "\\OSYS"}); }
                }
            }
            Device(#{const name Self::ACPI_PCI_ROOT_NAME}) {
                Name("_HID",EisaId("PNP0A08"));
                Name("_CID",EisaId("PNP0A03"));
                Name("_SEG",0u32); Name("_BBN",0u32); Name("_UID",0u32);
                Device("MCHC") { Name("_ADR",0u32); }
            }
        }
        .into();
        if config.post_memory.devices.igd {
            let gttmmio = {
                #[cfg(not(target_os = "none"))]
                if self.host_baseline {
                    0
                } else {
                    self.scanout
                        .map(|s| s.gtt_mmio_base)
                        .or_else(|| EcamDevice::new(0, 2, 0).memory_bar(0x10))
                        .unwrap_or(0)
                }
                #[cfg(target_os = "none")]
                {
                    self.scanout
                        .map(|s| s.gtt_mmio_base)
                        .or_else(|| EcamDevice::new(0, 2, 0).memory_bar(0x10))
                        .unwrap_or(0)
                }
            };
            let igd =
                crate::igd::gfx0_acpi_node_with::<Self>(gttmmio, crate::igd::GfxBacklight::Gen9);
            bytes.extend_from_slice(
                &fstart_acpi::aml_linker::scope_vec(
                    crate::namespace::pci_root::<Self>().as_str(),
                    &igd,
                )
                .expect("IGD scope"),
            );
        }
        // UIDs are MP logical indices, matching MADT/SMM, NOT APIC IDs.
        // Power objects come from the live ratio/power/HWP MSRs.
        #[cfg(not(target_os = "none"))]
        let (cpus, limits) = if self.host_baseline {
            (
                Self::HOST_CPUS,
                crate::cpu::skylake_aml::Limits {
                    min: 8,
                    max: 32,
                    turbo: Some(36),
                    tdp_mw: 35_000,
                    hwp: true,
                    eist: true,
                },
            )
        } else {
            (
                fstart_arch::x86::mp::online_apic_ids().count(),
                crate::cpu::skylake_aml::Limits::current(),
            )
        };
        #[cfg(target_os = "none")]
        let (cpus, limits) = (
            fstart_arch::x86::mp::online_apic_ids().count(),
            crate::cpu::skylake_aml::Limits::current(),
        );
        let cpus_aml = crate::cpu::skylake_aml::cpu_devices_aml(cpus, &limits);
        bytes.extend(fstart_acpi::aml_linker::scope_vec("\\", &cpus_aml).expect("CPU scope"));
        bytes
    }
    fn extra_tables(&self, config: &Config) -> Vec<Vec<u8>> {
        self.acpi_ready(config);
        let mut mcfg = fstart_acpi::mcfg::MCFG::new(
            fstart_acpi::OEM_ID,
            fstart_acpi::OEM_TABLE_ID,
            fstart_acpi::OEM_REVISION,
        );
        mcfg.add_ecam(config.ecam.base, 0, 0, (config.ecam.buses - 1) as u8);
        let mut bytes = Vec::new();
        mcfg.to_aml_bytes(&mut bytes);
        let mut tables = alloc::vec![bytes];
        #[cfg(not(target_os = "none"))]
        if self.host_baseline {
            use fstart_acpi::dmar::{Drhd, Rmrr, dmar_with_reserved};
            let mut units = Vec::new();
            let mut reserved = Vec::new();
            if config.post_memory.devices.igd {
                units.push(Drhd {
                    include_pci_all: false,
                    segment: 0,
                    base: u64::from(early_sa::GFX_VTD_BASE),
                    endpoints: &[(0, 2, 0)],
                });
                reserved.push(Rmrr {
                    segment: 0,
                    base: 0x7b80_0000,
                    limit: 0x7fff_ffff,
                    endpoints: &[(0, 2, 0)],
                });
            }
            units.push(Drhd {
                include_pci_all: true,
                segment: 0,
                base: u64::from(early_sa::VTVC0_BASE),
                endpoints: &[],
            });
            tables.push(dmar_with_reserved(38, &units, &reserved));
            return tables;
        }
        let (resources, _) = self.live_resources().expect("live VT-d resources");
        let mch = resources.mch;
        if mch.vtvc0_bar.get() == 0 {
            return tables;
        }
        assert_eq!(mch.vtvc0_bar.get(), early_sa::VTVC0_BASE | 1);
        assert_eq!(mch.vtvc0_bar_high.get(), 0);
        use fstart_acpi::dmar::{Drhd, Rmrr, dmar_with_reserved};
        let mut units = Vec::new();
        let mut reserved = Vec::new();
        if mch.gfx_vtd_bar.get() != 0 {
            assert_eq!(mch.gfx_vtd_bar.get(), early_sa::GFX_VTD_BASE | 1);
            assert_eq!(mch.gfx_vtd_bar_high.get(), 0);
            assert!(config.post_memory.devices.igd);
            units.push(Drhd {
                include_pci_all: false,
                segment: 0,
                base: u64::from(early_sa::GFX_VTD_BASE),
                endpoints: &[(0, 2, 0)],
            });
            let map = self
                .programmed_map()
                .expect("authenticated graphics reservation")
                .plan();
            // Both GTT and graphics stolen memory; never TSEG or ME backing.
            if map.bgsm < map.tolud {
                reserved.push(Rmrr {
                    segment: 0,
                    base: map.bgsm,
                    limit: map.tolud - 1,
                    endpoints: &[(0, 2, 0)],
                });
            }
        }
        units.push(Drhd {
            include_pci_all: true,
            segment: 0,
            base: u64::from(early_sa::VTVC0_BASE),
            endpoints: &[],
        });
        // flags=0: engine routing/configuration is NOT interrupt-remapping
        // protection. Do not advertise IR without IOAPIC/HPET source scopes.
        tables.push(dmar_with_reserved(
            (fstart_arch::x86::physical_address_bits() - 1) as u8,
            &units,
            &reserved,
        ));
        tables
    }
}
