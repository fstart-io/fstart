//! 5 Series PCH ACPI: coreboot `ibexpeak/fadt.c`, `lpc.c`'s generated
//! `_PRT` and hot-plug methods, and `bd82x6x/acpi/pch.asl` with the common
//! ICH fragments.
//!
//! Not emitted: the xHCI node and `_OSC` branch (no xHCI on this PCH), the
//! GNVS-backed `GLVL`/`SLVL` GPIO methods (nothing references them) and the
//! PIC-mode `_PRT` alternatives (the OS runs the I/O APIC).

extern crate alloc;

use super::{IntelIbexPeak, PchFunction};
use crate::{IntelSouthbridgeDriver, namespace::SouthbridgeAcpi};
use alloc::vec::Vec;
use fstart_acpi::AcpiPath;
use fstart_acpi::fadt::{Flags, PmProfile};
use fstart_acpi::platform::{
    HpetConfig, IoApicConfig, IsoConfig, X86Config, X86Fadt, X86PlatformProvider,
};
use fstart_acpi_macros::acpi_dsl;
use fstart_pci::ecam::EcamDevice;

const LAPIC_BASE: u64 = 0xfee0_0000;
const HPET_ID: u32 = 0x8086_a201;
/// coreboot `HPET_MIN_TICKS` for this PCH.
const HPET_MIN_TICKS: u16 = 0x80;
const SCI_IRQ: u8 = 9;
const APM_CNT: u32 = 0xb2;

static IOAPICS: [IoApicConfig; 1] = [IoApicConfig {
    id: 0,
    base: super::IOAPIC_BASE as u64,
    gsi_base: 0,
}];

static ISOS: [IsoConfig; 2] = [
    // IRQ0 -> GSI2, edge, active high.
    IsoConfig {
        bus: 0,
        source: 0,
        gsi: 2,
        flags: 0x0005,
    },
    // SCI: IRQ9, level, active high.
    IsoConfig {
        bus: 0,
        source: SCI_IRQ,
        gsi: SCI_IRQ as u32,
        flags: 0x000d,
    },
];

impl X86PlatformProvider for IntelIbexPeak {
    /// coreboot `acpi_fill_fadt()` and the common MADT/HPET writers.
    fn x86_platform_config(&self, online_cpus: u32) -> X86Config {
        let mut flags = Flags::Wbinvd as u32
            | Flags::ProcC1 as u32
            | Flags::SlpButton as u32
            | Flags::RtcS4 as u32
            | Flags::UsePlatformClock as u32;
        if self.config.docking_supported {
            flags |= Flags::DckCap as u32;
        }
        X86Config {
            apic_ids: (0..online_cpus).collect(),
            lapic_base: LAPIC_BASE,
            lapic_nmi_lint: None,
            ioapics: IOAPICS.to_vec(),
            isos: ISOS.to_vec(),
            hpet: Some(HpetConfig {
                base: super::HPET_BASE as u64,
                timer_block_id: HPET_ID,
                min_tick: HPET_MIN_TICKS,
            }),
            legacy_devices: true,
            sci_irq: SCI_IRQ,
            pmbase: self.config.pmbase,
            gpe0_offset: super::regs::pm::GPE0_STS,
            gpe0_length: 16,
            acpi_smi: Some(fstart_core::acpi::AcpiSmiConfig {
                smi_cmd: APM_CNT,
                acpi_enable: crate::southbridge::smi::APM_CNT_ACPI_ENABLE,
                acpi_disable: crate::southbridge::smi::APM_CNT_ACPI_DISABLE,
            }),
            fadt: X86Fadt {
                pm_profile: if self.config.laptop {
                    PmProfile::Mobile
                } else {
                    PmProfile::Desktop
                },
                flags,
                pm2_cnt_offset: Some(0x50),
                ..X86Fadt::default()
            },
        }
    }
}

/// coreboot `map_pirq()`: PCH devices route through their DxxIR register,
/// everything else (CPU-integrated functions) maps INTx 1:1 to PIRQ A..D.
fn dxx_ir(rcba: &super::regs::RcbaRegs, slot: u8) -> Option<u16> {
    use tock_registers::interfaces::Readable;
    Some(match slot {
        20 => rcba.d20ir.get(),
        22 => rcba.d22ir.get(),
        25 => rcba.d25ir.get(),
        26 => rcba.d26ir.get(),
        27 => rcba.d27ir.get(),
        28 => rcba.d28ir.get(),
        29 => rcba.d29ir.get(),
        30 => rcba.d30ir.get(),
        31 => rcba.d31ir.get(),
        _ => return None,
    })
}

