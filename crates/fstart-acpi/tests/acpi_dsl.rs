//! Integration tests for the `acpi_dsl!` proc-macro.
//!
//! These tests verify that the DSL produces correct AML bytecode
//! by checking opcodes, structure, and round-trip properties.

use fstart_acpi::Aml;
use fstart_acpi_macros::acpi_dsl;

#[test]
fn test_simple_device() {
    let aml: Vec<u8> = acpi_dsl! {
        device("COM0") {
            name("_HID", "ARMH0011");
            name("_UID", 0u32);
        }
    };

    // Device starts with ExtOpPrefix (0x5B) + DeviceOp (0x82)
    assert_eq!(aml[0], 0x5B);
    assert_eq!(aml[1], 0x82);
    assert!(aml.len() > 10);
}

#[test]
fn test_device_with_eisa_id() {
    let aml: Vec<u8> = acpi_dsl! {
        device("UAR0") {
            name("_HID", eisa_id("PNP0501"));
            name("_UID", 0u32);
        }
    };

    assert_eq!(aml[0], 0x5B);
    assert_eq!(aml[1], 0x82);
    assert!(aml.len() > 10);
}

#[test]
fn test_device_with_resource_template() {
    let aml: Vec<u8> = acpi_dsl! {
        device("COM0") {
            name("_HID", "ARMH0011");
            name("_CRS", resource_template {
                memory_32_fixed(ReadWrite, 0x6000_0000u32, 0x1000u32);
                interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, 33u32);
            });
        }
    };

    assert_eq!(aml[0], 0x5B);
    assert_eq!(aml[1], 0x82);
    assert!(aml.len() > 20);
}

#[test]
fn test_interpolation() {
    let uart_base: u64 = 0x6000_0000;
    let uart_irq: u32 = 33;

    let aml: Vec<u8> = acpi_dsl! {
        device("COM0") {
            name("_HID", "ARMH0011");
            name("_CRS", resource_template {
                memory_32_fixed(ReadWrite, #{uart_base}, 0x1000u32);
                interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{uart_irq});
            });
        }
    };

    assert_eq!(aml[0], 0x5B);
    assert_eq!(aml[1], 0x82);
    assert!(aml.len() > 20);
}

#[test]
fn test_method_with_return() {
    let aml: Vec<u8> = acpi_dsl! {
        method("_STA", 0, NotSerialized) {
            ret(0x0Fu32);
        }
    };

    // Method starts with byte 0x14 (MethodOp)
    assert_eq!(aml[0], 0x14);
    assert!(aml.len() > 5);
}

#[test]
fn test_scope_with_devices() {
    let aml: Vec<u8> = acpi_dsl! {
        scope("\\_SB_") {
            device("COM0") {
                name("_HID", "ARMH0011");
                name("_UID", 0u32);
            }
        }
    };

    // Scope starts with ScopeOp (0x10)
    assert_eq!(aml[0], 0x10);
    assert!(aml.len() > 20);
}

#[test]
fn test_macro_matches_manual_builder() {
    // Build the same device using the macro and the manual builder API,
    // then compare the output bytes.
    let macro_aml: Vec<u8> = acpi_dsl! {
        device("DEV0") {
            name("_HID", "TEST0001");
            name("_UID", 0u32);
        }
    };

    // Manual builder
    use fstart_acpi::aml::{Device, Name};
    let hid_val: &str = "TEST0001";
    let hid = Name::new("_HID".into(), &hid_val);
    let uid = Name::new("_UID".into(), &0u32);
    let dev = Device::new("DEV0".into(), vec![&hid, &uid]);
    let mut manual_aml = Vec::new();
    dev.to_aml_bytes(&mut manual_aml);

    assert_eq!(
        macro_aml, manual_aml,
        "macro output should match manual builder"
    );
}

// -----------------------------------------------------------------------
// x86 northbridge MCHC example (OperationRegion + Field + dynamic _CRS)
// -----------------------------------------------------------------------

