//! ACPI fragments shared by the ICH southbridges.
//!
//! The LPC bridge's ISA children, the PIRQ link devices and the PCIe root-port
//! INTx convention are the same on every ICH, and copying them per chipset had
//! already let them drift: different PIC/LDRC I/O ranges, link `_CRS` values
//! that disagreed with the programmed routing, a missing EHCI root hub. Each
//! chipset adds only what is genuinely its own.

#![cfg(feature = "acpi")]

extern crate alloc;

use alloc::vec::Vec;
use fstart_acpi_macros::acpi_dsl;

/// Legacy ISA devices behind the LPC bridge (coreboot `i82801gx/acpi/lpc.asl`).
///
/// Only the decoded PM1 and GPIO windows differ between ICH generations, so
/// the chipset passes its own; the rest is coreboot's descriptor set.
#[must_use]
pub fn legacy_isa_children(pm1_base: u16, gpio_base: u16) -> Vec<u8> {
    acpi_dsl! {
        Device("DMAC") {
            Name("_HID", EisaId("PNP0200"));
            Name("_CRS", ResourceTemplate {
                IO(0x0000u16, 0x0000u16, 0x01u8, 0x20u8);
                IO(0x0081u16, 0x0081u16, 0x01u8, 0x11u8);
                IO(0x0093u16, 0x0093u16, 0x01u8, 0x0Du8);
                IO(0x00C0u16, 0x00C0u16, 0x01u8, 0x20u8);
            });
        }

        Device("FWH_") {
            Name("_HID", EisaId("INT0800"));
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadOnly, 0xFF000000u32, 0x01000000u32);
            });
        }

        Device("HPET") {
            Name("_HID", EisaId("PNP0103"));
            Name("_CID", 0x010CD041u32);
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadOnly, 0xFED00000u32, 0x400u32);
            });
        }

        Device("PIC_") {
            Name("_HID", EisaId("PNP0000"));
            Name("_CRS", ResourceTemplate {
                IO(0x0020u16, 0x0020u16, 0x01u8, 0x02u8);
                IO(0x0024u16, 0x0024u16, 0x01u8, 0x02u8);
                IO(0x0028u16, 0x0028u16, 0x01u8, 0x02u8);
                IO(0x002Cu16, 0x002Cu16, 0x01u8, 0x02u8);
                IO(0x0030u16, 0x0030u16, 0x01u8, 0x02u8);
                IO(0x0034u16, 0x0034u16, 0x01u8, 0x02u8);
                IO(0x0038u16, 0x0038u16, 0x01u8, 0x02u8);
                IO(0x003Cu16, 0x003Cu16, 0x01u8, 0x02u8);
                IO(0x00A0u16, 0x00A0u16, 0x01u8, 0x02u8);
                IO(0x00A4u16, 0x00A4u16, 0x01u8, 0x02u8);
                IO(0x00A8u16, 0x00A8u16, 0x01u8, 0x02u8);
                IO(0x00ACu16, 0x00ACu16, 0x01u8, 0x02u8);
                IO(0x00B0u16, 0x00B0u16, 0x01u8, 0x02u8);
                IO(0x00B4u16, 0x00B4u16, 0x01u8, 0x02u8);
                IO(0x00B8u16, 0x00B8u16, 0x01u8, 0x02u8);
                IO(0x00BCu16, 0x00BCu16, 0x01u8, 0x02u8);
                IO(0x04D0u16, 0x04D0u16, 0x01u8, 0x02u8);
                IRQ(Edge, ActiveHigh, Exclusive, 2u32);
            });
        }

        Device("MATH") {
            Name("_HID", EisaId("PNP0C04"));
            Name("_CRS", ResourceTemplate {
                IO(0x00F0u16, 0x00F0u16, 0x01u8, 0x01u8);
                IRQ(Edge, ActiveHigh, Exclusive, 13u32);
            });
        }

        Device("LDRC") {
            Name("_HID", EisaId("PNP0C02"));
            Name("_UID", 2u32);
            Name("_CRS", ResourceTemplate {
                IO(0x002Eu16, 0x002Eu16, 0x01u8, 0x02u8);
                IO(0x004Eu16, 0x004Eu16, 0x01u8, 0x02u8);
                IO(0x0061u16, 0x0061u16, 0x01u8, 0x01u8);
                IO(0x0063u16, 0x0063u16, 0x01u8, 0x01u8);
                IO(0x0065u16, 0x0065u16, 0x01u8, 0x01u8);
                IO(0x0067u16, 0x0067u16, 0x01u8, 0x01u8);
                IO(0x0080u16, 0x0080u16, 0x01u8, 0x01u8);
                IO(0x0092u16, 0x0092u16, 0x01u8, 0x01u8);
                IO(0x00B2u16, 0x00B2u16, 0x01u8, 0x02u8);
                IO(0x0800u16, 0x0800u16, 0x01u8, 0x10u8);
                IO(#{word pm1_base}, #{word pm1_base}, 0x01u8, 0x80u8);
                IO(#{word gpio_base}, #{word gpio_base}, 0x01u8, 0x40u8);
            });
        }

        Device("RTC_") {
            Name("_HID", EisaId("PNP0B00"));
            Name("_CRS", ResourceTemplate {
                IO(0x0070u16, 0x0070u16, 0x01u8, 0x08u8);
                IRQ(Edge, ActiveHigh, Exclusive, 8u32);
            });
        }

        Device("TIMR") {
            Name("_HID", EisaId("PNP0100"));
            Name("_CRS", ResourceTemplate {
                IO(0x0040u16, 0x0040u16, 0x01u8, 0x04u8);
                IO(0x0050u16, 0x0050u16, 0x10u8, 0x04u8);
                IRQ(Edge, ActiveHigh, Exclusive, 0u32);
            });
        }
    }
    .into()
}

