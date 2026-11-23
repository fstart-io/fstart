//! Haswell PCI root, MCFG, DMAR, and package C-state / P-state ACPI descriptions.

use super::{ECAM, Haswell, HaswellConfig};
use alloc::vec::Vec;
use fstart_acpi::{
    aml_linker::{device_vec, scope_vec},
    device::AcpiDevice,
    gas::{AccessSize, AddressSpace, GAS},
    processor::PowerState,
};
use fstart_acpi_macros::acpi_dsl;

impl AcpiDevice for Haswell {
    type Config = HaswellConfig;

    fn dsdt_aml(&self, c: &HaswellConfig) -> Vec<u8> {
        let mut pci: Vec<u8> = acpi_dsl! {
            Name("_HID", EisaId("PNP0A08"));
            Name("_CID", EisaId("PNP0A03"));
            Name("_SEG", 0u32);
            Name("_BBN", 0u32);
            Device("MCHC") { Name("_ADR", 0u32); }
        }
        .into();
        if c.igd_enabled {
            #[cfg(target_os = "none")]
            let assigned = fstart_pci::EcamDevice::new(0, 2, 0)
                .memory_bar(0x10)
                .is_some();
            #[cfg(not(target_os = "none"))]
            let assigned = true;
            if assigned {
                pci.extend(crate::igd::gfx0_acpi_node_configurable::<Self>(
                    None,
                    crate::igd::GfxBacklight::Pch,
                    Some(c.igd.brightness_levels),
                ));
            }
            if c.minihd {
                pci.extend_from_slice(&acpi_dsl! {
                    Device("HDAU") { Name("_ADR", 0x30000u32); }
                });
            }
        }
        if !c.memory.ult {
            for (i, name) in ["PEGP", "PEG1", "PEG2"].into_iter().enumerate() {
                if c.peg[i].is_none() {
                    continue;
                }
                let adr = 0x10000 + i as u32;
                let bit = 3 - i as u32;
                let node: Vec<u8> = acpi_dsl! {
                    Name("_ADR", #{dword adr});
                    Method("_STA", 0, NotSerialized) {
                        Return(((#{const "\\_SB_.PCI0.MCHC.DEVN"} >> #{byte bit}) & 1u32) * 15u32);
                    }
                    Device("DEV0") { Name("_ADR", 0u32); }
                }
                .into();
                pci.extend(device_vec(name, &node).expect("PEG"));
            }
        }
        let lp = c.memory.ult;
        let states = [
            (if lp { 1 } else { 0 }, 0, 1000),
            (0x10, 67, 900),
            (
                if lp { 0x33 } else { 0x21 },
                148,
                if lp { 700 } else { 800 },
            ),
        ];
        let cstates = core::array::from_fn::<_, 3, _>(|i| PowerState {
            register: GAS::new(
                AddressSpace::FunctionalFixedHardware,
                1,
                2,
                AccessSize::ByteAccess,
                states[i].0,
            ),
            kind: i as u8 + 1,
            latency_us: states[i].1,
            milliwatts: states[i].2,
        });
        #[cfg(target_os = "none")]
        let count = fstart_arch::x86::mp::online_cpus().max(1);
        #[cfg(not(target_os = "none"))]
        let count = 1u16;
        use crate::cpu::ratio_aml::{RatioLimits, package, ratio_states};
        #[cfg(target_os = "none")]
        let (limits, tdp) = {
            use fstart_arch::x86::cpu::msr::{Msr, MsrAccess, NativeMsr};
            use tock_registers::interfaces::Readable;
            // SAFETY: running on validated Haswell bare-metal CPU.
            let cpu = unsafe { NativeMsr::new() };
            let mut l = RatioLimits::current();
            if (cpu.reg(Msr::<()>::new(0xce)).get() >> 33) & 3 != 0 {
                l.max = (cpu.reg(Msr::<()>::new(0x648)).get() & 255) as u32;
            }
            let units = cpu.reg(Msr::<()>::new(0x606)).get() & 15;
            let tdp = ((cpu.reg(Msr::<()>::new(0x614)).get() & 0x7fff) >> units) * 1000;
            (l, tdp as u32)
        };
        #[cfg(not(target_os = "none"))]
        let (limits, tdp) = (
            RatioLimits {
                min: 8,
                max: 16,
                turbo: None,
                sw_coordination: false,
                mwait_substates: 0,
                fine_throttling: false,
            },
            15000,
        );
        let states = ratio_states(&limits, |n| 100 * n, tdp, 8, 2, 8);
        let lowest = states.len().saturating_sub(1) as u32;
        let mch = u64::from(super::system_agent::MCH) + 0x5000;
        let control: Vec<u8> = acpi_dsl! {
            OperationRegion("MCFG", PciConfig, 0u32, 0x100u32);
            Field("MCFG", DWordAcc, NoLock, Preserve) { Offset(0x54), DEVN, 32, }
            OperationRegion("MCHB", SystemMemory, #{dword mch}, 0x1000u32);
            Field("MCHB", DWordAcc, Lock, Preserve) {
                Offset(0x938), PUNI, 4, // PACKAGE_POWER_SKU_UNIT
                Offset(0x95d), LFMR, 8, // PLATFORM_INFO[47:40]
                Offset(0x9a0), PL1V, 15, , 1, PL1C, 1, // PL1 value and clamp
                Offset(0xf54), TARS, 8, // TURBO_ACTIVATION_RATIO
            }
            Name("THEN", 0u32);
            Name("SPL1", 0u32);
            Name("STAR", 0u32);
            Name("SCLP", 0u32);
            Method("CTLE", 1, Serialized) { Return(CTDP(1u32, Arg0)); }
            Method("CTLD", 0, Serialized) { Return(CTDP(0u32, 0u32)); }
            Method("CTDP", 2, Serialized) {
                If (Arg0) {
                    // Requested watts in RAPL power units. Capture the original
                    // limit only on the first request, even if the EC repeats it.
                    Local0 = Arg1 << PUNI;
                    If ((Local0 > 0x7fffu32) || (Local0 == 0u32)) { Return(0u32); }
                    If (THEN == 0u32) {
                        SPL1 = PL1V;
                        STAR = TARS;
                        SCLP = PL1C;
                        THEN = 1u32;
                    }
                    Store(#{byte lowest}, #{const "\\_SB_.PPCM"});
                    CNOT(0x80u32);
                    TARS = LFMR - 1u32;
                    PL1V = Local0;
                    PL1C = 1u32;
                } Else {
                    If (THEN) {
                        PL1C = SCLP;
                        PL1V = SPL1;
                        TARS = STAR;
                        THEN = 0u32;
                        Store(0u32, #{const "\\_SB_.PPCM"});
                        CNOT(0x80u32);
                    }
                }
                Return(1u32);
            }
        }
        .into();
        pci.extend(scope_vec("MCHC", &control).expect("MCH power policy"));
        let mut sb = device_vec("PCI0", &pci).expect("PCI0");
        sb.extend_from_slice(&acpi_dsl! { Name("PPCM", 0u32); });
        sb.extend(package(
            usize::from(count),
            &limits,
            &states,
            &cstates,
            true,
        ));
        let mut aml = scope_vec("\\_SB_", &sb).expect("system bus");
        aml.extend_from_slice(&acpi_dsl! {
            Scope("\\") {
                Method("PPKG", 0, NotSerialized) { Return(#{const "\\_SB_.PPKG"}); }
                Name("TLVL", 0u32);
                Method("PNOT", 0, NotSerialized) { #{const "\\_SB_.CNOT"}(0x81u32); }
                Method("PPCN", 0, NotSerialized) { #{const "\\_SB_.CNOT"}(0x80u32); }
                Method("TNOT", 0, NotSerialized) { #{const "\\_SB_.CNOT"}(0x82u32); }
            }
        });
        aml
    }

    fn extra_tables(&self, _: &Self::Config) -> Vec<Vec<u8>> {
        #[allow(unused_mut)]
        let mut tables = alloc::vec![fstart_acpi::build_mcfg(&[fstart_pci::PciRootInfo {
            segment: 0,
            ecam_base: ECAM,
            bus_start: 0,
            bus_end: 63,
        }])];
        #[cfg(target_os = "none")]
        {
            let m = super::system_agent::mch();
            let host = fstart_pci::ecam::EcamDevice::new(0, 0, 0);
            if super::system_agent::vtd_capable_from_capid0_a(host.read32(0xe4)) {
                let bounds = (
                    host.read32(0xb4) & 0xfff0_0000,
                    host.read32(0xbc) & 0xfff0_0000,
                );
                if let Some(table) = remapping(
                    (u64::from(m.read32(0x5404)) << 32) | u64::from(m.read32(0x5400)),
                    (u64::from(m.read32(0x5414)) << 32) | u64::from(m.read32(0x5410)),
                    // SAFETY: IOAPIC is mapped at standard x86 base `0xfec0_0000`.
                    unsafe { fstart_arch::x86::ioapic::IoApic::new(0xfec0_0000) }.id(),
                    host.read32(0x54) & 16 != 0,
                    bounds,
                    (fstart_arch::x86::cpuid(0x8000_0008).0 as u8) - 1,
                ) {
                    tables.push(table);
                }
            }
        }
        tables
    }
}

#[cfg(any(test, target_os = "none"))]
fn remapping(
    gfx: u64,
    all: u64,
    ioapic: u8,
    igd: bool,
    (gtt, tolud): (u32, u32),
    width: u8,
) -> Option<Vec<u8>> {
    use fstart_acpi::dmar::{Drhd, InterruptScope, InterruptSource, Rmrr, dmar_with_interrupts};
    let valid = |bar: u64| bar >> 32 == 0 && bar & 1 != 0 && bar & !0xfff != 0;
    let base = all & !0xfff;
    let intr = core::array::from_fn::<_, 9, _>(|i| InterruptScope {
        unit_base: base,
        source: if i == 0 {
            InterruptSource::IoApic(ioapic)
        } else {
            InterruptSource::Hpet(0)
        },
        bdf: if i == 0 {
            (250, 31, 0)
        } else {
            (250, 15, (i - 1) as u8)
        },
    });
    let mut units = Vec::new();
    let mut regions = Vec::new();
    if igd && valid(gfx) {
        units.push(Drhd {
            include_pci_all: false,
            segment: 0,
            base: gfx & !0xfff,
            endpoints: &[(0, 2, 0)],
        });
    }
    if !units.is_empty() && gtt != 0 && gtt < tolud {
        regions.push(Rmrr {
            segment: 0,
            base: gtt.into(),
            limit: u64::from(tolud) - 1,
            endpoints: &[(0, 2, 0)],
        });
    }
    if !valid(all) {
        return (!units.is_empty()).then(|| dmar_with_interrupts(width, &units, &[], &regions));
    }
    units.push(Drhd {
        include_pci_all: true,
        segment: 0,
        base,
        endpoints: &[],
    });
    Some(dmar_with_interrupts(width, &units, &intr, &regions))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntelNorthbridgeDriver;

    #[test]
    fn dmar_interrupts_and_stolen_memory() {
        let gfx = 0xfed9_0001;
        let all = 0xfed9_1001;
        assert!(remapping(gfx | (1 << 32), all | (1 << 32), 7, true, (1, 2), 38).is_none());
        assert_eq!(
            remapping(gfx, all | (1 << 32), 7, true, (2, 2), 38)
                .unwrap()
                .len(),
            48 + 24
        );
        assert_eq!(
            remapping(gfx | (1 << 32), all, 7, true, (1, 2), 38)
                .unwrap()
                .len(),
            48 + 16 + 9 * 8
        );
        let d = remapping(gfx, all, 7, true, (0x8000_0000, 0x8420_0000), 38).unwrap();
        assert_eq!(d[37], 1);
        assert_eq!(d.len(), 48 + 24 + 16 + 9 * 8 + 32);
        assert_eq!(d.iter().fold(0u8, |s, v| s.wrapping_add(*v)), 0);
        assert_eq!(&d[d.len() - 16..d.len() - 8], &0x841f_ffffu64.to_le_bytes());
    }

    #[test]
    fn pdrc_gates_vtd_window() {
        static CONFIG: HaswellConfig = HaswellConfig::new(true);
        let nb = Haswell::new_from_config(&CONFIG).unwrap();
        let with_vtd =
            crate::namespace::pci_root_resources::<Haswell>(&nb.pci_root_for(true).unwrap())
                .unwrap();
        let without_vtd =
            crate::namespace::pci_root_resources::<Haswell>(&nb.pci_root_for(false).unwrap())
                .unwrap();
        let needle = 0xfed9_0000u32.to_le_bytes();
        assert!(with_vtd.windows(4).any(|w| w == needle));
        assert!(!without_vtd.windows(4).any(|w| w == needle));
    }
}