/// Test OperationRegion + Field with bitfields, gaps, and Offset directives.
/// This reproduces the Intel MCH (Memory Controller Hub) PCI config space
/// register layout from the example.
#[test]
fn test_op_region_and_field() {
    let aml: Vec<u8> = acpi_dsl! {
        device("MCHC") {
            name("_ADR", 0x0000_0000u32);

            op_region("MCHP", PciConfig, 0x00u32, 0x100u32);
            field("MCHP", DWordAcc, NoLock, Preserve) {
                offset(0x40),
                // EPBAR
                EPEN, 1,
                , 11,
                EPBR, 20,
                // MCHBAR
                MHEN, 1,
                , 13,
                MHBR, 18,
                // PCIe BAR
                PXEN, 1,
                PXSZ, 2,
                , 23,
                PXBR, 6,
                // DMIBAR
                DMEN, 1,
                , 11,
                DMBR, 20,

                offset(0x90),
                // PAM0
                , 4,
                PM0H, 2,
                , 2,
                // PAM1
                PM1L, 2,
                , 2,
                PM1H, 2,
                , 2,

                offset(0x9c),
                // TOLUD
                , 3,
                TLUD, 5,

                offset(0xa0),
                // TOM
                TOM_, 16,
            }
        }
    };

    // Device starts with ExtOpPrefix (0x5B) + DeviceOp (0x82)
    assert_eq!(aml[0], 0x5B);
    assert_eq!(aml[1], 0x82);

    // Should contain "MCHC" device name
    assert!(aml.windows(4).any(|w| w == b"MCHC"));
    // Should contain "MCHP" region name
    assert!(aml.windows(4).any(|w| w == b"MCHP"));
    // Should contain named fields: EPEN, MHEN, TLUD, TOM_
    assert!(aml.windows(4).any(|w| w == b"EPEN"));
    assert!(aml.windows(4).any(|w| w == b"MHEN"));
    assert!(aml.windows(4).any(|w| w == b"TLUD"));

    // OpRegion opcode: ExtOpPrefix(0x5B) + OpRegionOp(0x80)
    assert!(
        aml.windows(2).any(|w| w == [0x5B, 0x80]),
        "expected OpRegion opcode"
    );
    // Field opcode: ExtOpPrefix(0x5B) + FieldOp(0x81)
    assert!(
        aml.windows(2).any(|w| w == [0x5B, 0x81]),
        "expected Field opcode"
    );
}