/// PIRQ link devices `LNKA`-`LNKH`.
///
/// `_CRS` reports the IRQ the LPC `PIRQx_ROUT` registers were programmed with
/// and `_STA` reports a link whose routing is disabled as inactive, so OSPM
/// sees the routing the firmware installed (coreboot `irqlinks.asl`).
#[must_use]
pub fn pci_irq_links(pirq_routing: &[u8; 8]) -> Vec<u8> {
    let irq = |index: usize| u32::from(pirq_routing[index] & 0x0f);
    let state = |index: usize| {
        if pirq_routing[index] & 0x80 != 0 {
            0x09u32
        } else {
            0x0Bu32
        }
    };
    let (irq_a, irq_b, irq_c, irq_d) = (irq(0), irq(1), irq(2), irq(3));
    let (irq_e, irq_f, irq_g, irq_h) = (irq(4), irq(5), irq(6), irq(7));
    let (sta_a, sta_b, sta_c, sta_d) = (state(0), state(1), state(2), state(3));
    let (sta_e, sta_f, sta_g, sta_h) = (state(4), state(5), state(6), state(7));

    let mut links: Vec<u8> = Vec::new();
    let link_a: Vec<u8> = acpi_dsl! {
        Device("LNKA") {
            Name("_HID", EisaId("PNP0C0F"));
            Name("_UID", 1u32);
            Name("_PRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared,
                    3u32, 4u32, 5u32, 6u32, 7u32,
                    10u32, 11u32, 12u32, 14u32, 15u32);
            });
            Name("_CRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq_a});
            });
            Method("_STA", 0, NotSerialized) { Return(#{dword sta_a}); }
        }
    }
    .into();
    links.extend_from_slice(&link_a);
    let link_b: Vec<u8> = acpi_dsl! {
        Device("LNKB") {
            Name("_HID", EisaId("PNP0C0F"));
            Name("_UID", 2u32);
            Name("_PRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared,
                    3u32, 4u32, 5u32, 6u32, 7u32,
                    10u32, 11u32, 12u32, 14u32, 15u32);
            });
            Name("_CRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq_b});
            });
            Method("_STA", 0, NotSerialized) { Return(#{dword sta_b}); }
        }
    }
    .into();
    links.extend_from_slice(&link_b);
    let link_c: Vec<u8> = acpi_dsl! {
        Device("LNKC") {
            Name("_HID", EisaId("PNP0C0F"));
            Name("_UID", 3u32);
            Name("_PRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared,
                    3u32, 4u32, 5u32, 6u32, 7u32,
                    10u32, 11u32, 12u32, 14u32, 15u32);
            });
            Name("_CRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq_c});
            });
            Method("_STA", 0, NotSerialized) { Return(#{dword sta_c}); }
        }
    }
    .into();
    links.extend_from_slice(&link_c);
    let link_d: Vec<u8> = acpi_dsl! {
        Device("LNKD") {
            Name("_HID", EisaId("PNP0C0F"));
            Name("_UID", 4u32);
            Name("_PRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared,
                    3u32, 4u32, 5u32, 6u32, 7u32,
                    10u32, 11u32, 12u32, 14u32, 15u32);
            });
            Name("_CRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq_d});
            });
            Method("_STA", 0, NotSerialized) { Return(#{dword sta_d}); }
        }
    }
    .into();
    links.extend_from_slice(&link_d);
    let link_e: Vec<u8> = acpi_dsl! {
        Device("LNKE") {
            Name("_HID", EisaId("PNP0C0F"));
            Name("_UID", 5u32);
            Name("_PRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared,
                    3u32, 4u32, 5u32, 6u32, 7u32,
                    10u32, 11u32, 12u32, 14u32, 15u32);
            });
            Name("_CRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq_e});
            });
            Method("_STA", 0, NotSerialized) { Return(#{dword sta_e}); }
        }
    }
    .into();
    links.extend_from_slice(&link_e);
    let link_f: Vec<u8> = acpi_dsl! {
        Device("LNKF") {
            Name("_HID", EisaId("PNP0C0F"));
            Name("_UID", 6u32);
            Name("_PRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared,
                    3u32, 4u32, 5u32, 6u32, 7u32,
                    10u32, 11u32, 12u32, 14u32, 15u32);
            });
            Name("_CRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq_f});
            });
            Method("_STA", 0, NotSerialized) { Return(#{dword sta_f}); }
        }
    }
    .into();
    links.extend_from_slice(&link_f);
    let link_g: Vec<u8> = acpi_dsl! {
        Device("LNKG") {
            Name("_HID", EisaId("PNP0C0F"));
            Name("_UID", 7u32);
            Name("_PRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared,
                    3u32, 4u32, 5u32, 6u32, 7u32,
                    10u32, 11u32, 12u32, 14u32, 15u32);
            });
            Name("_CRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq_g});
            });
            Method("_STA", 0, NotSerialized) { Return(#{dword sta_g}); }
        }
    }
    .into();
    links.extend_from_slice(&link_g);
    let link_h: Vec<u8> = acpi_dsl! {
        Device("LNKH") {
            Name("_HID", EisaId("PNP0C0F"));
            Name("_UID", 8u32);
            Name("_PRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared,
                    3u32, 4u32, 5u32, 6u32, 7u32,
                    10u32, 11u32, 12u32, 14u32, 15u32);
            });
            Name("_CRS", ResourceTemplate {
                Interrupt(ResourceConsumer, Level, ActiveLow, Shared, #{dword irq_h});
            });
            Method("_STA", 0, NotSerialized) { Return(#{dword sta_h}); }
        }
    }
    .into();
    links.extend_from_slice(&link_h);
    links
}

