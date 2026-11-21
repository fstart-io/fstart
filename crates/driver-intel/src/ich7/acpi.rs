//! ICH7 ACPI tables and device namespace, derived from the driver's policy.

extern crate alloc;
use crate::IntelSouthbridgeDriver;
use crate::namespace::SouthbridgeAcpi;
use crate::southbridge::acpi as acpi_fragments;
use alloc::vec::Vec;
use fstart_acpi::AcpiPath;
use fstart_acpi::pci_root::adr;
use fstart_acpi::platform::{
    HpetConfig, IoApicConfig, IsoConfig, X86Config, X86Fadt, X86PlatformProvider,
};
use fstart_acpi_macros::acpi_dsl;

use super::*;

fn pci0_scope_aml(pci_root: &AcpiPath, children: &[u8]) -> Vec<u8> {
    fstart_acpi::aml_linker::scope_vec(pci_root.as_str(), children)
        .expect("ICH7 required PCI scope emission failed")
}

/// ICH7 PCI device IDs (LPC bridge variants).
#[allow(dead_code)]
const LPC_PIDS: &[u16] = &[0x27B0, 0x27B8, 0x27B9, 0x27BC, 0x27BD];

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

impl X86PlatformProvider for IntelIch7 {
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
            gpe0_offset: 0x28,
            gpe0_length: 8,
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

impl SouthbridgeAcpi for IntelIch7 {
    /// Produce ICH7 DSDT content for the LPC bridge and all its PCI
    /// function siblings.
    ///
    /// The output is a flat sequence of AML objects.  The caller is
    /// expected to embed them inside the appropriate PCI0 scope.
    /// Sleep-state objects (`_S0_`, `_S5_`) are appended at the end
    /// for placement at root scope by the caller.
    ///
    /// # LPCB children (ISA legacy devices)
    ///
    /// `DMAC` — 8237 DMA controller (PNP0200)
    /// `FWH_` — Firmware Hub (INT0800)
    /// `HPET` — High Precision Event Timer (PNP0103)
    /// `PIC_` — 8259 interrupt controller (PNP0000)
    /// `MATH` — FPU / x87 (PNP0C04)
    /// `LDRC` — LPC resource consumption (PNP0C02)
    /// `RTC_` — Real Time Clock (PNP0B00)
    /// `TIMR` — 8254 timer (PNP0100)
    ///
    /// # PCI function siblings (at PCI0 scope level)
    ///
    /// `HDEF` 0:1B.0, `USB1`–`USB4` 0:1D.0-3, `EHC1` 0:1D.7,
    /// `RP01`–`RP06` 0:1C.0-5, `PCIB` 0:1E.0,
    /// `SATA` 0:1F.2, `PATA` 0:1F.1, `SBUS` 0:1F.3.
    ///
    /// Ported from coreboot `src/southbridge/intel/i82801gx/acpi/`.
    fn dsdt_aml(&self, config: &Self::Config, pci_root: &AcpiPath) -> Vec<u8> {
        // ---------------------------------------------------------------
        // 1. LPCB device with all ISA legacy children.
        //
        // IRQNoFlags() is not a supported macro keyword; we substitute
        // the extended Interrupt() descriptor (opcode 0x89) which Linux
        // handles equivalently for ISA legacy IRQs.
        //
        // DMA() channel descriptor is also unsupported by the macro and
        // is omitted — Linux does not require it for boot enumeration.
        // ---------------------------------------------------------------
        // PM/GPIO SystemIO OpRegions are intentionally omitted until an
        // emitted AML method actually consumes them. Declaring unused
        // regions makes ACPI claim the native lpc_ich driver resources.
        let mut aml = Vec::new();

        aml.extend_from_slice(&pci0_scope_aml(
            pci_root,
            &acpi_dsl! {
                    OperationRegion("RCRB", SystemMemory, 0xFED1C000u32, 0x4000u32);
                    Field("RCRB", DWordAcc, Lock, Preserve) {
                        Offset(0x3404),
                        HPAS, 2,
                        , 5,
                        HPTE, 1,
                        Offset(0x3418),
                        , 1,
                        PATD, 1,
                        SATD, 1,
                        SMBD, 1,
                        HDAD, 1,
                        A97D, 1,
                        M97D, 1,
                        ILND, 1,
                        US1D, 1,
                        US2D, 1,
                        US3D, 1,
                        US4D, 1,
                        , 2,
                        LPBD, 1,
                        EHCD, 1,
                        RP1D, 1,
                        RP2D, 1,
                        RP3D, 1,
                        RP4D, 1,
                        RP5D, 1,
                        RP6D, 1,
                    }
            },
        ));

        let mut pci0_aml = Vec::new();
        // The LPC bridge's own registers, then the ISA children and PIRQ
        // links every ICH shares (see `southbridge::acpi`).
        let mut lpcb: Vec<u8> = acpi_dsl! {

                Name("_ADR", #{dword adr(ich7::LPC)});
                Name("_HID", EisaId("PNP0A05"));

                // LPC PCI config OpRegion for PIRQ routing registers.
                // PRTA-PRTH at offsets 0x60-0x63 and 0x68-0x6B.
                // Read by PIRQ link devices (LNKA-LNKH) and by the
                // OS to discover current IRQ assignments.
                // Coreboot: lpc.asl OperationRegion(LPC0)
                OperationRegion("LPC0", PciConfig, 0x00u32, 0x100u32);
                Field("LPC0", AnyAcc, NoLock, Preserve) {
                    Offset(0x60),
                    PRTA, 8,
                    PRTB, 8,
                    PRTC, 8,
                    PRTD, 8,
                    Offset(0x68),
                    PRTE, 8,
                    PRTF, 8,
                    PRTG, 8,
                    PRTH, 8,
                }
        }
        .into();
        lpcb.extend_from_slice(&acpi_fragments::legacy_isa_children(
            DEFAULT_PMBASE as u16,
            DEFAULT_GPIOBASE as u16,
        ));
        lpcb.extend_from_slice(&acpi_fragments::pci_irq_links(&config.pirq_routing));
        pci0_aml.extend_from_slice(
            &fstart_acpi::aml_linker::device_vec(Self::ACPI_LPC_NAME, &lpcb)
                .expect("LPCB device emission"),
        );

        // PS/2 keyboard and mouse nodes come from the concrete SuperIO
        // driver when the board enables those LDNs, not from the reusable
        // LPC bridge.

        // ---------------------------------------------------------------
        // 2. Per-function PCI device nodes — siblings to LPCB at PCI0
        //    scope level.  Includes _PRW (Power Resources for Wake)
        //    for USB and HDA devices.
        //
        //    Ported from coreboot:
        //      audio_ich.asl, usb.asl, pcie.asl, pci.asl,
        //      sata.asl, pata.asl, smbus.asl
        // ---------------------------------------------------------------

        // Root-bus PCI interrupt routing, generated from the same data the
        // router registers are programmed with: each pin delivers on the
        // PIRQ line it is wired to, and PIRQs map to GSIs from 16 on.
        // The root ports and the PCI bridge carry their own `_PRT` for the
        // buses behind them.
        let routes = config
            .pirq
            .root_bus_routes()
            .map(|(slot, pin, gsi)| (slot, pin.index(), gsi));
        pci0_aml.extend_from_slice(&fstart_acpi::pirq::prt_name_aml(routes));

        // HDEF — HD Audio controller  0:1B.0
        // _PRW: GPE bit 5, can wake from S4.
        pci0_aml.extend_from_slice(&acpi_fragments::hda_node(ich7::HDA));

        // USB UHCI controllers  0:1D.0–3
        // _PRW: GPE bit 3, can wake from S4.
        // _S3D/_S4D: highest D-state in S3/S4 (D2 — USB stays
        //   partially powered for wake-on-USB).
        // Coreboot: usb.asl USB1–USB4
        for (function, name) in [
            ich7::USB1,
            ich7::USB2,
            ich7::USB3,
            fstart_pci::devfn(0x1d, 3),
        ]
        .into_iter()
        .zip(["USB1", "USB2", "USB3", "USB4"])
        {
            pci0_aml.extend_from_slice(&acpi_fragments::uhci_node(name, function, 3));
        }

        // EHC1 — EHCI USB 2.0 controller  0:1D.7
        // Includes root hub (HUB7) with 6 port child devices.
        // _PRW: GPE bit 13, can wake from S4.
        // Coreboot: usb.asl EHC1 + HUB7
        pci0_aml.extend_from_slice(&acpi_fragments::ehci_node("EHC1", ich7::EHCI, 13, 6));

        // ---------------------------------------------------------------
        // PCIe root ports  0:1C.0–5 with APIC-mode _PRT routing.
        //
        // Each root port's _PRT maps INTA-INTD to IOAPIC GSIs 16-19
        // with a rotation based on port number (matching coreboot's
        // pcie.asl IRQM method).  The rotation is:
        //   Port 1,5: A→16 B→17 C→18 D→19
        //   Port 2,6: A→17 B→18 C→19 D→16
        //   Port 3:   A→18 B→19 C→16 D→17
        //   Port 4:   A→19 B→16 C→17 D→18
        //
        // We emit APIC-mode routing only (GSI values, no link
        // devices) since the kernel uses IOAPIC when available.
        // ---------------------------------------------------------------

        pci0_aml.extend_from_slice(&acpi_fragments::pcie_root_port(
            "RP01",
            ich7::root_port(1),
            1,
            true,
        ));
        pci0_aml.extend_from_slice(&acpi_fragments::pcie_root_port(
            "RP02",
            ich7::root_port(2),
            2,
            true,
        ));
        pci0_aml.extend_from_slice(&acpi_fragments::pcie_root_port(
            "RP03",
            ich7::root_port(3),
            3,
            true,
        ));
        pci0_aml.extend_from_slice(&acpi_fragments::pcie_root_port(
            "RP04",
            ich7::root_port(4),
            4,
            true,
        ));
        pci0_aml.extend_from_slice(&acpi_fragments::pcie_root_port(
            "RP05",
            ich7::root_port(5),
            5,
            true,
        ));
        pci0_aml.extend_from_slice(&acpi_fragments::pcie_root_port(
            "RP06",
            ich7::root_port(6),
            6,
            true,
        ));
        pci0_aml.extend_from_slice(&acpi_fragments::pcie_root_port(
            "RP07",
            ich7::root_port(7),
            7,
            true,
        ));
        pci0_aml.extend_from_slice(&acpi_fragments::pcie_root_port(
            "RP08",
            ich7::root_port(8),
            8,
            true,
        ));

        // PCIB — PCI-to-PCI bridge  0:1E.0, with the board's `_PRT`
        // for the devices behind it.
        pci0_aml.extend_from_slice(&acpi_fragments::pci_bridge_node(
            fstart_pci::devfn(0x1e, 0),
            &config.pirq,
        ));

        // AC’97 — audio (0:1E.2) and modem (0:1E.3).
        // Legacy audio/modem on ICH7 — behind the PCI bridge.
        // Most boards use HDA instead, but ICH7 variants still
        // have these functions.  Modem can wake from S4.
        // Coreboot: ac97.asl
        // 0:1e.2 and 0:1e.3 are the bridges the AC'97 and modem codecs
        // sit behind.
        let (audio_adr, modem_adr) = (
            adr(fstart_pci::devfn(0x1e, 2)),
            adr(fstart_pci::devfn(0x1e, 3)),
        );
        pci0_aml.extend(Vec::<u8>::from(acpi_dsl! {
            Device("AUD0") {
                Name("_ADR", #{dword audio_adr});
            }
        }));
        pci0_aml.extend(Vec::<u8>::from(acpi_dsl! {
            Device("MODM") {
                Name("_ADR", #{dword modem_adr});
                Name("_PRW", Package(5u32, 4u32));
            }
        }));

        // SATA — SATA controller  0:1F.2
        // Primary channel (PRID) with two disk children.
        // _GTM/_STM timing methods omitted (need CreateDwordField;
        // Linux libata doesn’t use them in AHCI mode).
        // Coreboot: sata.asl
        pci0_aml.extend_from_slice(&acpi_fragments::sata_node(ich7::SATA));

        // PATA — IDE / PATA controller  0:1F.1
        // Coreboot: pata.asl
        let pata_adr = adr(fstart_pci::devfn(0x1f, 1));
        pci0_aml.extend(Vec::<u8>::from(acpi_dsl! {
            Device("PATA") {
                Name("_ADR", #{dword pata_adr});
                Device("PRID") {
                    Name("_ADR", 0u32);
                    Device("DSK0") { Name("_ADR", 0u32); }
                    Device("DSK1") { Name("_ADR", 1u32); }
                }
            }
        }));

        // SBUS — SMBus controller  0:1F.3
        pci0_aml.extend_from_slice(&acpi_fragments::smbus_node(ich7::SMBUS));

        aml.extend_from_slice(&pci0_scope_aml(pci_root, &pci0_aml));

        // ---------------------------------------------------------------
        // 3. Root-scope objects.
        // ---------------------------------------------------------------

        // _PIC method + PICM variable.
        //
        // The OS calls _PIC(1) during boot to signal APIC mode.
        // PICM is used by _PRT methods to select PIC vs APIC routing.
        // Since we only emit APIC-mode _PRT tables, PICM is
        // informational — but Linux expects _PIC to exist.
        // Coreboot: dsdt_top.asl
        aml.extend_from_slice(&acpi_dsl! {
            Scope("\\") {
                Name("PICM", 0u32);
                Method("_PIC", 1, NotSerialized) {
                    Store(Arg0, PICM);
                }
            }
        });

        // System sleep states.
        // S0 = working, S3 = suspend-to-RAM, S4 = hibernate, S5 = soft-off.
        // SLP_TYP values match ICH7 pmio::PM1_CNT encoding.
        // Coreboot: sleepstates.asl
        aml.extend_from_slice(&acpi_dsl! {
            Scope("\\") {
                Name("_S0_", Package(0u32, 0u32, 0u32, 0u32));
                Name("_S4_", Package(6u32, 4u32, 0u32, 0u32));
                Name("_S5_", Package(7u32, 0u32, 0u32, 0u32));
            }
        });

        if self.s3_enabled {
            aml.extend_from_slice(&acpi_dsl! { Scope("\\") {
                Name("_S3_", Package(5u32, 0u32, 0u32, 0u32));
            } });
        }
        aml
    }

    /// Produce standalone FADT fields as extra table bytes.
    ///
    /// The PM block addresses are carried in the x86 platform config
    /// (`X86PlatformAcpi`), so the LPC bridge does not produce any
    /// standalone tables. PIRQ routing is handled by board metadata
    /// `isos` (Interrupt Source Overrides) in the MADT.
    fn extra_tables(&self, _config: &Self::Config) -> Vec<Vec<u8>> {
        Vec::new()
    }
}