/// Test the dynamic _CRS pattern: named resource template, CreateDwordField
/// to get writeable handles, and arithmetic to patch it at runtime.
#[test]
fn test_dynamic_crs_method() {
    let aml: Vec<u8> = acpi_dsl! {
        // Named resource template with PCI memory region
        name("MCRS", resource_template {
            word_bus_number(0x0000u16, 0x00FFu16);
            io(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
            dword_io(0x0000u32, 0xFFFFu32);
            dword_memory(Cacheable, ReadWrite, 0x000A_0000u32, 0x000B_FFFFu32);
            dword_memory(NotCacheable, ReadWrite, 0x0000_0000u32, 0xFEBF_FFFFu32);
        });

        // _CRS method that patches the resource template based on TOLUD
        method("_CRS", 0, Serialized) {
            create_dword_field(#{fstart_acpi::aml::Path::new("MCRS")}, 0x00u32, "PMIN");
            create_dword_field(#{fstart_acpi::aml::Path::new("MCRS")}, 0x04u32, "PMAX");
            create_dword_field(#{fstart_acpi::aml::Path::new("MCRS")}, 0x08u32, "PLEN");
            // PMIN = TLUD << 27
            shl(#{fstart_acpi::aml::Path::new("PMIN")},
                #{fstart_acpi::aml::Path::new("TLUD")},
                27u32);
            // PLEN = PMAX - PMIN + 1
            subtract(#{fstart_acpi::aml::Path::new("PLEN")},
                     #{fstart_acpi::aml::Path::new("PMAX")},
                     #{fstart_acpi::aml::Path::new("PMIN")});
            add(#{fstart_acpi::aml::Path::new("PLEN")},
                #{fstart_acpi::aml::Path::new("PLEN")},
                1u32);
            ret(#{fstart_acpi::aml::Path::new("MCRS")});
        }
    };

    // Should contain MethodOp (0x14) for _CRS
    assert_eq!(aml[0], 0x08); // NameOp for MCRS
    assert!(aml.windows(4).any(|w| w == b"MCRS"));
    assert!(aml.windows(4).any(|w| w == b"_CRS"));
    assert!(aml.windows(4).any(|w| w == b"PMIN"));
    assert!(aml.windows(4).any(|w| w == b"PMAX"));
    assert!(aml.windows(4).any(|w| w == b"PLEN"));
}

/// Test I/O resource descriptors in resource templates.
#[test]
fn test_io_descriptors() {
    let aml: Vec<u8> = acpi_dsl! {
        name("_CRS", resource_template {
            io(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
            dword_io(0x0D00u32, 0xFFFFu32);
        });
    };

    assert!(aml.len() > 10);
    // IO descriptor tag is 0x47
    assert!(aml.contains(&0x47), "expected IO descriptor (0x47)");
}

/// Test Store operation in method body.
#[test]
fn test_store() {
    let aml: Vec<u8> = acpi_dsl! {
        method("TEST", 0, NotSerialized) {
            store(0x42u32, #{fstart_acpi::aml::Local(0)});
        }
    };

    assert_eq!(aml[0], 0x14); // MethodOp
    assert!(aml.windows(4).any(|w| w == b"TEST"));
    // StoreOp = 0x70
    assert!(aml.contains(&0x70), "expected StoreOp (0x70)");
}

/// Full x86 northbridge test combining MCHC device with OperationRegion,
/// Field, dynamic _CRS method, I/O descriptors, and PCI memory ranges.
#[test]
fn test_x86_northbridge_full() {
    let aml: Vec<u8> = acpi_dsl! {
        scope("\\_SB_") {
            device("PCI0") {
                name("_HID", eisa_id("PNP0A08"));
                name("_CID", eisa_id("PNP0A03"));
                name("_ADR", 0u32);

                // Memory Controller Hub device
                device("MCHC") {
                    name("_ADR", 0x0000_0000u32);
                    op_region("MCHP", PciConfig, 0x00u32, 0x100u32);
                    field("MCHP", DWordAcc, NoLock, Preserve) {
                        offset(0x40),
                        EPEN, 1,
                        , 11,
                        EPBR, 20,
                        offset(0x9c),
                        , 3,
                        TLUD, 5,
                    }
                }

                // PCI resource template
                name("MCRS", resource_template {
                    word_bus_number(0x0000u16, 0x00FFu16);
                    io(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
                    dword_memory(NotCacheable, ReadWrite, 0x0000_0000u32, 0xFEBF_FFFFu32);
                });

                method("_CRS", 0, Serialized) {
                    ret(#{fstart_acpi::aml::Path::new("MCRS")});
                }

                method("_OSC", 4, NotSerialized) {
                    ret(#{fstart_acpi::aml::Arg(3)});
                }
            }
        }
    };

    // Scope starts with ScopeOp (0x10)
    assert_eq!(aml[0], 0x10);

    // Verify key structures are present
    assert!(aml.windows(4).any(|w| w == b"PCI0"));
    assert!(aml.windows(4).any(|w| w == b"MCHC"));
    assert!(aml.windows(4).any(|w| w == b"MCHP"));
    assert!(aml.windows(4).any(|w| w == b"EPEN"));
    assert!(aml.windows(4).any(|w| w == b"TLUD"));
    assert!(aml.windows(4).any(|w| w == b"MCRS"));
    assert!(aml.windows(4).any(|w| w == b"_CRS"));
    assert!(aml.windows(4).any(|w| w == b"_OSC"));

    // OpRegion + Field opcodes present
    assert!(aml.windows(2).any(|w| w == [0x5B, 0x80])); // OpRegion
    assert!(aml.windows(2).any(|w| w == [0x5B, 0x81])); // Field

    // Should be reasonably sized (a full northbridge is ~200+ bytes)
    assert!(
        aml.len() > 100,
        "expected >100 bytes for full northbridge, got {}",
        aml.len()
    );
}
