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
