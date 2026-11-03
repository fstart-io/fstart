//! PCH ACPI: FADT/MADT facts (coreboot `fadt.c`, `lpc.c`) and the
//! namespace of `acpi/pch.asl` with the `_PRT` coreboot generates from the
//! default interrupt map (`intel_acpi_gen_def_acpi_pirq`).

extern crate alloc;

use super::*;
use crate::southbridge::acpi as fragments;
use crate::southbridge::pirq::{PciPin, PinRoute, Pirq, PirqRouting};
use alloc::vec::Vec;
use fstart_acpi::AcpiPath;
use fstart_acpi::fadt::{Flags, PmProfile};
use fstart_acpi::pci_root::adr;
use fstart_acpi::platform::{
    HpetConfig, IoApicConfig, IsoConfig, X86Config, X86Fadt, X86PlatformProvider,
};
use fstart_acpi_macros::acpi_dsl;

const LAPIC_BASE: u64 = 0xfee0_0000;
const HPET_ID: u32 = 0x8086_a201;
const SCI_IRQ: u8 = 9;
const APM_CNT: u32 = 0xb2;

static IOAPICS: [IoApicConfig; 1] = [IoApicConfig {
    id: 2,
    base: IOAPIC_BASE,
    gsi_base: 0,
}];

static ISOS: [IsoConfig; 2] = [
    // acpi_create_madt_ioapic_gsi0_default: IRQ0 -> GSI2, edge, high.
    IsoConfig {
        bus: 0,
        source: 0,
        gsi: 2,
        flags: 0x0005,
    },
    // SCI: IRQ9, level, high.
    IsoConfig {
        bus: 0,
        source: SCI_IRQ,
        gsi: SCI_IRQ as u32,
        flags: 0x000d,
    },
];

const fn route(slot: u8, pin: PciPin, line: Pirq) -> PinRoute {
    PinRoute::new(slot, pin, line)
}

/// `southbridge_configure_default_intmap` as `_PRT` routes; the CPU's
/// devices hardwire INTA..D to PIRQ A..D.
static DEFAULT_ROUTES: [PinRoute; 22] = [
    route(0x01, PciPin::A, Pirq::A),
    route(0x01, PciPin::B, Pirq::B),
    route(0x01, PciPin::C, Pirq::C),
    route(0x01, PciPin::D, Pirq::D),
    route(0x02, PciPin::A, Pirq::A),
    route(0x06, PciPin::A, Pirq::A),
    route(0x14, PciPin::A, Pirq::B),
    route(0x16, PciPin::A, Pirq::A),
    route(0x19, PciPin::A, Pirq::B),
    route(0x1a, PciPin::A, Pirq::D),
    route(0x1b, PciPin::A, Pirq::D),
    route(0x1c, PciPin::A, Pirq::A),
    route(0x1c, PciPin::B, Pirq::B),
    route(0x1c, PciPin::C, Pirq::C),
    route(0x1c, PciPin::D, Pirq::D),
    route(0x1d, PciPin::A, Pirq::C),
    route(0x1f, PciPin::A, Pirq::A),
    route(0x1f, PciPin::B, Pirq::A),
    route(0x1f, PciPin::C, Pirq::B),
    route(0x1f, PciPin::D, Pirq::B),
    // 0x06 and 0x02 only use INTA; slot 0x1e has no pin.
    route(0x06, PciPin::B, Pirq::B),
    route(0x06, PciPin::C, Pirq::C),
];

/// The routing the default interrupt map programs.
pub const DEFAULT_ROUTING: PirqRouting = PirqRouting {
    slot_pins: &[],
    pin_routes: &DEFAULT_ROUTES,
    bridge_routes: &[],
};

impl X86PlatformProvider for IntelBd82x6x {
    fn x86_platform_config(&self, online_cpus: u32) -> X86Config {
        X86Config {
            apic_ids: (0..online_cpus).collect(),
            lapic_base: LAPIC_BASE,
            lapic_nmi_lint: None,
            ioapics: IOAPICS.to_vec(),
            isos: ISOS.to_vec(),
            hpet: Some(HpetConfig {
                base: HPET_BASE,
                timer_block_id: HPET_ID,
                min_tick: 0x80,
            }),
            legacy_devices: true,
            sci_irq: SCI_IRQ,
            pmbase: self.config.pmbase,
            gpe0_offset: crate::southbridge::smi::ICH8_GPE0.sts_offset,
            gpe0_length: 16,
            acpi_smi: Some(fstart_core::acpi::AcpiSmiConfig {
                smi_cmd: APM_CNT,
                acpi_enable: crate::southbridge::smi::APM_CNT_ACPI_ENABLE,
                acpi_disable: crate::southbridge::smi::APM_CNT_ACPI_DISABLE,
            }),
            // coreboot bd82x6x `fadt.c`: PM2_CNT, no C2/C3 latencies (the
            // CPU's _CST is authoritative), sealed case, S4 RTC wake.
            fadt: X86Fadt {
                pm_profile: if self.config.laptop {
                    PmProfile::Mobile
                } else {
                    PmProfile::Desktop
                },
                flags: Flags::Wbinvd as u32
                    | Flags::ProcC1 as u32
                    | Flags::SlpButton as u32
                    | Flags::SealedCase as u32
                    | Flags::RtcS4 as u32
                    | Flags::UsePlatformClock as u32
                    | if self.config.docking_supported {
                        Flags::DckCap as u32
                    } else {
                        0
                    },
                p_lvl2_lat: 101,
                p_lvl3_lat: 1001,
                duty_offset: 0,
                duty_width: 0,
                pm2_cnt_offset: Some(0x50),
                ..X86Fadt::default()
            },
        }
    }
}

