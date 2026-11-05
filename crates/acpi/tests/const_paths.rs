//! `#{const path N EXPR}` and `#{const name EXPR}` encode exactly like the
//! literal paths they replace, including in generic code.
use fstart_acpi::{AcpiPath, PcNamespace};
use fstart_acpi_macros::acpi_dsl;

const PCI0: AcpiPath = AcpiPath::SB.child("PCI0");
const LPC: AcpiPath = PCI0.child("LPCB");

struct Chipset;
impl PcNamespace for Chipset {
    const LPC: AcpiPath = LPC;
    const IGD: AcpiPath = PCI0.child("GFX0");
}

fn bytes(fragment: impl Into<Vec<u8>>) -> Vec<u8> {
    fragment.into()
}

#[test]
fn const_paths_match_literals_at_every_depth() {
    assert_eq!(
        bytes(acpi_dsl! { Scope(#{const path 0 AcpiPath::ROOT}) { Name("A", 1u32); } }),
        bytes(acpi_dsl! { Scope("\\") { Name("A", 1u32); } }),
    );
    assert_eq!(
        bytes(acpi_dsl! { Scope(#{const path 1 AcpiPath::SB}) { Name("A", 1u32); } }),
        bytes(acpi_dsl! { Scope("\\_SB_") { Name("A", 1u32); } }),
    );
    assert_eq!(
        bytes(acpi_dsl! { Scope(#{const path 2 PCI0}) { Name("A", 1u32); } }),
        bytes(acpi_dsl! { Scope("\\_SB_.PCI0") { Name("A", 1u32); } }),
    );
    assert_eq!(
        bytes(acpi_dsl! {
            Method("TEST", 0, NotSerialized) {
                Notify(#{const path 4 LPC.child("EC")}, 0x80u32);
                Local0 = #{const path 5 LPC.child("EC").child("TMP0")};
                #{const path 5 LPC.child("EC").child("FANE")}(1u32);
            }
        }),
        bytes(acpi_dsl! {
            Method("TEST", 0, NotSerialized) {
                Notify(#{const "\\_SB_.PCI0.LPCB.EC__"}, 0x80u32);
                Local0 = #{const "\\_SB_.PCI0.LPCB.EC__.TMP0"};
                #{const "\\_SB_.PCI0.LPCB.EC__.FANE"}(1u32);
            }
        }),
    );
}

#[test]
fn const_names_match_literals() {
    const LPC_NAME: &str = "LPCB";
    assert_eq!(
        bytes(acpi_dsl! { Device(#{const name LPC_NAME}) { Name("_ADR", 0x001F0000u32); } }),
        bytes(acpi_dsl! { Device("LPCB") { Name("_ADR", 0x001F0000u32); } }),
    );
    assert_eq!(
        bytes(acpi_dsl! { Device(#{const name "EC"}) { } }),
        bytes(acpi_dsl! { Device("EC__") { } }),
    );
}

/// Paths from a generic parameter, with and without runtime operands.
fn notify_igd<N: PcNamespace>(value: u32) -> (Vec<u8>, Vec<u8>) {
    (
        bytes(acpi_dsl! { Notify(#{const path 3 N::IGD}, 0x82u32); }),
        bytes(acpi_dsl! { Notify(#{const path 3 N::IGD}, #{dword value}); }),
    )
}

#[test]
fn generic_paths_resolve_per_namespace() {
    let (fixed, runtime) = notify_igd::<Chipset>(0x82);
    assert_eq!(
        fixed,
        bytes(acpi_dsl! { Notify(#{const "\\_SB_.PCI0.GFX0"}, 0x82u32); })
    );
    let literal = bytes(acpi_dsl! { Notify(#{const "\\_SB_.PCI0.GFX0"}, #{dword 0x82u32}); });
    assert_eq!(runtime, literal);
}

/// Integer consts in modules named like the new keywords keep their meaning.
mod name {
    pub const VALUE: u32 = 0x11;
}
mod path {
    pub const VALUE: u32 = 0x22;
}

#[test]
fn keyword_named_modules_stay_integer_expressions() {
    assert_eq!(
        bytes(acpi_dsl! { Name("TEST", #{const dword name::VALUE}); }),
        bytes(acpi_dsl! { Name("TEST", #{const dword 0x11u32}); }),
    );
    assert_eq!(
        bytes(acpi_dsl! { Name("TEST", #{const dword path::VALUE}); }),
        bytes(acpi_dsl! { Name("TEST", #{const dword 0x22u32}); }),
    );
}
