//! ICH8 ACPI tables and device namespace, derived from the driver's policy.

extern crate alloc;

use crate::IntelSouthbridgeDriver;
use crate::namespace::SouthbridgeAcpi;
use alloc::vec::Vec;
use fstart_acpi::AcpiPath;
use fstart_acpi::platform::{
    HpetConfig, IoApicConfig, IsoConfig, X86Config, X86Fadt, X86PlatformProvider,
};

use fstart_acpi::pci_root::adr;
use fstart_acpi_macros::acpi_dsl;

use super::*;
use crate::southbridge::acpi as acpi_fragments;

const LAPIC_BASE: u64 = 0xFEE0_0000;
const IOAPIC_BASE: u64 = 0xFEC0_0000;
const HPET_BASE: u64 = 0xFED0_0000;
/// HPET capabilities ID the tables have always advertised for the ICH.
const HPET_ID: u32 = 0x8086_A201;
const SCI_IRQ: u8 = 9;
const PMBASE: u16 = 0x0500;
/// APM command port firmware and OSPM use to switch ACPI mode.
const APM_CNT: u32 = crate::southbridge::smi::APM_CNT as u32;
use crate::southbridge::smi::{APM_CNT_ACPI_DISABLE, APM_CNT_ACPI_ENABLE};

static IOAPICS: [IoApicConfig; 1] = [IoApicConfig {
    id: 0,
    base: IOAPIC_BASE,
    gsi_base: 0,
}];

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

impl X86PlatformProvider for IntelIch8 {
    fn x86_platform_config(&self, online_cpus: u32) -> X86Config {
        let _ = self;
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
            gpe0_offset: GPE0_STS_ICH8,
            gpe0_length: 16,
            // The SMM image handles the APM command port the way coreboot's
            // smihandler does: 0xe1 sets SCI_EN, 0x1e clears it.
            acpi_smi: Some(fstart_core::acpi::AcpiSmiConfig {
                smi_cmd: APM_CNT,
                acpi_enable: APM_CNT_ACPI_ENABLE,
                acpi_disable: APM_CNT_ACPI_DISABLE,
            }),
            fadt: X86Fadt::default(),
        }
    }
}

impl SouthbridgeAcpi for IntelIch8 {
    /// Produce ICH8-M DSDT content under `\\_SB.PCI0`.
    ///
    /// This is the reusable southbridge ACPI namespace: LPC legacy
    /// devices, PIRQ links, USB/HDA/SATA/SMBus/PCIe/PCI bridge device
    /// nodes, and APIC-mode `_PRT` routing. Mainboard-specific EC,
    /// dock/GPE/SMI trap glue is emitted by mainboard drivers.
    fn dsdt_aml(&self, config: &Self::Config, pci_root: &AcpiPath) -> Vec<u8> {
        // PM/GPIO SystemIO OpRegions are intentionally omitted until an
        // emitted AML method actually consumes them. Declaring unused
        // regions makes ACPI claim the native lpc_ich driver resources.
        let mut aml = Vec::new();

        // The PCI0 body: this chipset's RCRB window and downstream PCI
        // bridge, plus the fragments every ICH shares (coreboot's
        // `pcie.asl`, `usb.asl`, `azalia.asl` device nodes and the LPC
        // bridge's ISA children, PIRQ links and root-bus `_PRT`).
        let mut body: Vec<u8> = acpi_dsl! {
            OperationRegion("RCRB", SystemMemory, 0xFED1C000u32, 0x4000u32);
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
                , 3,
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
            config
                .pirq
                .root_bus_routes()
                .map(|(slot, pin, gsi)| (slot, pin.index(), gsi)),
        ));

        body.extend_from_slice(&acpi_fragments::hda_node(ich8::HDA));
        // UHCI controllers 1-3 live on device 0x1D, 4-6 on device 0x1A
        // (coreboot i82801ix usb.asl): device 0x1D has no function 3.
        for (function, name) in [
            ich8::USB1,
            ich8::USB2,
            ich8::USB3,
            ich8::USB4,
            ich8::USB5,
            ich8::USB6,
        ]
        .into_iter()
        .zip(["USB1", "USB2", "USB3", "USB4", "USB5", "USB6"])
        {
            body.extend_from_slice(&acpi_fragments::uhci_node(name, function, 3));
        }
        body.extend_from_slice(&acpi_fragments::ehci_node("EHC1", ich8::EHCI1, 13, 6));
        body.extend_from_slice(&acpi_fragments::ehci_node("EHC2", ich8::EHCI2, 13, 6));
        for port in 1..=6u8 {
            let name = alloc::format!("RP0{port}");
            body.extend_from_slice(&acpi_fragments::pcie_root_port(
                &name,
                ich8::root_port(port),
                port,
                false,
            ));
        }
        body.extend_from_slice(&acpi_fragments::sata_node(ich8::SATA));
        body.extend_from_slice(&acpi_fragments::smbus_node(ich8::SMBUS));

        // The downstream PCI bridge carries the board's own slot layout.
        body.extend_from_slice(&acpi_fragments::pci_bridge_node(
            ich8::PCI_BRIDGE,
            &config.pirq,
        ));

        // The LPC bridge: its own register window, the shared ISA children
        // and PIRQ links, then this board's PS/2 nodes.
        let mut lpcb: Vec<u8> = acpi_dsl! {
            Name("_ADR", #{dword adr(ich8::LPC)});
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
            ich8::DEFAULT_PMBASE,
            ich8::DEFAULT_GPIOBASE,
        ));
        lpcb.extend_from_slice(&acpi_fragments::pci_irq_links(&config.pirq_routing));
        lpcb.extend_from_slice(&acpi_fragments::ps2_controller_nodes());
        body.extend_from_slice(
            &fstart_acpi::aml_linker::device_vec(Self::ACPI_LPC_NAME, &lpcb)
                .expect("ICH8 LPCB device emission"),
        );

        aml.extend_from_slice(
            &fstart_acpi::aml_linker::scope_vec(pci_root.as_str(), &body)
                .expect("ICH8 PCI scope emission"),
        );
        aml
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn uhci_nodes_match_the_ich8_function_layout() {
        static CONFIG: IntelIch8Config = IntelIch8Config::new();
        let south = IntelIch8::new_from_config(&CONFIG).expect("ICH8 config is valid");
        let aml = south.dsdt_aml(&CONFIG, &AcpiPath::SB.child("PCI0"));

        let has_adr = |adr: u32| aml.windows(10).any(|window| window == adr_name_aml(adr));
        // Coreboot i82801ix: USB1-3 on device 0x1D, USB4-6 on 0x1A.
        for function in 0..3u32 {
            assert!(has_adr(0x001D_0000 | function), "missing 0:1D.{function}");
            assert!(has_adr(0x001A_0000 | function), "missing 0:1A.{function}");
        }
        // Device 0x1D has no function 3 on ICH8; USB4 is at 0:1A.0.
        assert!(!has_adr(0x001D_0003), "0:1D.3 does not exist on ICH8");
    }
}