impl crate::namespace::SouthbridgeAcpi for IntelBd82x6x {
    fn dsdt_aml(&self, config: &Self::Config, pci_root: &AcpiPath) -> Vec<u8> {
        // S3 only once this image's training and stage caches persisted.
        // S1 is not advertised: the returning sleep-SMI path does not re-arm
        // interception, which the next S3 entry needs.
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
            DEFAULT_ROUTING
                .root_bus_routes()
                .map(|(slot, pin, gsi)| (slot, pin.index(), gsi)),
        ));
        body.extend(fragments::hda_node(dev::HDA));
        // Root ports by function number after coalescing (coreboot names
        // RP01..RP08 by the devicetree function).
        for func in 0..8u8 {
            let devfn = fstart_pci::devfn(dev::PCIE, func);
            // Without ECAM (host builds) every port is described.
            let present = EcamDevice::from_devfn(0, devfn)
                .try_read16(0)
                .is_none_or(|vendor| vendor != 0xffff);
            if present {
                body.extend(fragments::pcie_root_port(
                    &alloc::format!("RP0{}", func + 1),
                    devfn,
                    func + 1,
                    self.pcie_hotplug[usize::from(func)],
                ));
            }
        }
        body.extend(fragments::ehci_rmh_node("EHC1", dev::EHCI1, 8));
        body.extend(fragments::ehci_rmh_node("EHC2", dev::EHCI2, 6));
        if self.pch_type_or_cpt() == PCH_TYPE_PPT {
            let xhci_adr = adr(dev::XHCI);
            body.extend(Vec::<u8>::from(acpi_dsl! {
                Device("XHC_") {
                    Name("_ADR", #{dword xhci_adr});
                    Name("_PRW", Package(13u32, 4u32));
                    Method("_S3D", 0, NotSerialized) { Return(2u32); }
                    Method("_S4D", 0, NotSerialized) { Return(2u32); }
                }
            }));
        }
        let sata_adr = adr(dev::SATA1);
        body.extend(Vec::<u8>::from(acpi_dsl! {
            Device("SATA") { Name("_ADR", #{dword sata_adr}); }
        }));
        body.extend(fragments::smbus_node(dev::SMBUS));
        let lpc_adr = adr(dev::LPC);
        let mut lpcb: Vec<u8> = acpi_dsl! {
            Name("_ADR", #{dword lpc_adr});
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
            }
        }
        .into();
        lpcb.extend(fragments::legacy_isa_children(
            config.pmbase,
            config.gpiobase,
        ));
        lpcb.extend(fragments::pci_irq_links(&[11; 8]));
        body.extend(
            fstart_acpi::aml_linker::device_vec(
                <Self as crate::IntelSouthbridgeDriver>::ACPI_LPC_NAME,
                &lpcb,
            )
            .expect("LPCB AML"),
        );
        aml.extend(fstart_acpi::aml_linker::scope_vec(pci_root.as_str(), &body).expect("PCH AML"));
        aml.extend(fragments::pcie_hotplug_aml(&self.pcie_hotplug, pci_root));
        aml
    }

    fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
        Vec::new()
    }
}

impl IntelBd82x6x {
    /// The PCH type, assuming Cougar Point off target.
    fn pch_type_or_cpt(&self) -> u8 {
        if cfg!(target_os = "none") {
            self.pch_type()
        } else {
            PCH_TYPE_CPT
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_routing_matches_the_programmed_intmap() {
        // D31IR = route(A, A, B, B), D29IR = C, D28IR linear.
        assert_eq!(DEFAULT_ROUTING.ir_value(0x1f), 0x1100);
        assert_eq!(DEFAULT_ROUTING.ir_value(0x1d), 0x0002);
        assert_eq!(DEFAULT_ROUTING.ir_value(0x1c), 0x3210);
        assert_eq!(DEFAULT_ROUTING.ir_value(0x1b), 0x0003);
        assert_eq!(DEFAULT_ROUTING.gsi(0x1a, PciPin::A), 19);
    }
}
