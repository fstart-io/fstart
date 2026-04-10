//! Integration tests for the `acpi_dsl!` proc-macro.
//!
//! These tests verify that the DSL produces correct AML bytecode
//! by checking opcodes, structure, and round-trip properties.

use fstart_acpi::aml::{FieldAccessType, OpRegionSpace};
use fstart_acpi::tock_bridge::{build_multi_register_field, tock_field_entries};
use fstart_acpi::Aml;
use fstart_acpi_macros::acpi_dsl;
use tock_registers::register_bitfields;
use tock_registers::register_structs;
use tock_registers::registers::ReadWrite;

#[test]
fn test_simple_device() {
    let aml: Vec<u8> = acpi_dsl! {
        Device("COM0") {
            Name("_HID", "ARMH0011");
            Name("_UID", 0u32);
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
        Device("UAR0") {
            Name("_HID", EisaId("PNP0501"));
            Name("_UID", 0u32);
        }
    };

    assert_eq!(aml[0], 0x5B);
    assert_eq!(aml[1], 0x82);
    assert!(aml.len() > 10);
}

#[test]
fn test_device_with_resource_template() {
    let aml: Vec<u8> = acpi_dsl! {
        Device("COM0") {
            Name("_HID", "ARMH0011");
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadWrite, 0x6000_0000u32, 0x1000u32);
                Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, 33u32);
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
        Device("COM0") {
            Name("_HID", "ARMH0011");
            Name("_CRS", ResourceTemplate {
                Memory32Fixed(ReadWrite, #{uart_base}, 0x1000u32);
                Interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{uart_irq});
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
        Method("_STA", 0, NotSerialized) {
            Return(0x0Fu32);
        }
    };

    // Method starts with byte 0x14 (MethodOp)
    assert_eq!(aml[0], 0x14);
    assert!(aml.len() > 5);
}

#[test]
fn test_scope_with_devices() {
    let aml: Vec<u8> = acpi_dsl! {
        Scope("\\_SB_") {
            Device("COM0") {
                Name("_HID", "ARMH0011");
                Name("_UID", 0u32);
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
        Device("DEV0") {
            Name("_HID", "TEST0001");
            Name("_UID", 0u32);
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
        Device("MCHC") {
            Name("_ADR", 0x0000_0000u32);

            OperationRegion("MCHP", PciConfig, 0x00u32, 0x100u32);
            Field("MCHP", DWordAcc, NoLock, Preserve) {
                Offset(0x40),
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

                Offset(0x90),
                // PAM0
                , 4,
                PM0H, 2,
                , 2,
                // PAM1
                PM1L, 2,
                , 2,
                PM1H, 2,
                , 2,

                Offset(0x9c),
                // TOLUD
                , 3,
                TLUD, 5,

                Offset(0xa0),
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
        Name("MCRS", ResourceTemplate {
            WordBusNumber(0x0000u16, 0x00FFu16);
            IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
            DWordIO(0x0000u32, 0xFFFFu32);
            DWordMemory(Cacheable, ReadWrite, 0x000A_0000u32, 0x000B_FFFFu32);
            DWordMemory(NotCacheable, ReadWrite, 0x0000_0000u32, 0xFEBF_FFFFu32);
        });

        // _CRS method that patches the resource template based on TOLUD
        Method("_CRS", 0, Serialized) {
            CreateDwordField(#{fstart_acpi::aml::Path::new("MCRS")}, 0x00u32, "PMIN");
            CreateDwordField(#{fstart_acpi::aml::Path::new("MCRS")}, 0x04u32, "PMAX");
            CreateDwordField(#{fstart_acpi::aml::Path::new("MCRS")}, 0x08u32, "PLEN");
            // PMIN = TLUD << 27
            ShiftLeft(#{fstart_acpi::aml::Path::new("PMIN")},
                #{fstart_acpi::aml::Path::new("TLUD")},
                27u32);
            // PLEN = PMAX - PMIN + 1
            Subtract(#{fstart_acpi::aml::Path::new("PLEN")},
                     #{fstart_acpi::aml::Path::new("PMAX")},
                     #{fstart_acpi::aml::Path::new("PMIN")});
            Add(#{fstart_acpi::aml::Path::new("PLEN")},
                #{fstart_acpi::aml::Path::new("PLEN")},
                1u32);
            Return(#{fstart_acpi::aml::Path::new("MCRS")});
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
        Name("_CRS", ResourceTemplate {
            IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
            DWordIO(0x0D00u32, 0xFFFFu32);
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
        Method("TEST", 0, NotSerialized) {
            Store(0x42u32, #{fstart_acpi::aml::Local(0)});
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
        Scope("\\_SB_") {
            Device("PCI0") {
                Name("_HID", EisaId("PNP0A08"));
                Name("_CID", EisaId("PNP0A03"));
                Name("_ADR", 0u32);

                // Memory Controller Hub device
                Device("MCHC") {
                    Name("_ADR", 0x0000_0000u32);
                    OperationRegion("MCHP", PciConfig, 0x00u32, 0x100u32);
                    Field("MCHP", DWordAcc, NoLock, Preserve) {
                        Offset(0x40),
                        EPEN, 1,
                        , 11,
                        EPBR, 20,
                        Offset(0x9c),
                        , 3,
                        TLUD, 5,
                    }
                }

                // PCI resource template
                Name("MCRS", ResourceTemplate {
                    WordBusNumber(0x0000u16, 0x00FFu16);
                    IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
                    DWordMemory(NotCacheable, ReadWrite, 0x0000_0000u32, 0xFEBF_FFFFu32);
                });

                Method("_CRS", 0, Serialized) {
                    Return(#{fstart_acpi::aml::Path::new("MCRS")});
                }

                Method("_OSC", 4, NotSerialized) {
                    Return(#{fstart_acpi::aml::Arg(3)});
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

// -----------------------------------------------------------------------
// Complete x86 PCI host bridge DSDT -- the full coreboot equivalent.
//
// Combines every feature: tock-registers derived fields, acpi_dsl!,
// OperationRegion, Field, dynamic _CRS with CreateDwordField + ShiftLeft,
// I/O and memory ranges, _OSC.
//
// This is what an actual x86 board would produce.
// -----------------------------------------------------------------------

register_bitfields! [u32,
    /// EPBAR: Enhanced PCI Express Base Address Register.
    NB_EPBAR [
        EPEN OFFSET(0) NUMBITS(1) [],
        EPBR OFFSET(12) NUMBITS(20) []
    ],
    /// MCHBAR: Memory Controller Hub Base Address Register.
    NB_MCHBAR [
        MHEN OFFSET(0) NUMBITS(1) [],
        MHBR OFFSET(14) NUMBITS(18) []
    ],
    /// PXBAR: PCI Express Base Address Register.
    NB_PXBAR [
        PXEN OFFSET(0) NUMBITS(1) [],
        PXSZ OFFSET(1) NUMBITS(2) [],
        PXBR OFFSET(26) NUMBITS(6) []
    ],
    /// DMIBAR: Direct Media Interface Base Address Register.
    NB_DMIBAR [
        DMEN OFFSET(0) NUMBITS(1) [],
        DMBR OFFSET(12) NUMBITS(20) []
    ]
];

register_bitfields! [u8,
    /// PAM0: Programmable Attribute Map 0.
    NB_PAM0 [
        PM0H OFFSET(4) NUMBITS(2) []
    ],
    /// PAM1: Programmable Attribute Map 1.
    NB_PAM1 [
        PM1L OFFSET(0) NUMBITS(2) [],
        PM1H OFFSET(4) NUMBITS(2) []
    ],
    /// PAM2: Programmable Attribute Map 2.
    NB_PAM2 [
        PM2L OFFSET(0) NUMBITS(2) [],
        PM2H OFFSET(4) NUMBITS(2) []
    ],
    /// PAM3: Programmable Attribute Map 3.
    NB_PAM3 [
        PM3L OFFSET(0) NUMBITS(2) [],
        PM3H OFFSET(4) NUMBITS(2) []
    ],
    /// PAM4: Programmable Attribute Map 4.
    NB_PAM4 [
        PM4L OFFSET(0) NUMBITS(2) [],
        PM4H OFFSET(4) NUMBITS(2) []
    ],
    /// PAM5: Programmable Attribute Map 5.
    NB_PAM5 [
        PM5L OFFSET(0) NUMBITS(2) [],
        PM5H OFFSET(4) NUMBITS(2) []
    ],
    /// PAM6: Programmable Attribute Map 6.
    NB_PAM6 [
        PM6L OFFSET(0) NUMBITS(2) [],
        PM6H OFFSET(4) NUMBITS(2) []
    ],
    /// TOLUD: Top of Low Usable DRAM.
    NB_TOLUD [
        TLUD OFFSET(3) NUMBITS(5) []
    ]
];

register_bitfields! [u16,
    /// TOM: Top of Memory.
    NB_TOM [
        TOM_ OFFSET(0) NUMBITS(16) []
    ]
];

/// Complete x86 PCI host bridge DSDT combining tock-registers derived
/// MCH fields, resource templates, dynamic _CRS, and _OSC.
///
/// Equivalent coreboot ASL:
///
/// ```text
/// Scope (\_SB) {
///   Device (PCI0) {
///     Name (_HID, EisaId ("PNP0A08"))
///     Name (_CID, EisaId ("PNP0A03"))
///     Name (_ADR, 0x00000000)
///
///     Device (MCHC) {
///       Name(_ADR, 0x00000000)
///       OperationRegion(MCHP, PCI_Config, 0x00, 0x100)
///       Field (MCHP, DWordAcc, NoLock, Preserve) { ... }
///     }
///
///     Name (MCRS, ResourceTemplate() { ... })
///
///     Method (_CRS, 0, Serialized) {
///       CreateDwordField(MCRS, ^PM01._MIN, PMIN)
///       CreateDwordField(MCRS, ^PM01._MAX, PMAX)
///       CreateDwordField(MCRS, ^PM01._LEN, PLEN)
///       PMIN = ^MCHC.TLUD << 27
///       PLEN = PMAX - PMIN + 1
///       Return (MCRS)
///     }
///
///     Method (_OSC, 4, NotSerialized) { Return (Arg3) }
///   }
/// }
/// ```
#[test]
fn test_complete_x86_host_bridge() {
    // --- Step 1: Build MCH register fields from tock-registers ---
    let mchp = build_multi_register_field(
        "MCHP",
        OpRegionSpace::PCIConfig,
        0x00,
        0x100,
        FieldAccessType::DWord,
        &[
            (0x40, 32, &tock_field_entries::<u32, NB_EPBAR::Register>(32)),
            (
                0x44,
                32,
                &tock_field_entries::<u32, NB_MCHBAR::Register>(32),
            ),
            (0x48, 32, &tock_field_entries::<u32, NB_PXBAR::Register>(32)),
            (
                0x4C,
                32,
                &tock_field_entries::<u32, NB_DMIBAR::Register>(32),
            ),
            (0x90, 8, &tock_field_entries::<u8, NB_PAM0::Register>(8)),
            (0x91, 8, &tock_field_entries::<u8, NB_PAM1::Register>(8)),
            (0x92, 8, &tock_field_entries::<u8, NB_PAM2::Register>(8)),
            (0x93, 8, &tock_field_entries::<u8, NB_PAM3::Register>(8)),
            (0x94, 8, &tock_field_entries::<u8, NB_PAM4::Register>(8)),
            (0x95, 8, &tock_field_entries::<u8, NB_PAM5::Register>(8)),
            (0x96, 8, &tock_field_entries::<u8, NB_PAM6::Register>(8)),
            (0x9C, 8, &tock_field_entries::<u8, NB_TOLUD::Register>(8)),
            (0xA0, 16, &tock_field_entries::<u16, NB_TOM::Register>(16)),
        ],
    );

    // --- Step 2: Build the full DSDT scope ---
    //
    // Helper: Path::new is not Copy, so we use a closure to create
    // fresh instances for each #{} interpolation site.
    use fstart_acpi::aml::Path;
    let p = |s| Path::new(s);

    let aml: Vec<u8> = acpi_dsl! {
        Scope("\\_SB_") {
            Device("PCI0") {
                Name("_HID", EisaId("PNP0A08"));
                Name("_CID", EisaId("PNP0A03"));
                Name("_ADR", 0x0000_0000u32);

                // MCH device: registers derived from tock-registers
                Device("MCHC") {
                    Name("_ADR", 0x0000_0000u32);
                    #{mchp}
                }

                // Static resource template (patched at runtime by _CRS)
                Name("MCRS", ResourceTemplate {
                    // Bus numbers 0-255
                    WordBusNumber(0x0000u16, 0x00FFu16);
                    // Legacy I/O: 0x0000-0x0CF7
                    DWordIO(0x0000u32, 0x0CF7u32);
                    // PCI config space I/O port
                    IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
                    // Legacy I/O: 0x0D00-0xFFFF
                    DWordIO(0x0D00u32, 0xFFFFu32);
                    // VGA memory
                    DWordMemory(Cacheable, ReadWrite, 0x000A_0000u32, 0x000B_FFFFu32);
                    // PCI memory (placeholder range, patched by _CRS)
                    DWordMemory(NotCacheable, ReadWrite, 0x0000_0000u32, 0xFEBF_FFFFu32);
                });

                // Dynamic _CRS: reads TOLUD from MCH, patches PCI memory range
                Method("_CRS", 0, Serialized) {
                    CreateDwordField(#{p("MCRS")}, 0x00u32, "PMIN");
                    CreateDwordField(#{p("MCRS")}, 0x04u32, "PMAX");
                    CreateDwordField(#{p("MCRS")}, 0x08u32, "PLEN");
                    // PMIN = TLUD << 27
                    ShiftLeft(#{p("PMIN")}, #{p("TLUD")}, 27u32);
                    // PLEN = PMAX - PMIN + 1
                    Subtract(#{p("PLEN")}, #{p("PMAX")}, #{p("PMIN")});
                    Add(#{p("PLEN")}, #{p("PLEN")}, 1u32);
                    Return(#{p("MCRS")});
                }

                // _OSC: accept all OS capabilities
                Method("_OSC", 4, NotSerialized) {
                    Return(#{fstart_acpi::aml::Arg(3)});
                }
            }
        }
    };

    // --- Step 3: Verify the complete DSDT ---

    // Top-level structure
    assert_eq!(aml[0], 0x10, "ScopeOp"); // \_SB scope

    // All device names present
    assert!(aml.windows(4).any(|w| w == b"PCI0"), "PCI0 device");
    assert!(aml.windows(4).any(|w| w == b"MCHC"), "MCHC device");

    // MCH register field names (tock-registers derived)
    assert!(aml.windows(4).any(|w| w == b"MCHP"), "MCHP region");
    assert!(aml.windows(4).any(|w| w == b"EPEN"), "EPBAR.EPEN");
    assert!(aml.windows(4).any(|w| w == b"MHEN"), "MCHBAR.MHEN");
    assert!(aml.windows(4).any(|w| w == b"PXEN"), "PXBAR.PXEN");
    assert!(aml.windows(4).any(|w| w == b"DMEN"), "DMIBAR.DMEN");
    assert!(aml.windows(4).any(|w| w == b"PM0H"), "PAM0.PM0H");
    assert!(aml.windows(4).any(|w| w == b"PM1L"), "PAM1.PM1L");
    assert!(aml.windows(4).any(|w| w == b"PM2L"), "PAM2.PM2L");
    assert!(aml.windows(4).any(|w| w == b"PM3L"), "PAM3.PM3L");
    assert!(aml.windows(4).any(|w| w == b"PM4L"), "PAM4.PM4L");
    assert!(aml.windows(4).any(|w| w == b"PM5L"), "PAM5.PM5L");
    assert!(aml.windows(4).any(|w| w == b"PM6L"), "PAM6.PM6L");
    assert!(aml.windows(4).any(|w| w == b"TLUD"), "TOLUD.TLUD");
    assert!(aml.windows(4).any(|w| w == b"TOM_"), "TOM");

    // Resource template
    assert!(
        aml.windows(4).any(|w| w == b"MCRS"),
        "MCRS resource template"
    );
    assert!(aml.contains(&0x47), "IO descriptor (0x47)");
    assert!(aml.contains(&0x87), "DWordMemory descriptor (0x87)");
    assert!(aml.contains(&0x88), "WordBusNumber descriptor (0x88)");

    // Methods
    assert!(aml.windows(4).any(|w| w == b"_CRS"), "_CRS method");
    assert!(aml.windows(4).any(|w| w == b"_OSC"), "_OSC method");

    // Dynamic _CRS internals
    assert!(
        aml.windows(4).any(|w| w == b"PMIN"),
        "CreateDWordField PMIN"
    );
    assert!(
        aml.windows(4).any(|w| w == b"PMAX"),
        "CreateDWordField PMAX"
    );
    assert!(
        aml.windows(4).any(|w| w == b"PLEN"),
        "CreateDWordField PLEN"
    );

    // OpRegion + Field opcodes
    assert!(aml.windows(2).any(|w| w == [0x5B, 0x80]), "OpRegion opcode");
    assert!(aml.windows(2).any(|w| w == [0x5B, 0x81]), "Field opcode");

    // EISA IDs (PNP0A08, PNP0A03)
    assert!(aml.windows(4).any(|w| w == b"_HID"), "_HID");
    assert!(aml.windows(4).any(|w| w == b"_CID"), "_CID");

    // Size sanity: a real x86 northbridge DSDT is 500+ bytes
    assert!(
        aml.len() > 400,
        "expected >400 bytes for complete host bridge, got {}",
        aml.len()
    );
}

// -----------------------------------------------------------------------
// Same northbridge, but using register_structs! for automatic offsets.
//
// register_structs! gives us the byte offset of each register via
// core::mem::offset_of!.  The tock_acpi_field! macro reads those
// offsets + the register bitfield metadata to produce the ACPI Field.
// No hardcoded 0x40, 0x44, 0x90, etc.
// -----------------------------------------------------------------------

register_structs! {
    /// MCH PCI config register block (0x00..0x100).
    ///
    /// The offsets here are the single source of truth -- they're used
    /// by the firmware for MMIO access AND by the ACPI bridge to
    /// produce OperationRegion + Field definitions.
    MchPciConfig {
        (0x000 => _pad0),
        (0x040 => pub epbar: ReadWrite<u32, NB_EPBAR::Register>),
        (0x044 => pub mchbar: ReadWrite<u32, NB_MCHBAR::Register>),
        (0x048 => pub pxbar: ReadWrite<u32, NB_PXBAR::Register>),
        (0x04C => pub dmibar: ReadWrite<u32, NB_DMIBAR::Register>),
        (0x050 => _pad1),
        (0x090 => pub pam0: ReadWrite<u8, NB_PAM0::Register>),
        (0x091 => pub pam1: ReadWrite<u8, NB_PAM1::Register>),
        (0x092 => pub pam2: ReadWrite<u8, NB_PAM2::Register>),
        (0x093 => pub pam3: ReadWrite<u8, NB_PAM3::Register>),
        (0x094 => pub pam4: ReadWrite<u8, NB_PAM4::Register>),
        (0x095 => pub pam5: ReadWrite<u8, NB_PAM5::Register>),
        (0x096 => pub pam6: ReadWrite<u8, NB_PAM6::Register>),
        (0x097 => _pad2),
        (0x09C => pub tolud: ReadWrite<u8, NB_TOLUD::Register>),
        (0x09D => _pad3),
        (0x0A0 => pub tom: ReadWrite<u16, NB_TOM::Register>),
        (0x0A2 => _pad4),
        (0x100 => @END),
    }
}

/// Same as test_complete_x86_host_bridge but with offsets derived from
/// register_structs! via the tock_acpi_field! macro.
///
/// Compare:
///
/// **Before** (manual offsets):
/// ```ignore
/// build_multi_register_field("MCHP", PCIConfig, 0x00, 0x100, DWordAcc, &[
///     (0x40, 32, &tock_field_entries::<u32, NB_EPBAR::Register>(32)),
///     (0x44, 32, &tock_field_entries::<u32, NB_MCHBAR::Register>(32)),
///     ...
/// ]);
/// ```
///
/// **After** (struct-derived offsets):
/// ```ignore
/// tock_acpi_field!(MchPciConfig, "MCHP", PCIConfig, DWord, [
///     epbar: u32, NB_EPBAR,
///     mchbar: u32, NB_MCHBAR,
///     ...
/// ]);
/// ```
#[test]
fn test_x86_host_bridge_register_structs() {
    // --- tock_acpi_field! derives offsets from MchPciConfig layout ---
    let mchp = fstart_acpi::tock_acpi_field!(
        MchPciConfig,
        "MCHP",
        PCIConfig,
        DWord,
        [epbar, mchbar, pxbar, dmibar, pam0, pam1, pam2, pam3, pam4, pam5, pam6, tolud, tom,]
    );

    let aml: Vec<u8> = acpi_dsl! {
        Device("MCHC") {
            Name("_ADR", 0x0000_0000u32);
            #{mchp}
        }
    };

    // Verify same output as the manual-offset version.
    assert_eq!(aml[0], 0x5B); // ExtOpPrefix
    assert_eq!(aml[1], 0x82); // DeviceOp
    assert!(aml.windows(4).any(|w| w == b"MCHC"));
    assert!(aml.windows(4).any(|w| w == b"MCHP"));

    // All tock-derived field names present.
    assert!(aml.windows(4).any(|w| w == b"EPEN"));
    assert!(aml.windows(4).any(|w| w == b"MHEN"));
    assert!(aml.windows(4).any(|w| w == b"PXEN"));
    assert!(aml.windows(4).any(|w| w == b"DMEN"));
    assert!(aml.windows(4).any(|w| w == b"PM0H"));
    assert!(aml.windows(4).any(|w| w == b"PM1L"));
    assert!(aml.windows(4).any(|w| w == b"TLUD"));
    assert!(aml.windows(4).any(|w| w == b"TOM_"));

    // OpRegion + Field opcodes.
    assert!(aml.windows(2).any(|w| w == [0x5B, 0x80]));
    assert!(aml.windows(2).any(|w| w == [0x5B, 0x81]));

    // Verify the OpRegion+Field output is byte-identical to manual offsets.
    // Rebuild since `mchp` was moved into acpi_dsl!.
    let mchp_again = fstart_acpi::tock_acpi_field!(
        MchPciConfig,
        "MCHP",
        PCIConfig,
        DWord,
        [epbar, mchbar, pxbar, dmibar, pam0, pam1, pam2, pam3, pam4, pam5, pam6, tolud, tom,]
    );
    let mchp_manual = build_multi_register_field(
        "MCHP",
        OpRegionSpace::PCIConfig,
        0x00,
        0x100,
        FieldAccessType::DWord,
        &[
            (0x40, 32, &tock_field_entries::<u32, NB_EPBAR::Register>(32)),
            (
                0x44,
                32,
                &tock_field_entries::<u32, NB_MCHBAR::Register>(32),
            ),
            (0x48, 32, &tock_field_entries::<u32, NB_PXBAR::Register>(32)),
            (
                0x4C,
                32,
                &tock_field_entries::<u32, NB_DMIBAR::Register>(32),
            ),
            (0x90, 8, &tock_field_entries::<u8, NB_PAM0::Register>(8)),
            (0x91, 8, &tock_field_entries::<u8, NB_PAM1::Register>(8)),
            (0x92, 8, &tock_field_entries::<u8, NB_PAM2::Register>(8)),
            (0x93, 8, &tock_field_entries::<u8, NB_PAM3::Register>(8)),
            (0x94, 8, &tock_field_entries::<u8, NB_PAM4::Register>(8)),
            (0x95, 8, &tock_field_entries::<u8, NB_PAM5::Register>(8)),
            (0x96, 8, &tock_field_entries::<u8, NB_PAM6::Register>(8)),
            (0x9C, 8, &tock_field_entries::<u8, NB_TOLUD::Register>(8)),
            (0xA0, 16, &tock_field_entries::<u16, NB_TOM::Register>(16)),
        ],
    );

    let mut manual_bytes = Vec::new();
    mchp_manual.to_aml_bytes(&mut manual_bytes);

    // Both should produce identical AML for the same register set.
    let mut macro_field_bytes = Vec::new();
    mchp_again.to_aml_bytes(&mut macro_field_bytes);

    assert_eq!(
        macro_field_bytes, manual_bytes,
        "tock_acpi_field! output must match manual build_multi_register_field"
    );
}

// =======================================================================
// ASL 2.0 expression and control flow tests
// =======================================================================

/// Test If/Else control flow with comparison operators.
#[test]
fn test_if_else() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("TEST", 1, NotSerialized) {
            If (Arg0 == 0u32) {
                Return(1u32);
            } Else {
                Return(0u32);
            }
        }
    };

    assert_eq!(aml[0], 0x14); // MethodOp
    assert!(aml.windows(4).any(|w| w == b"TEST"));
    // IfOp = 0xA0
    assert!(aml.contains(&0xA0), "expected IfOp (0xA0)");
    // ElseOp = 0xA1
    assert!(aml.contains(&0xA1), "expected ElseOp (0xA1)");
}

/// Test If without Else.
#[test]
fn test_if_no_else() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("CHK_", 1, NotSerialized) {
            If (Arg0 > 10u32) {
                Return(1u32);
            }
            Return(0u32);
        }
    };

    assert_eq!(aml[0], 0x14); // MethodOp
    assert!(aml.contains(&0xA0), "expected IfOp");
    // No ElseOp should appear
    assert!(!aml.contains(&0xA1), "should not have ElseOp");
}

/// Test While loop with Break.
#[test]
fn test_while_loop() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("POLL", 0, NotSerialized) {
            Local0 = 100u32;
            While (Local0 > 0u32) {
                Local0--;
            }
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14); // MethodOp
    assert!(aml.windows(4).any(|w| w == b"POLL"));
    // WhileOp = 0xA2
    assert!(aml.contains(&0xA2), "expected WhileOp (0xA2)");
    // StoreOp = 0x70 (for Local0 = 100u32)
    assert!(aml.contains(&0x70), "expected StoreOp (0x70)");
    // DecrementOp = 0x76
    assert!(aml.contains(&0x76), "expected DecrementOp (0x76)");
}

/// Test While with Break.
#[test]
fn test_while_break() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("BRK_", 0, NotSerialized) {
            Local0 = 0u32;
            While (One) {
                If (Local0 == 10u32) {
                    Break;
                }
                Local0++;
            }
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14);
    // BreakOp = 0xA5
    assert!(aml.contains(&0xA5), "expected BreakOp (0xA5)");
    // IncrementOp = 0x75
    assert!(aml.contains(&0x75), "expected IncrementOp (0x75)");
}

/// Test ASL 2.0 assignment syntax with arithmetic.
#[test]
fn test_assign_arithmetic() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("MATH", 2, NotSerialized) {
            Local0 = Arg0 + Arg1;
            Local1 = Arg0 - Arg1;
            Local2 = Arg0 << 4u32;
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14);
    // Should have Store ops
    assert!(aml.contains(&0x70), "expected StoreOp");
}

/// Test bitwise operators.
#[test]
fn test_bitwise_operators() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("BITS", 2, NotSerialized) {
            Local0 = Arg0 & Arg1;
            Local1 = Arg0 | Arg1;
            Local2 = Arg0 ^ Arg1;
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14);
    assert!(aml.windows(4).any(|w| w == b"BITS"));
}

/// Test logical operators.
#[test]
fn test_logical_operators() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("LOGC", 2, NotSerialized) {
            If (Arg0 == 1u32 && Arg1 == 2u32) {
                Return(1u32);
            }
            If (Arg0 == 0u32 || Arg1 == 0u32) {
                Return(0u32);
            }
            Return(2u32);
        }
    };

    assert_eq!(aml[0], 0x14);
    // LAndOp = 0x90
    assert!(aml.contains(&0x90), "expected LAndOp (0x90)");
    // LOrOp = 0x91
    assert!(aml.contains(&0x91), "expected LOrOp (0x91)");
}

/// Test logical NOT.
#[test]
fn test_logical_not() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("LNOT", 1, NotSerialized) {
            If (!Arg0) {
                Return(1u32);
            }
            Return(0u32);
        }
    };

    assert_eq!(aml[0], 0x14);
    // LNotOp = 0x92
    assert!(aml.contains(&0x92), "expected LNotOp (0x92)");
}

/// Test Notify statement.
#[test]
fn test_notify() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("NTFY", 0, NotSerialized) {
            Notify(DEV0, 0x80u32);
        }
    };

    assert_eq!(aml[0], 0x14);
    // NotifyOp = 0x86
    assert!(aml.contains(&0x86), "expected NotifyOp (0x86)");
}

/// Test Sleep statement.
#[test]
fn test_sleep() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("SLP_", 0, NotSerialized) {
            Sleep(100u32);
        }
    };

    assert_eq!(aml[0], 0x14);
    // Sleep = ExtOpPrefix(0x5B) + SleepOp(0x22)
    assert!(
        aml.windows(2).any(|w| w == [0x5B, 0x22]),
        "expected Sleep opcode"
    );
}

/// Test Stall statement.
#[test]
fn test_stall() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("STL_", 0, NotSerialized) {
            Stall(50u8);
        }
    };

    assert_eq!(aml[0], 0x14);
    // Stall = ExtOpPrefix(0x5B) + StallOp(0x21)
    assert!(
        aml.windows(2).any(|w| w == [0x5B, 0x21]),
        "expected Stall opcode"
    );
}

/// Test ToUUID in expression context.
#[test]
fn test_touuid_expr() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("_OSC", 4, NotSerialized) {
            If (Arg0 == ToUUID("33DB4D5B-1FF7-401C-9657-7441C03DD766")) {
                Return(Arg3);
            }
            Return(Arg3);
        }
    };

    assert_eq!(aml[0], 0x14);
    assert!(aml.windows(4).any(|w| w == b"_OSC"));
    // Should have IfOp
    assert!(aml.contains(&0xA0), "expected IfOp");
}

/// Complete _OSC method test -- the canonical ASL 2.0 example.
///
/// Tests expression parsing, If/Else, CreateDwordField, bitwise
/// assignment, and Return with expression argument.
#[test]
fn test_osc_method() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("_OSC", 4, NotSerialized) {
            CreateDwordField(#{fstart_acpi::aml::Arg(3)}, 0u32, "CDW1");

            If (Arg0 == ToUUID("33DB4D5B-1FF7-401C-9657-7441C03DD766")) {
                CreateDwordField(#{fstart_acpi::aml::Arg(3)}, 4u32, "CDW2");
                CDW2 = CDW2 & 0x1Du32;
            } Else {
                CDW1 = CDW1 | 4u32;
            }

            Return(Arg3);
        }
    };

    assert_eq!(aml[0], 0x14); // MethodOp
    assert!(aml.windows(4).any(|w| w == b"_OSC"));
    assert!(aml.windows(4).any(|w| w == b"CDW1"));
    assert!(aml.windows(4).any(|w| w == b"CDW2"));
    assert!(aml.contains(&0xA0), "expected IfOp");
    assert!(aml.contains(&0xA1), "expected ElseOp");
    assert!(aml.contains(&0x70), "expected StoreOp");
}

/// Test increment/decrement with function-call syntax.
#[test]
fn test_increment_decrement_call() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("INCD", 0, NotSerialized) {
            Local0 = 0u32;
            Increment(Local0);
            Decrement(Local0);
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14);
    assert!(aml.contains(&0x75), "expected IncrementOp");
    assert!(aml.contains(&0x76), "expected DecrementOp");
}

/// Test ASL 2.0 assignment where the value is a comparison
/// (stores boolean result).
#[test]
fn test_assign_comparison() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("CMP_", 2, NotSerialized) {
            Local0 = Arg0 >= Arg1;
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14);
    assert!(aml.contains(&0x70), "expected StoreOp");
}

/// Verify ASL 2.0 assignment produces the same AML as legacy Store.
#[test]
fn test_assign_matches_store() {
    // ASL 2.0 style
    let aml_2: Vec<u8> = acpi_dsl! {
        Method("TST1", 0, NotSerialized) {
            Local0 = 42u32;
        }
    };

    // Legacy style
    let aml_1: Vec<u8> = acpi_dsl! {
        Method("TST1", 0, NotSerialized) {
            Store(42u32, #{fstart_acpi::aml::Local(0)});
        }
    };

    assert_eq!(aml_2, aml_1, "ASL 2.0 assignment should match legacy Store");
}

/// Test postfix increment `Local0++` and decrement `Local0--`.
#[test]
fn test_postfix_inc_dec() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("POST", 0, NotSerialized) {
            Local0 = 5u32;
            Local0++;
            Local0--;
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14);
    assert!(aml.contains(&0x75), "expected IncrementOp");
    assert!(aml.contains(&0x76), "expected DecrementOp");
}

/// Test Zero, One, Ones constants in expressions.
#[test]
fn test_aml_constants() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("CON_", 0, NotSerialized) {
            Local0 = Zero;
            Local1 = One;
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14);
    // Zero encodes as 0x00, One as 0x01 -- but these are also used
    // by NullTarget and other structures, so just check StoreOp is present
    assert!(aml.contains(&0x70), "expected StoreOp");
}

/// Test complex nested expression: `(a + b) * c`.
#[test]
fn test_nested_arithmetic() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("NEST", 3, NotSerialized) {
            Local0 = (Arg0 + Arg1) * Arg2;
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14);
    assert!(aml.contains(&0x70), "expected StoreOp");
}

/// ElseIf chain test.
#[test]
fn test_elseif_chain() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("ELIF", 1, NotSerialized) {
            If (Arg0 == 0u32) {
                Return(0u32);
            } Else If (Arg0 == 1u32) {
                Return(1u32);
            } Else {
                Return(2u32);
            }
        }
    };

    assert_eq!(aml[0], 0x14);
    assert!(aml.windows(4).any(|w| w == b"ELIF"));
    // Should have two IfOps (outer + inner ElseIf)
    let if_count = aml.iter().filter(|&&b| b == 0xA0).count();
    assert!(if_count >= 2, "expected at least 2 IfOps, got {}", if_count);
}

/// Test shift right operator.
#[test]
fn test_shift_right() {
    let aml: Vec<u8> = acpi_dsl! {
        Method("SHR_", 1, NotSerialized) {
            Local0 = Arg0 >> 4u32;
            Return(Local0);
        }
    };

    assert_eq!(aml[0], 0x14);
    assert!(aml.contains(&0x70), "expected StoreOp");
}

/// Full dynamic _CRS using ASL 2.0 assignment syntax (replaces
/// the legacy test_dynamic_crs_method with C-style operators).
#[test]
fn test_dynamic_crs_asl2() {
    use fstart_acpi::aml::Path;
    let p = |s| Path::new(s);

    let aml: Vec<u8> = acpi_dsl! {
        Name("MCRS", ResourceTemplate {
            WordBusNumber(0x0000u16, 0x00FFu16);
            IO(0x0CF8u16, 0x0CF8u16, 0x01u8, 0x08u8);
            DWordIO(0x0000u32, 0xFFFFu32);
            DWordMemory(Cacheable, ReadWrite, 0x000A_0000u32, 0x000B_FFFFu32);
            DWordMemory(NotCacheable, ReadWrite, 0x0000_0000u32, 0xFEBF_FFFFu32);
        });

        Method("_CRS", 0, Serialized) {
            CreateDwordField(#{p("MCRS")}, 0x00u32, "PMIN");
            CreateDwordField(#{p("MCRS")}, 0x04u32, "PMAX");
            CreateDwordField(#{p("MCRS")}, 0x08u32, "PLEN");

            PMIN = TLUD << 27u32;
            PLEN = PMAX - PMIN;
            PLEN = PLEN + 1u32;

            Return(#{p("MCRS")});
        }
    };

    assert_eq!(aml[0], 0x08); // NameOp for MCRS
    assert!(aml.windows(4).any(|w| w == b"MCRS"));
    assert!(aml.windows(4).any(|w| w == b"_CRS"));
    assert!(aml.windows(4).any(|w| w == b"PMIN"));
    assert!(aml.windows(4).any(|w| w == b"PMAX"));
    assert!(aml.windows(4).any(|w| w == b"PLEN"));
    // Should have StoreOps for the assignments
    assert!(aml.contains(&0x70), "expected StoreOp");
}