impl IntelIbexPeak {
    /// coreboot `intel_acpi_gen_def_acpi_pirq()`: one APIC route per
    /// (slot, pin) of the bus-0 functions that use an interrupt.
    fn root_bus_routes(&self) -> Vec<(u8, u8, u8)> {
        let rcba = self.rcba();
        let mut routes: Vec<(u8, u8, u8)> = Vec::new();
        for slot in 0..32u8 {
            for func in 0..8u8 {
                let device = EcamDevice::new(0, slot, func);
                // Without ECAM (host builds) there is nothing to route.
                let Some(vendor) = device.try_read16(0) else {
                    return routes;
                };
                if vendor == 0xffff {
                    continue;
                }
                let pin = device.read8(0x3d);
                if !(1..=4).contains(&pin) || routes.iter().any(|r| r.0 == slot && r.1 == pin - 1) {
                    continue;
                }
                let pirq = match dxx_ir(rcba, slot) {
                    Some(ir) => ((ir >> (4 * u16::from(pin - 1))) & 7) as u8,
                    None if (19..=31).contains(&slot) && slot != 24 && slot != 21 && slot != 23 => {
                        continue;
                    }
                    None => pin - 1,
                };
                routes.push((slot, pin - 1, 16 + pirq));
            }
        }
        routes
    }
}

/// AML `Sleep(ms)` (`SleepOp ByteConst`), which the builders lack.
struct Sleep(u8);
impl fstart_acpi::Aml for Sleep {
    fn to_aml_bytes(&self, sink: &mut dyn fstart_acpi::AmlSink) {
        sink.vec(&[0x5b, 0x22, 0x0a, self.0]);
    }
}

/// coreboot `intel_acpi_pcie_hotplug_generator()`: a removable `SLOT`
/// under each hot-plug root port and the `_L01` GPE that notifies it.
fn pcie_hotplug_aml(hotplug: &[bool; 8], pci_root: &AcpiPath) -> Vec<u8> {
    use fstart_acpi::Aml;
    use fstart_acpi::aml::{If, Method, Notify, Path, Scope, Store};
    let mut aml = Vec::new();
    let ports: Vec<usize> = (0..8).filter(|port| hotplug[*port]).collect();
    if ports.is_empty() {
        return aml;
    }
    let slot: Vec<u8> = acpi_dsl! {
        Device("SLOT") {
            Name("_ADR", 0u32);
            Method("_RMV", 0, NotSerialized) { Return(1u32); }
        }
    }
    .into();
    for port in &ports {
        let scope = alloc::format!("{}.RP0{}", pci_root.as_str(), port + 1);
        aml.extend(
            fstart_acpi::aml_linker::scope_vec(&scope, &slot).expect("hot-plug SLOT emission"),
        );
    }
    let roots: Vec<Path> = ports
        .iter()
        .map(|port| Path::new(&alloc::format!("{}.RP0{}", pci_root.as_str(), port + 1)))
        .collect();
    let hpcs: Vec<Path> = ports
        .iter()
        .map(|port| {
            Path::new(&alloc::format!(
                "{}.RP0{}.HPCS",
                pci_root.as_str(),
                port + 1
            ))
        })
        .collect();
    let pdc: Vec<Path> = ports
        .iter()
        .map(|port| {
            Path::new(&alloc::format!(
                "{}.RP0{}.PDC_",
                pci_root.as_str(),
                port + 1
            ))
        })
        .collect();
    let one = fstart_acpi::aml::ONE;
    let zero = fstart_acpi::aml::ZERO;
    let sleep = Sleep(100);
    let clear_hpcs: Vec<Store> = hpcs.iter().map(|path| Store::new(path, &one)).collect();
    let clear_pdc: Vec<Store> = pdc.iter().map(|path| Store::new(path, &one)).collect();
    let notify: Vec<Notify> = roots.iter().map(|path| Notify::new(path, &zero)).collect();
    let inner: Vec<If> = (0..ports.len())
        .map(|i| If::new(&pdc[i], alloc::vec![&clear_pdc[i] as &dyn Aml, &notify[i]]))
        .collect();
    let checks: Vec<If> = (0..ports.len())
        .map(|i| {
            If::new(
                &hpcs[i],
                alloc::vec![&sleep as &dyn Aml, &clear_hpcs[i], &inner[i]],
            )
        })
        .collect();
    let method = Method::new(
        "_L01".into(),
        0,
        false,
        checks.iter().map(|check| check as &dyn Aml).collect(),
    );
    Scope::new("\\_GPE".into(), alloc::vec![&method]).to_aml_bytes(&mut aml);
    aml
}

