//! Host bridge ACPI (coreboot `sandybridge/acpi/*.asl`, `acpi.c` and the
//! `mc_gen_ssdt`/`generate_cpu_entries` SSDT parts): `PCI0` with `MCHC`,
//! `PDRC`, the producer `_CRS`, the PEG ports and `GFX0`; the CPU
//! namespace; MCFG and DMAR.

extern crate alloc;

use super::*;
use alloc::vec::Vec;
use fstart_acpi::device::AcpiDevice;
use fstart_acpi_macros::acpi_dsl;

/// coreboot `PCH_IOAPIC_PCI_BUS/SLOT`, `PCH_HPET_PCI_BUS/SLOT`.
const PCH_IOAPIC_SCOPE: (u8, u8, u8) = (250, 31, 0);
const PCH_HPET_SCOPE: (u8, u8) = (250, 15);
const IOAPIC_BASE: u64 = 0xfec0_0000;

/// One PEG root port: `_STA` from its DEVEN bit and a `_PRT` with the
/// coreboot `IRQM(n)` rotation of PIRQ A..D (GSI 16..19).
fn peg_port(name: &str, adr: u32, present: bool, rotation: u32) -> Vec<u8> {
    let gsi = |pin: u32| 16 + (pin + rotation) % 4;
    let (a, b, c, d) = (gsi(0), gsi(1), gsi(2), gsi(3));
    // DEVEN is final when the tables are built (coreboot evaluates it).
    let sta = if present { 0x0fu32 } else { 0 };
    let body: Vec<u8> = acpi_dsl! {
        Name("_ADR", #{dword adr});
        Name("_STA", #{dword sta});
        Device("DEV0") { Name("_ADR", 0u32); }
        Name("_PRT", Package(
            Package(0x0000FFFFu32, 0u32, 0u32, #{dword a}),
            Package(0x0000FFFFu32, 1u32, 0u32, #{dword b}),
            Package(0x0000FFFFu32, 2u32, 0u32, #{dword c}),
            Package(0x0000FFFFu32, 3u32, 0u32, #{dword d})
        ));
    }
    .into();
    fstart_acpi::aml_linker::device_vec(name, &body).expect("PEG port AML")
}

impl AcpiDevice for IntelSandybridge {
    type Config = IntelSandybridgeConfig;

    fn dsdt_aml(&self, config: &Self::Config) -> Vec<u8> {
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
                    EPBR, 27,
                    Offset(0x48),
                    MHEN, 1,
                    , 14,
                    MHBR, 24,
                    Offset(0x54),
                    DVEN, 32,
                    Offset(0x60),
                    PXEN, 1,
                    PXSZ, 2,
                    , 23,
                    PXBR, 13,
                    Offset(0x68),
                    DMEN, 1,
                    , 11,
                    DMBR, 27,
                    Offset(0x70),
                    MEBA, 64,
                    Offset(0xA0),
                    TOM_, 64,
                    Offset(0xBC),
                    TLUD, 32,
                }
            }
        }
        .into();
        // PCI0's `_CRS` and `PDRC` are the platform's: from the root bridge
        // it enumerated.
        // coreboot peg.asl: IRQM(1..4) rotations for 01.0, 01.1, 01.2, 06.0.
        // DEVEN is final when the tables are built; without ECAM (host
        // builds) no PEG port is present.
        let deven = tock_registers::LocalRegisterCopy::<u32, DEVEN::Register>::new(
            ecam::EcamDevice::new(0, 0, 0).try_read32(0x54).unwrap_or(0),
        );
        pci0.extend(peg_port("PEGP", 0x0001_0000, deven.is_set(DEVEN::PEG10), 0));
        pci0.extend(peg_port("PEG1", 0x0001_0001, deven.is_set(DEVEN::PEG11), 1));
        pci0.extend(peg_port("PEG2", 0x0001_0002, deven.is_set(DEVEN::PEG12), 2));
        pci0.extend(peg_port("PEG6", 0x0006_0000, deven.is_set(DEVEN::PEG60), 3));
        let gttmmio = self.igd_bars.map_or(0, |bars| bars.gtt_mmio);
        pci0.extend(crate::igd::gfx0_acpi_node_with::<Self>(
            gttmmio,
            crate::igd::GfxBacklight::Pch,
        ));
        let mut aml = fstart_acpi::aml_linker::scope_vec(
            "\\_SB_",
            &fstart_acpi::aml_linker::device_vec("PCI0", &pci0).expect("PCI0 emission"),
        )
        .expect("\\_SB emission");
        #[cfg(target_os = "none")]
        let cpus = crate::cpu::model_206ax_aml::current_cpu_devices_aml(
            usize::from(fstart_arch::x86::mp::online_cpus()),
            &config.cpu.acpi_cstates,
        );
        #[cfg(not(target_os = "none"))]
        let cpus = crate::cpu::model_206ax_aml::cpu_devices_aml(
            IntelSandybridge::HOST_CPUS,
            IntelSandybridge::HOST_CPUS as u32,
            &config.cpu.acpi_cstates,
            &fstart_arch::x86::cpu::intel::model_206ax::PstateLimits {
                ratio_min: 8,
                ratio_max: 25,
                ratio_turbo: Some(32),
                power_max_mw: 35_000,
                hw_coordination: true,
            },
            0x0002_1120,
            true,
        );
        aml.extend(fstart_acpi::aml_linker::scope_vec("\\", &cpus).expect("CPU AML scope"));
        aml
    }

    /// MCFG for PCIEXBAR and coreboot `acpi.c`'s DMAR.
    fn extra_tables(&self, config: &Self::Config) -> Vec<Vec<u8>> {
        let mut mcfg = fstart_acpi::mcfg::MCFG::new(
            fstart_acpi::OEM_ID,
            fstart_acpi::OEM_TABLE_ID,
            fstart_acpi::OEM_REVISION,
        );
        mcfg.add_ecam(config.ecam_base, 0, 0, (config.ecam_buses - 1) as u8);
        let mut mcfg_bytes = Vec::new();
        fstart_acpi::Aml::to_aml_bytes(&mcfg, &mut mcfg_bytes);
        let mut tables = alloc::vec![mcfg_bytes];
        if !self.dram_ready || self.host().capid0_a.is_set(CAPID0_A::VTDD) {
            return tables;
        }
        use fstart_acpi::dmar::{
            Drhd, InterruptScope, InterruptSource, Rmrr, dmar_with_interrupts,
        };
        let igd = self.igd_enabled();
        let igd_endpoints = [(0, 2, 0)];
        // SAFETY: the PCH I/O APIC is decoded at its fixed base; table
        // generation runs single-threaded on the BSP.
        let ioapic_id = unsafe { fstart_arch::x86::ioapic::IoApic::new(IOAPIC_BASE as usize) }.id();
        let mut interrupts: Vec<InterruptScope> = alloc::vec![InterruptScope {
            unit_base: VTVC0_BASE,
            source: InterruptSource::IoApic(ioapic_id),
            bdf: PCH_IOAPIC_SCOPE,
        }];
        interrupts.extend((0..8).map(|function| InterruptScope {
            unit_base: VTVC0_BASE,
            source: InterruptSource::Hpet(0),
            bdf: (PCH_HPET_SCOPE.0, PCH_HPET_SCOPE.1, function),
        }));
        let mut units = Vec::new();
        if igd {
            units.push(Drhd {
                include_pci_all: false,
                segment: 0,
                base: GFXVT_BASE,
                endpoints: &igd_endpoints,
            });
        }
        units.push(Drhd {
            include_pci_all: true,
            segment: 0,
            base: VTVC0_BASE,
            endpoints: &[],
        });
        // The IGD keeps using its stolen memory: BGSM..TOLUD.
        let hb = self.host();
        let bgsm = u64::from(hb.bgsm.get() & 0xfff0_0000);
        let tolud = u64::from(hb.tolud.get() & 0xfff0_0000);
        let mut rmrrs = Vec::new();
        if igd && bgsm != 0 && tolud != 0 {
            rmrrs.push(Rmrr {
                segment: 0,
                base: bgsm,
                limit: tolud - 1,
                endpoints: &igd_endpoints,
            });
        }
        let width = (fstart_arch::x86::physical_address_bits() - 1) as u8;
        tables.push(dmar_with_interrupts(width, &units, &interrupts, &rmrrs));
        tables
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peg_prt_rotates_like_irqm() {
        let aml = peg_port("PEG1", 0x0001_0001, true, 1);
        // IRQM(2): INTA -> GSI 17, INTD -> GSI 16.
        assert!(aml.windows(5).any(|w| w == [0x0c, 17, 0, 0, 0]));
        assert!(aml.windows(5).any(|w| w == [0x0c, 16, 0, 0, 0]));
    }
}
