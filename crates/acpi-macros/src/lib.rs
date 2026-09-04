//! Proc-macro crate for the `acpi_dsl!` AML inline-assembly macro.
//!
//! The parser and validator run in the compiler process. The backend encodes
//! every opcode, integer, `NameString`, resource descriptor, and `PkgLength`
//! there as well, so the expanded program contains a const
//! [`fstart_acpi::AmlFragment`] value. Runtime expressions are paired with that
//! static fragment in a [`fstart_acpi::BoundAmlFragment`].
//!
//! Runtime operands must declare their fixed AML width: `#{byte EXPR}`,
//! `#{word EXPR}`, `#{dword EXPR}`, or `#{qword EXPR}`. The macro evaluates each
//! expression once and binds it to the corresponding fixup in source order.
//! Plain `#{EXPR}` is deliberately rejected. `#{const EXPR}` accepts integer
//! literals with their shortest encoding and string literals where a path is
//! expected. Named const integer expressions use an explicit fixed width, for
//! example `#{const dword MMIO_BASE + 4}`. Const string variables are not
//! supported because their length would change the surrounding AML layout.

extern crate proc_macro;

mod emit;
mod parse;
mod validate;

use proc_macro::TokenStream;

/// Compile Rust-flavored ASL directly into an AML fragment.
///
/// Literal-only and const-only fragments are const-evaluable and can be placed
/// in a `static`. Literal-only fragments implement `Aml`; fragments with runtime
/// operands return a bound fragment whose `emit` method needs no operand array.
///
/// ```ignore
/// use fstart_acpi_macros::acpi_dsl;
///
/// const UART_BASE: u32 = 0x6000_0000;
/// static UART: fstart_acpi::AmlFragment<10, 0> = acpi_dsl! {
///     Name("UBAS", #{const dword UART_BASE});
/// };
///
/// let irq = 33u32;
/// let runtime = acpi_dsl! { Name("UIRQ", #{dword irq}); };
/// let mut bytes = Vec::new();
/// runtime.emit(&mut bytes);
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
