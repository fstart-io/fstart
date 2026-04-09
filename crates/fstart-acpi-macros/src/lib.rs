//! Proc-macro crate for the `acpi_dsl!` macro.
//!
//! Provides a Rust-flavored ASL DSL that compiles to `fstart_acpi`
//! builder calls.  The macro validates ACPI names and argument counts
//! at compile time and emits `let`-binding chains in leaf-to-root
//! order (required because `acpi_tables` uses `&dyn Aml` references
//! that must outlive their parents).
//!
//! # Supported constructs (Phase 2 core subset)
//!
//! - `scope("path") { ... }` -- ACPI Scope
//! - `device("NAME") { ... }` -- ACPI Device
//! - `name("_HID", value)` -- ACPI Name object
//! - `method("NAME", argc, Serialized|NotSerialized) { ... }` -- Method
//! - `ret(value)` -- Return statement
//! - `eisa_id("PNP0501")` -- EISA ID encoding
//! - `resource_template { ... }` -- ResourceTemplate
//! - `memory_32_fixed(ReadWrite|ReadOnly, base, size)` -- Memory32Fixed
//! - `interrupt(consumer, level, polarity, sharing, irq)` -- Interrupt
//! - `#{rust_expr}` -- Interpolation of Rust expressions

extern crate proc_macro;

mod emit;
mod parse;
mod validate;

use proc_macro::TokenStream;

/// ACPI DSL macro -- transforms Rust-flavored ASL into `fstart_acpi` builder calls.
///
/// Returns a `Vec<u8>` containing the serialized AML bytes.
///
/// # Example
///
/// ```ignore
/// use fstart_acpi_macros::acpi_dsl;
///
/// let uart_base: u64 = 0x6000_0000;
/// let uart_irq: u32 = 33;
///
/// let aml_bytes: Vec<u8> = acpi_dsl! {
///     device("COM0") {
///         name("_HID", "ARMH0011");
///         name("_UID", 0u32);
///         name("_CRS", resource_template {
///             memory_32_fixed(ReadWrite, #{uart_base}, 0x1000u32);
///             interrupt(ResourceConsumer, Level, ActiveHigh, Exclusive, #{uart_irq});
///         });
///     }
/// };
/// ```
#[proc_macro]
pub fn acpi_dsl(input: TokenStream) -> TokenStream {
    let items = match parse::parse_dsl(input.into()) {
        Ok(items) => items,
        Err(e) => return e.to_compile_error().into(),
    };

    if let Err(e) = validate::validate_items(&items) {
        return e.to_compile_error().into();
    }

    emit::emit_items(&items).into()
}
