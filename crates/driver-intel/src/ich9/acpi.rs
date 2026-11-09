//! ICH9 ACPI tables and device namespace, derived from the driver's policy.

extern crate alloc;

use super::*;
use crate::IntelSouthbridgeDriver;
use crate::namespace::SouthbridgeAcpi;
use crate::southbridge::acpi as acpi_fragments;
use crate::southbridge::smi::{APM_CNT, APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE};
use alloc::vec::Vec;
use fstart_acpi::AcpiPath;
use fstart_acpi::fadt::{Flags, PmProfile};
use fstart_acpi::platform::{
    HpetConfig, IoApicConfig, IsoConfig, X86Config, X86Fadt, X86PlatformProvider,
};
use fstart_acpi_macros::acpi_dsl;

const LAPIC_BASE: u64 = 0xFEE0_0000;
const IOAPIC_BASE: u64 = 0xFEC0_0000;
const HPET_BASE: u64 = 0xFED0_0000;
const HPET_ID: u32 = 0x8086_A201;
const SCI_IRQ: u8 = 9;
const PMBASE: u16 = 0x0500;

static IOAPICS: [IoApicConfig; 1] = [IoApicConfig {
    id: 0,
    base: IOAPIC_BASE,
    gsi_base: 0,
}];

/// Root-port downstream INTx swizzling follows the configured D28IR,
/// not a fixed A-D to GSI16-19 assumption.
fn root_port_aml(
    name: &str,
    adr: u32,
    port: u8,
    hotplug: bool,
    routing: &crate::southbridge::pirq::PirqRouting,
) -> Vec<u8> {
    use crate::southbridge::pirq::PciPin;
    let mut body: Vec<u8> = acpi_dsl! { Name("_ADR", #{dword adr}); }.into();
    if hotplug {
        let region: Vec<u8> = acpi_dsl! {
            OperationRegion("RPCS", PciConfig, 0x00u32, 0xFFu32);
            Field("RPCS", AnyAcc, NoLock, Preserve) {
                Offset(0x4C),
                , 24,
                RPPN, 8,
                Offset(0x5A),
                , 3,
                PDC_, 1,
                Offset(0xDF),
                , 6,
                HPCS, 1,
            }
        }
        .into();
        body.extend_from_slice(&region);
    }
    let gsi = |pin: u8| {
        let rotated = (port - 1 + pin) % 4;
        routing.gsi(0x1c, PciPin::from_index(rotated).expect("INTx pin 0..3")) as u32
    };
    let (a, b, c, d) = (gsi(0), gsi(1), gsi(2), gsi(3));
    let prt: Vec<u8> = acpi_dsl! {
        Name("_PRT", Package(
            Package(0x0000FFFFu32, 0u32, 0u32, #{dword a}),
            Package(0x0000FFFFu32, 1u32, 0u32, #{dword b}),
            Package(0x0000FFFFu32, 2u32, 0u32, #{dword c}),
            Package(0x0000FFFFu32, 3u32, 0u32, #{dword d})
        ));
    }
    .into();
    body.extend_from_slice(&prt);
    fstart_acpi::aml_linker::device_vec(name, &body).expect("ICH9 PCIe root port emission")
}

static ISOS: [IsoConfig; 2] = [
    // coreboot acpi_create_madt_ioapic_gsi0_default(): ISA IRQ0 -> GSI2,
    // edge-triggered, active-high.
    IsoConfig {
        bus: 0,
        source: 0,
        gsi: 2,
        flags: 0x0005,
    },
    // coreboot southbridge/intel/common/pmbase.c ioapic_get_sci_pin():
    // SCI IRQ9/GSI9, level-triggered, active-high.
    IsoConfig {
        bus: 0,
        source: SCI_IRQ,
        gsi: SCI_IRQ as u32,
        flags: 0x000D,
    },
];

impl X86PlatformProvider for IntelIch9 {
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
                min_tick: 0,
            }),
            legacy_devices: true,
            sci_irq: SCI_IRQ,
            pmbase: PMBASE,
            gpe0_offset: GPE0_STS_ICH9,
            gpe0_length: 16,
            // The SMM image handles the shared APM commands.
            acpi_smi: Some(fstart_core::acpi::AcpiSmiConfig {
                smi_cmd: APM_CNT as u32,
                acpi_enable: APM_CNT_ACPI_ENABLE,
                acpi_disable: APM_CNT_ACPI_DISABLE,
            }),
            // coreboot i82801ix/fadt.c: PM2_CNT, C3 latency 0x39, a
            // 3-bit throttling duty field, docking, no C2 on MP.
            fadt: X86Fadt {
                pm_profile: if self.config.laptop {
                    PmProfile::Mobile
                } else {
                    PmProfile::Desktop
                },
                flags: Flags::Wbinvd as u32
                    | Flags::ProcC1 as u32
                    | Flags::SlpButton as u32
                    | Flags::RtcS4 as u32
                    | Flags::DckCap as u32
                    | Flags::UsePlatformClock as u32,
                p_lvl3_lat: 0x39,
                duty_width: 3,
                pm2_cnt_offset: Some(0x50),
                ..X86Fadt::default()
            },
        }
    }
}

impl SouthbridgeAcpi for IntelIch9 {
    /// Produce ICH9-M DSDT content in the northbridge's `pci_root`.
    /// Mainboard-specific EC, dock/GPE/SMI trap glue stays with the board.
    fn dsdt_aml(&self, config: &Self::Config, pci_root: &AcpiPath) -> Vec<u8> {
        // PM/GPIO SystemIO OpRegions are intentionally omitted until an
        // emitted AML method actually consumes them. Declaring unused
        // regions makes ACPI claim the native lpc_ich driver resources.
        // Coreboot sleepstates.asl; S3 only once the platform persisted
        // training and stage caches for this image.
        let mut aml: Vec<u8> = acpi_dsl! {
            Scope("\\") {
                Name("_S0_", Package(0u32, 0u32, 0u32, 0u32));
                Name("_S5_", Package(7u32, 0u32, 0u32, 0u32));
            }
        }
        .into();
        if self.s3_enabled {
            aml.extend_from_slice(&acpi_dsl! { Scope("\\") {
                Name("_S3_", Package(5u32, 0u32, 0u32, 0u32));
            } });
        }
        // When keeping strap routing, _PRT must describe the live router,
        // not an assumed A-D map (coreboot intel/common/rcba_pirq.c).
        let hardware_routes = if config.preserve_hardware_pirq {
            use crate::southbridge::pirq::{PciPin, PinRoute, Pirq};
            let r = self.rcba().regs();
            // Devices 1..3 are outside the RCBA router: coreboot's
            // map_pirq uses the hardwired pin-to-PIRQ A..D identity there.
            let slots = [1, 2, 3, 0x19, 0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f];
            let ir = [
                0x3210,
                0x3210,
                0x3210,
                r.d25ir.get(),
                r.d26ir.get(),
                r.d27ir.get(),
                r.d28ir.get(),
                r.d29ir.get(),
                r.d30ir.get(),
                r.d31ir.get(),
            ];
            // The tables are built once per cold boot; the routes stay
            // with them like the board's static routing would.
            Some(&*alloc::boxed::Box::leak(alloc::boxed::Box::new(
                core::array::from_fn::<_, 40, _>(|index| {
                    let pin = PciPin::ALL[index % 4];
                    PinRoute {
                        slot: slots[index / 4],
                        pin,
                        line: Pirq::from_index(((ir[index / 4] >> (4 * pin.index())) & 7) as u8)
                            .expect("three-bit PIRQ selector"),
                    }
                }),
            )))
        } else {
            None
        };
        let hardware_routing = crate::southbridge::pirq::PirqRouting {
            slot_pins: &[],
            pin_routes: hardware_routes.map_or(&[], |r| &r[..]),
            bridge_routes: &[],
        };
        let routing = config.pirq.as_ref().unwrap_or(&hardware_routing);

        // Chipset RCRB, PCI bridge and the shared ICH namespace fragments.
        let rcba = config.rcba as u32;
        let mut body: Vec<u8> = acpi_dsl! {
            OperationRegion("RCRB", SystemMemory, #{dword rcba}, 0x4000u32);
            Field("RCRB", DWordAcc, Lock, Preserve) {
                Offset(0x3404),
                HPAS, 2,
                , 5,
                HPTE, 1,
                Offset(0x3418),
                , 2,
                SA1D, 1,
                SMBD, 1,
                HDAD, 1,
                , 2,
                U6DD, 1,
                US1D, 1,
                US2D, 1,
                US3D, 1,
                US4D, 1,
                US5D, 1,
                EH2D, 1,
                LPBD, 1,
                EH1D, 1,
                Offset(0x341A),
                RP1D, 1,
                RP2D, 1,
                RP3D, 1,
                RP4D, 1,
                RP5D, 1,
                RP6D, 1,
                , 2,
                THRD, 1,
            }
        }
        .into();

        body.extend_from_slice(&fstart_acpi::pirq::prt_name_aml(
            routing
                .root_bus_routes()
                .map(|(slot, pin, gsi)| (slot, pin.index(), gsi)),
        ));

        body.extend_from_slice(&acpi_fragments::hda_node());
        // ICH9 USB1-3 are on 0x1D, USB4-6 on 0x1A; 0x1D has no function 3.
        for (function, name) in ["USB1", "USB2", "USB3"].iter().enumerate() {
            let adr = 0x001D_0000u32 | function as u32;
            body.extend_from_slice(&acpi_fragments::uhci_node(name, adr, 3));
        }
        for (function, name) in ["USB4", "USB5", "USB6"].iter().enumerate() {
            let adr = 0x001A_0000u32 | function as u32;
            body.extend_from_slice(&acpi_fragments::uhci_node(name, adr, 3));
        }
        body.extend_from_slice(&acpi_fragments::ehci_node("EHC1", 0x001D_0007, 13, 6));
        body.extend_from_slice(&acpi_fragments::ehci_node("EHC2", 0x001A_0007, 13, 6));
        for port in 1..=6u8 {
            let adr = 0x001C_0000u32 + u32::from(port - 1);
            let name = alloc::format!("RP0{port}");
            body.extend_from_slice(&root_port_aml(
                &name,
                adr,
                port,
                config.pcie_hotplug[(port - 1) as usize],
                routing,
            ));
        }
        body.extend_from_slice(&acpi_fragments::sata_node());
        body.extend_from_slice(&acpi_fragments::smbus_node());

        // Downstream slot wiring belongs to the board, not the ICH9.
        let pcib: Vec<u8> = acpi_dsl! {
            Device("PCIB") {
                Name("_ADR", 0x001E0000u32);
            }
        }
        .into();
        body.extend_from_slice(&pcib);

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
            }
        }
        .into();
        lpcb.extend_from_slice(&acpi_fragments::legacy_isa_children(
            ich9::DEFAULT_PMBASE,
            ich9::DEFAULT_GPIOBASE,
        ));
        lpcb.extend_from_slice(&acpi_fragments::pci_irq_links(&config.pirq_routing));
        lpcb.extend_from_slice(&acpi_fragments::ps2_controller_nodes());

        body.extend_from_slice(
            &fstart_acpi::aml_linker::device_vec(Self::ACPI_LPC_NAME, &lpcb)
                .expect("ICH9 LPCB device emission"),
        );

        aml.extend_from_slice(
            &fstart_acpi::aml_linker::scope_vec(pci_root.as_str(), &body)
                .expect("ICH9 PCI scope emission"),
        );
        aml
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::IntelSouthbridgeDriver;

    /// `Name("_ADR", <dword>)` as the DSL encodes it.
    fn adr_name_aml(adr: u32) -> [u8; 10] {
        let mut bytes = [0u8; 10];
        bytes[0] = 0x08; // NameOp
        bytes[1..5].copy_from_slice(b"_ADR");
        bytes[5] = 0x0C; // DwordPrefix
        bytes[6..10].copy_from_slice(&adr.to_le_bytes());
        bytes
    }

    #[test]
    fn pcie_prt_follows_the_programmed_ich9_d28ir() {
        let aml = root_port_aml("RP01", 0x001c_0000, 1, false, &super::super::tests::ROUTING);
        assert!(aml.windows(5).any(|bytes| bytes == [0x0c, 22, 0, 0, 0]));
    }

    #[test]
    fn uhci_nodes_match_the_ich9_function_layout() {
        let mut config = IntelIch9Config::new();
        config.pirq = Some(super::super::tests::ROUTING);
        let config = alloc::boxed::Box::leak(alloc::boxed::Box::new(config));
        let south = IntelIch9::new_from_config(config).expect("ICH9 config is valid");
        let aml = south.dsdt_aml(config, &AcpiPath::SB.child("PCI0"));
        let has_adr = |adr: u32| aml.windows(10).any(|window| window == adr_name_aml(adr));
        for function in 0..3u32 {
            assert!(has_adr(0x001D_0000 | function), "missing 0:1D.{function}");
            assert!(has_adr(0x001A_0000 | function), "missing 0:1A.{function}");
        }
        assert!(!has_adr(0x001D_0003), "0:1D.3 does not exist on ICH9");
    }
}
