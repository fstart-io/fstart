//! Code emission for the `acpi_dsl!` macro.
//!
//! Transforms the parsed AST into `proc_macro2::TokenStream` that
//! constructs `fstart_acpi` builder types and serializes them.
//!
//! The key challenge is that `acpi_tables` uses `&dyn Aml` references
//! that must outlive their parents.  We generate `let` bindings in
//! leaf-to-root order, accumulating variable names, then assemble the
//! tree at the end.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};

use crate::parse::{CacheableKind, DslItem, DslValue, NameOrInterp, ResourceDesc};

/// Counter for unique variable names within a macro invocation.
struct VarGen {
    counter: usize,
}

impl VarGen {
    fn new() -> Self {
        Self { counter: 0 }
    }

    fn next(&mut self, prefix: &str) -> proc_macro2::Ident {
        let id = format_ident!("__acpi_{}_{}", prefix, self.counter);
        self.counter += 1;
        id
    }
}

/// Emit the top-level code for a list of DSL items.
///
/// Returns a block expression that evaluates to `Vec<u8>` containing
/// the serialized AML.
pub fn emit_items(items: &[DslItem]) -> TokenStream {
    let mut gen = VarGen::new();
    let mut bindings = TokenStream::new();
    let mut child_refs: Vec<proc_macro2::Ident> = Vec::new();

    for item in items {
        let (binding, var) = emit_item(item, &mut gen);
        bindings.extend(binding);
        child_refs.push(var);
    }

    // If there's exactly one item, serialize it directly.
    // If multiple, they need to be collected.
    if child_refs.len() == 1 {
        let var = &child_refs[0];
        quote! {
            {
                extern crate alloc;
                use alloc::vec::Vec;
                use fstart_acpi::Aml;

                #bindings

                let mut __acpi_out = Vec::new();
                #var.to_aml_bytes(&mut __acpi_out);
                __acpi_out
            }
        }
    } else {
        quote! {
            {
                extern crate alloc;
                use alloc::vec::Vec;
                use fstart_acpi::Aml;

                #bindings

                let mut __acpi_out = Vec::new();
                #(#child_refs.to_aml_bytes(&mut __acpi_out);)*
                __acpi_out
            }
        }
    }
}

/// Emit bindings for a single DSL item, returning the binding code
/// and the variable name holding the built AML object.
fn emit_item(item: &DslItem, gen: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    match item {
        DslItem::Scope { path, children, .. } => emit_scope(path, children, gen),
        DslItem::Device { name, children, .. } => emit_device(name, children, gen),
        DslItem::Name { name, value, .. } => emit_name(name, value, gen),
        DslItem::Method {
            name,
            argc,
            serialized,
            body,
            ..
        } => emit_method(name, *argc, *serialized, body, gen),
        DslItem::Return { value, .. } => emit_return(value, gen),
    }
}

/// Emit a token stream for a NameOrInterp value.
fn emit_name_or_interp(name: &NameOrInterp) -> TokenStream {
    match name {
        NameOrInterp::Literal(s) => quote! { #s },
        NameOrInterp::Interpolation(expr) => quote! { #expr },
    }
}

fn emit_scope(
    path: &NameOrInterp,
    children: &[DslItem],
    gen: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut child_vars: Vec<proc_macro2::Ident> = Vec::new();

    for child in children {
        let (binding, var) = emit_item(child, gen);
        bindings.extend(binding);
        child_vars.push(var);
    }

    let var = gen.next("scope");
    let refs: Vec<_> = child_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();

    let path_expr = emit_name_or_interp(path);
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::Scope::new(
            fstart_acpi::aml::Path::new(#path_expr),
            alloc::vec![#(#refs),*],
        );
    });

    (bindings, var)
}

fn emit_device(
    name: &NameOrInterp,
    children: &[DslItem],
    gen: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut child_vars: Vec<proc_macro2::Ident> = Vec::new();

    for child in children {
        let (binding, var) = emit_item(child, gen);
        bindings.extend(binding);
        child_vars.push(var);
    }

    let var = gen.next("dev");
    let refs: Vec<_> = child_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();

    let name_expr = emit_name_or_interp(name);
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::Device::new(
            #name_expr.into(),
            alloc::vec![#(#refs),*],
        );
    });

    (bindings, var)
}

fn emit_name(name: &str, value: &DslValue, gen: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (val_binding, val_var) = emit_value(value, gen);
    bindings.extend(val_binding);

    let var = gen.next("name");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::Name::new(#name.into(), &#val_var);
    });

    (bindings, var)
}

fn emit_method(
    name: &str,
    argc: u8,
    serialized: bool,
    body: &[DslItem],
    gen: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut body_vars: Vec<proc_macro2::Ident> = Vec::new();

    for item in body {
        let (binding, var) = emit_item(item, gen);
        bindings.extend(binding);
        body_vars.push(var);
    }

    let var = gen.next("method");
    let refs: Vec<_> = body_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();

    bindings.extend(quote! {
        let #var = fstart_acpi::aml::Method::new(
            #name.into(),
            #argc,
            #serialized,
            alloc::vec![#(#refs),*],
        );
    });

    (bindings, var)
}

fn emit_return(value: &DslValue, gen: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (val_binding, val_var) = emit_value(value, gen);
    bindings.extend(val_binding);

    let var = gen.next("ret");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::Return::new(&#val_var);
    });

    (bindings, var)
}