/// HD Audio controller node (`0:1b.0`).
#[must_use]
pub fn hda_node() -> Vec<u8> {
    acpi_dsl! {
        Device("HDEF") {
            Name("_ADR", 0x001B0000u32);
            Name("_PRW", Package(5u32, 4u32));
        }
    }
    .into()
}

/// One UHCI controller node.
///
/// The address, wake GPE and device name are the chipset's own layout, so the
/// caller supplies them; what is shared is the wake and D-state policy.
#[must_use]
pub fn uhci_node(name: &str, adr: u32, wake_gpe: u32) -> Vec<u8> {
    let body: Vec<u8> = acpi_dsl! {
        Name("_ADR", #{dword adr});
        Name("_PRW", Package(#{dword wake_gpe}, 4u32));
        Method("_S3D", 0, NotSerialized) { Return(2u32); }
        Method("_S4D", 0, NotSerialized) { Return(2u32); }
    }
    .into();
    fstart_acpi::aml_linker::device_vec(name, &body).expect("UHCI device emission")
}

/// One EHCI controller node, optionally with its root hub port children.
#[must_use]
pub fn ehci_node(name: &str, adr: u32, wake_gpe: u32, root_ports: u8) -> Vec<u8> {
    let mut body: Vec<u8> = acpi_dsl! {
        Name("_ADR", #{dword adr});
        Name("_PRW", Package(#{dword wake_gpe}, 4u32));
        Method("_S3D", 0, NotSerialized) { Return(2u32); }
        Method("_S4D", 0, NotSerialized) { Return(2u32); }
    }
    .into();
    if root_ports != 0 {
        body.extend_from_slice(&root_hub_node(root_ports));
    }
    fstart_acpi::aml_linker::device_vec(name, &body).expect("EHCI device emission")
}

/// Root hub children of an EHCI controller.
#[must_use]
fn root_hub_node(ports: u8) -> Vec<u8> {
    let mut body: Vec<u8> = acpi_dsl! {
        Name("_ADR", #{dword 0u32});
    }
    .into();
    for port in 1..=ports {
        let adr = u32::from(port);
        let child: Vec<u8> = acpi_dsl! {
            Name("_ADR", #{dword adr});
        }
        .into();
        body.extend_from_slice(
            &fstart_acpi::aml_linker::device_vec(&alloc::format!("PRT{port}"), &child)
                .expect("root hub port emission"),
        );
    }
    fstart_acpi::aml_linker::device_vec("HUB7", &body).expect("root hub emission")
}

/// SATA controller node (`0:1f.2`) with its port children.
#[must_use]
pub fn sata_node() -> Vec<u8> {
    acpi_dsl! {
        Device("SATA") {
            Name("_ADR", 0x001F0002u32);
            Device("PRID") {
                Name("_ADR", 0u32);
                Device("DSK0") { Name("_ADR", 0u32); }
                Device("DSK1") { Name("_ADR", 1u32); }
            }
        }
    }
    .into()
}

/// SMBus controller node (`0:1f.3`).
#[must_use]
pub fn smbus_node() -> Vec<u8> {
    acpi_dsl! {
        Device("SBUS") {
            Name("_ADR", 0x001F0003u32);
        }
    }
    .into()
}

/// One PCIe root port with its APIC-mode `_PRT`.
///
/// INTx is rotated by port number, matching coreboot's `pcie.asl` interrupt
/// maps: ports 1 and 5 start at GSI 16, ports 2 and 6 at 17, and so on.
/// `hotplug` adds the config-space `RPCS` region the hotplug fields live in.
#[must_use]
pub fn pcie_root_port(name: &str, adr: u32, port: u8, hotplug: bool) -> Vec<u8> {
    let base = u32::from((port - 1) % 4);
    let gsi = |offset: u32| 16 + (base + offset) % 4;
    let (a, b, c, d) = (gsi(0), gsi(1), gsi(2), gsi(3));

    let mut body: Vec<u8> = acpi_dsl! {
        Name("_ADR", #{dword adr});
    }
    .into();
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

    fstart_acpi::aml_linker::device_vec(name, &body).expect("root port device emission")
}