/// An EHCI controller behind its rate-matching hub (coreboot
/// `bd82x6x/acpi/usb.asl`): `HUB7.URMH.PRTn`, the hub itself not visible.
fn ehci_rmh_node(name: &str, adr: u32, ports: u8) -> Vec<u8> {
    let mut rmh: Vec<u8> = acpi_dsl! {
        Name("_ADR", 1u32);
        Name("_UPC", Package(0xFFu32, 0xFFu32, 0u32, 0u32));
        // GPLD(0): revision 2, not user visible.
        Name("_PLD", Package(Buffer(
            0x02u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8,
            0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8, 0u8)));
    }
    .into();
    for port in 0..ports {
        let adr = u32::from(port) + 1;
        let child: Vec<u8> = acpi_dsl! { Name("_ADR", #{dword adr}); }.into();
        rmh.extend(
            fstart_acpi::aml_linker::device_vec(&alloc::format!("PRT{port}"), &child)
                .expect("EHCI port emission"),
        );
    }
    let mut hub: Vec<u8> = acpi_dsl! { Name("_ADR", 0u32); }.into();
    hub.extend(fstart_acpi::aml_linker::device_vec("URMH", &rmh).expect("URMH emission"));
    let mut body: Vec<u8> = acpi_dsl! {
        Name("_ADR", #{dword adr});
        Name("_PRW", Package(13u32, 4u32));
        Method("_S3D", 0, NotSerialized) { Return(2u32); }
        Method("_S4D", 0, NotSerialized) { Return(2u32); }
    }
    .into();
    body.extend(fstart_acpi::aml_linker::device_vec("HUB7", &hub).expect("HUB7 emission"));
    fstart_acpi::aml_linker::device_vec(name, &body).expect("EHCI emission")
}

impl SouthbridgeAcpi for IntelIbexPeak {
    fn dsdt_aml(&self, config: &Self::Config, pci_root: &AcpiPath) -> Vec<u8> {
        // S3 only once this image's training and stage caches persisted.
        // Do not advertise S1: the returning sleep-SMI path does not yet
        // re-arm interception, which is required for subsequent S3 entry.
        let mut aml: Vec<u8> = acpi_dsl! {
            Scope("\\") {
                Name("_S0_", Package(0u32, 0u32, 0u32, 0u32));
                Name("_S4_", Package(6u32, 4u32, 0u32, 0u32));
                Name("_S5_", Package(7u32, 0u32, 0u32, 0u32));
            }
        }
        .into();
        if self.s3_enabled {
            aml.extend_from_slice(&acpi_dsl! { Scope("\\") {
                Name("_S3_", Package(5u32, 0u32, 0u32, 0u32));
            } });
        }

        let rcba = config.rcba as u32;
        let mut body: Vec<u8> = acpi_dsl! {
            // pch.asl: the Root Complex fields OSPM reads.
            OperationRegion("RCRB", SystemMemory, #{dword rcba}, 0x4000u32);
            Field("RCRB", DWordAcc, Lock, Preserve) {
                Offset(0x3404),
                HPAS, 2,
                , 5,
                HPTE, 1,
                Offset(0x3418),
                , 1,
                PCID, 1,
                SA1D, 1,
                SMBD, 1,
                HDAD, 1,
                , 8,
                EH2D, 1,
                LPBD, 1,
                EH1D, 1,
                RP1D, 1,
                RP2D, 1,
                RP3D, 1,
                RP4D, 1,
                RP5D, 1,
                RP6D, 1,
                RP7D, 1,
                RP8D, 1,
                TTRD, 1,
                SA2D, 1,
                Offset(0x3428),
                BDFD, 1,
                ME1D, 1,
                ME2D, 1,
                IDRD, 1,
                KTCT, 1,
            }
            Method("_OSC", 4, NotSerialized) {
                If (Arg0 == ToUUID("33DB4D5B-1FF7-401C-9657-7441C03DD766")) {
                    // Let the OS control every PCIe feature.
                    Return(Arg3);
                }
                CreateDwordField(Arg3, 0u32, "CDW1");
                CDW1 = CDW1 | 4u32;
                Return(Arg3);
            }
        }
        .into();
        body.extend(fstart_acpi::pirq::prt_name_aml(
            self.root_bus_routes().into_iter(),
        ));
        // HDEF: wakes on GPE 13 from S4 (bd82x6x audio.asl).
        body.extend_from_slice(&acpi_dsl! {
            Device("HDEF") {
                Name("_ADR", 0x001B0000u32);
                Name("_PRW", Package(13u32, 4u32));
            }
        });
        for port in 0..8u8 {
            if !config.enabled(PchFunction::PcieRootPort(port)) {
                continue;
            }
            body.extend(crate::southbridge::acpi::pcie_root_port(
                &alloc::format!("RP0{}", port + 1),
                0x001c_0000 | u32::from(port),
                port + 1,
                true,
            ));
        }
        body.extend(ehci_rmh_node("EHC1", 0x001d_0000, 8));
        body.extend(ehci_rmh_node("EHC2", 0x001a_0000, 6));
        body.extend_from_slice(&acpi_dsl! {
            Device("SATA") { Name("_ADR", 0x001F0002u32); }
            Device("SBUS") { Name("_ADR", 0x001F0003u32); }
        });

        // lpc.asl.
        let mut lpcb: Vec<u8> = acpi_dsl! {
            Name("_ADR", 0x001F0000u32);
            OperationRegion("LPC0", PciConfig, 0x00u32, 0x100u32);
            Field("LPC0", AnyAcc, NoLock, Preserve) {
                Offset(0x40),
                PMBS, 16,
                Offset(0x60),
                PRTA, 8, PRTB, 8, PRTC, 8, PRTD, 8,
                Offset(0x68),
                PRTE, 8, PRTF, 8, PRTG, 8, PRTH, 8,
                Offset(0x80),
                IOD0, 8, IOD1, 8,
                Offset(0xB8),
                GR00, 2, GR01, 2, GR02, 2, GR03, 2, GR04, 2, GR05, 2, GR06, 2, GR07, 2,
                GR08, 2, GR09, 2, GR10, 2, GR11, 2, GR12, 2, GR13, 2, GR14, 2, GR15, 2,
            }
        }
        .into();
        lpcb.extend(crate::southbridge::acpi::legacy_isa_children(
            config.pmbase,
            config.gpiobase,
        ));
        lpcb.extend(crate::southbridge::acpi::pci_irq_links(&[11; 8]));
        body.extend(
            fstart_acpi::aml_linker::device_vec(Self::ACPI_LPC_NAME, &lpcb).expect("LPCB emission"),
        );

        aml.extend(
            fstart_acpi::aml_linker::scope_vec(pci_root.as_str(), &body)
                .expect("PCH scope emission"),
        );
        aml.extend(pcie_hotplug_aml(&config.pcie_hotplug, pci_root));
        aml
    }

    fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ROOT: AcpiPath = AcpiPath::SB.child("PCI0");

    fn count(aml: &[u8], name: &[u8]) -> usize {
        aml.windows(name.len()).filter(|w| *w == name).count()
    }

    #[test]
    fn sleep_states_omit_s1_and_gate_s3_on_persistence() {
        use crate::{IntelSouthbridgeDriver, ibexpeak::IntelIbexPeakConfig};
        static CONFIG: IntelIbexPeakConfig = IntelIbexPeakConfig::new();
        let mut pch = IntelIbexPeak::new_from_config(&CONFIG).unwrap();
        for enabled in [false, true] {
            pch.set_s3_enabled(enabled);
            let aml = pch.dsdt_aml(&CONFIG, &ROOT);
            assert_eq!(count(&aml, b"_S1_"), 0);
            assert_eq!(count(&aml, b"_S3_"), usize::from(enabled));
            assert_eq!(count(&aml, b"_S4_"), 1);
            assert_eq!(count(&aml, b"_S5_"), 1);
        }
    }

    #[test]
    fn hotplug_adds_a_removable_slot_and_the_gpe() {
        let aml = pcie_hotplug_aml(
            &[false, false, false, true, false, false, false, false],
            &ROOT,
        );
        assert_eq!(count(&aml, b"SLOT"), 1);
        assert_eq!(count(&aml, b"_L01"), 1);
        // The SLOT scope, the notified port, two HPCS and two PDC_ paths.
        assert_eq!(count(&aml, b"RP04"), 6);
        assert!(pcie_hotplug_aml(&[false; 8], &ROOT).is_empty());
    }

    #[test]
    fn ehci_hides_the_rate_matching_hub() {
        let aml = ehci_rmh_node("EHC2", 0x001a_0000, 6);
        assert_eq!(count(&aml, b"URMH"), 1);
        assert_eq!(count(&aml, b"PRT5"), 1);
        assert_eq!(count(&aml, b"PRT6"), 0);
    }
}