/// Emit bindings for a value, returning the binding code and variable name.
fn emit_value(value: &DslValue, gen: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    match value {
        DslValue::StringLit(s) => {
            let var = gen.next("str");
            let binding = quote! {
                let #var: &str = #s;
            };
            (binding, var)
        }
        DslValue::IntLit(tokens) => {
            let var = gen.next("int");
            let binding = quote! {
                let #var = #tokens;
            };
            (binding, var)
        }
        DslValue::EisaId(id) => {
            let var = gen.next("eisa");
            let binding = quote! {
                let #var = fstart_acpi::aml::EISAName::new(#id);
            };
            (binding, var)
        }
        DslValue::Package(elements) => emit_package(elements, gen),
        DslValue::ResourceTemplate(descs) => emit_resource_template(descs, gen),
        DslValue::Interpolation(expr) => {
            let var = gen.next("expr");
            let binding = quote! {
                let #var = #expr;
            };
            (binding, var)
        }
    }
}

fn emit_package(elements: &[DslValue], gen: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut elem_vars: Vec<proc_macro2::Ident> = Vec::new();

    for elem in elements {
        let (binding, var) = emit_value(elem, gen);
        bindings.extend(binding);
        elem_vars.push(var);
    }

    let var = gen.next("pkg");
    let refs: Vec<_> = elem_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();

    bindings.extend(quote! {
        let #var = fstart_acpi::aml::Package::new(alloc::vec![#(#refs),*]);
    });

    (bindings, var)
}

fn emit_resource_template(
    descs: &[ResourceDesc],
    gen: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut desc_vars: Vec<proc_macro2::Ident> = Vec::new();

    for desc in descs {
        let (binding, var) = emit_resource_desc(desc, gen);
        bindings.extend(binding);
        desc_vars.push(var);
    }

    let var = gen.next("rt");
    let refs: Vec<_> = desc_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();

    bindings.extend(quote! {
        let #var = fstart_acpi::aml::ResourceTemplate::new(alloc::vec![#(#refs),*]);
    });

    (bindings, var)
}

fn emit_resource_desc(desc: &ResourceDesc, gen: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    match desc {
        ResourceDesc::Memory32Fixed {
            read_write,
            base,
            size,
        } => {
            let mut bindings = TokenStream::new();
            let (base_binding, base_var) = emit_value(base, gen);
            let (size_binding, size_var) = emit_value(size, gen);
            bindings.extend(base_binding);
            bindings.extend(size_binding);

            let var = gen.next("mem");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::Memory32Fixed::new(
                    #read_write,
                    #base_var as u32,
                    #size_var as u32,
                );
            });
            (bindings, var)
        }
        ResourceDesc::Interrupt {
            consumer,
            level,
            active_high,
            exclusive,
            irq,
        } => {
            let mut bindings = TokenStream::new();
            let (irq_binding, irq_var) = emit_value(irq, gen);
            bindings.extend(irq_binding);

            // Interrupt::new(consumer, level, active_high, !exclusive, irq)
            // Note: acpi_tables Interrupt::new takes `shared` not `exclusive`
            let shared = !exclusive;

            let var = gen.next("irq");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::Interrupt::new(
                    #consumer,
                    #level,
                    #active_high,
                    #shared,
                    #irq_var as u32,
                );
            });
            (bindings, var)
        }
        ResourceDesc::WordBusNumber { start, end } => {
            let mut bindings = TokenStream::new();
            let (s_bind, s_var) = emit_value(start, gen);
            let (e_bind, e_var) = emit_value(end, gen);
            bindings.extend(s_bind);
            bindings.extend(e_bind);

            let var = gen.next("bus");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::AddressSpace::<u16>::new_bus_number(
                    #s_var as u16,
                    #e_var as u16,
                );
            });
            (bindings, var)
        }
        ResourceDesc::DWordMemory {
            cacheable,
            read_write,
            base,
            end,
        } => {
            let mut bindings = TokenStream::new();
            let (b_bind, b_var) = emit_value(base, gen);
            let (e_bind, e_var) = emit_value(end, gen);
            bindings.extend(b_bind);
            bindings.extend(e_bind);

            let cache_tok = emit_cacheable(*cacheable);
            let var = gen.next("dw");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::AddressSpace::<u32>::new_memory(
                    #cache_tok,
                    #read_write,
                    #b_var as u32,
                    #e_var as u32,
                    None,
                );
            });
            (bindings, var)
        }
        ResourceDesc::QWordMemory {
            cacheable,
            read_write,
            base,
            end,
        } => {
            let mut bindings = TokenStream::new();
            let (b_bind, b_var) = emit_value(base, gen);
            let (e_bind, e_var) = emit_value(end, gen);
            bindings.extend(b_bind);
            bindings.extend(e_bind);

            let cache_tok = emit_cacheable(*cacheable);
            let var = gen.next("qw");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::AddressSpace::<u64>::new_memory(
                    #cache_tok,
                    #read_write,
                    #b_var as u64,
                    #e_var as u64,
                    None,
                );
            });
            (bindings, var)
        }
    }
}

fn emit_cacheable(kind: CacheableKind) -> TokenStream {
    match kind {
        CacheableKind::NotCacheable => {
            quote! { fstart_acpi::aml::AddressSpaceCacheable::NotCacheable }
        }
        CacheableKind::Cacheable => quote! { fstart_acpi::aml::AddressSpaceCacheable::Cacheable },
        CacheableKind::WriteCombining => {
            quote! { fstart_acpi::aml::AddressSpaceCacheable::WriteCombining }
        }
        CacheableKind::Prefetchable => {
            quote! { fstart_acpi::aml::AddressSpaceCacheable::Prefetchable }
        }
    }
}
