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

use crate::parse::{
    CacheableKind, DslItem, DslValue, FieldAccess, FieldEntryDsl, FieldLock, FieldUpdate,
    NameOrInterp, RegionSpace, ResourceDesc,
};

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
        DslItem::OpRegion {
            name,
            space,
            offset,
            length,
            ..
        } => emit_op_region(name, *space, offset, length, gen),
        DslItem::Field {
            region,
            access,
            lock,
            update,
            entries,
            ..
        } => emit_field(region, *access, *lock, *update, entries, gen),
        DslItem::CreateDwordField {
            buffer,
            index,
            name,
            ..
        } => emit_create_dword_field(buffer, index, name, gen),
        DslItem::Store { value, target, .. } => emit_store(value, target, gen),
        DslItem::ShiftLeft {
            target,
            value,
            count,
            ..
        } => emit_binary_op("ShiftLeft", target, value, count, gen),
        DslItem::Subtract { target, a, b, .. } => emit_binary_op("Subtract", target, a, b, gen),
        DslItem::Add { target, a, b, .. } => emit_binary_op("Add", target, a, b, gen),
        DslItem::RawExpr { expr } => {
            let var = gen.next("raw");
            let bindings = quote! {
                let #var = #expr;
            };
            (bindings, var)
        }
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

fn emit_op_region(
    name: &str,
    space: RegionSpace,
    offset: &DslValue,
    length: &DslValue,
    gen: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (off_bind, off_var) = emit_value(offset, gen);
    let (len_bind, len_var) = emit_value(length, gen);
    bindings.extend(off_bind);
    bindings.extend(len_bind);

    let space_tok = match space {
        RegionSpace::SystemMemory => quote! { fstart_acpi::aml::OpRegionSpace::SystemMemory },
        RegionSpace::SystemIO => quote! { fstart_acpi::aml::OpRegionSpace::SystemIO },
        RegionSpace::PciConfig => quote! { fstart_acpi::aml::OpRegionSpace::PCIConfig },
        RegionSpace::EmbeddedControl => quote! { fstart_acpi::aml::OpRegionSpace::EmbeddedControl },
    };

    let var = gen.next("opreg");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::OpRegion::new(
            fstart_acpi::aml::Path::new(#name),
            #space_tok,
            &#off_var,
            &#len_var,
        );
    });
    (bindings, var)
}

fn emit_field(
    region: &str,
    access: FieldAccess,
    lock: FieldLock,
    update: FieldUpdate,
    entries: &[FieldEntryDsl],
    gen: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let access_tok = match access {
        FieldAccess::AnyAcc => quote! { fstart_acpi::aml::FieldAccessType::Any },
        FieldAccess::ByteAcc => quote! { fstart_acpi::aml::FieldAccessType::Byte },
        FieldAccess::WordAcc => quote! { fstart_acpi::aml::FieldAccessType::Word },
        FieldAccess::DWordAcc => quote! { fstart_acpi::aml::FieldAccessType::DWord },
        FieldAccess::QWordAcc => quote! { fstart_acpi::aml::FieldAccessType::QWord },
    };
    let lock_tok = match lock {
        FieldLock::NoLock => quote! { fstart_acpi::aml::FieldLockRule::NoLock },
        FieldLock::Lock => quote! { fstart_acpi::aml::FieldLockRule::Lock },
    };
    let update_tok = match update {
        FieldUpdate::Preserve => quote! { fstart_acpi::aml::FieldUpdateRule::Preserve },
        FieldUpdate::WriteAsOnes => quote! { fstart_acpi::aml::FieldUpdateRule::WriteAsOnes },
        FieldUpdate::WriteAsZeroes => quote! { fstart_acpi::aml::FieldUpdateRule::WriteAsZeroes },
    };

    // Convert DSL entries to FieldEntry values, resolving Offset directives
    // into Reserved gaps.
    let mut field_entries = Vec::new();
    let mut bit_pos: usize = 0;
    for entry in entries {
        match entry {
            FieldEntryDsl::Named(name, bits) => {
                let mut name_bytes = [b'_'; 4];
                for (i, b) in name.bytes().take(4).enumerate() {
                    name_bytes[i] = b;
                }
                let [a, b, c, d] = name_bytes;
                field_entries.push(quote! {
                    fstart_acpi::aml::FieldEntry::Named([#a, #b, #c, #d], #bits)
                });
                bit_pos += bits;
            }
            FieldEntryDsl::Reserved(bits) => {
                field_entries.push(quote! {
                    fstart_acpi::aml::FieldEntry::Reserved(#bits)
                });
                bit_pos += bits;
            }
            FieldEntryDsl::Offset(byte_offset) => {
                let target_bits = byte_offset * 8;
                if target_bits > bit_pos {
                    let gap = target_bits - bit_pos;
                    field_entries.push(quote! {
                        fstart_acpi::aml::FieldEntry::Reserved(#gap)
                    });
                    bit_pos = target_bits;
                }
            }
        }
    }

    let var = gen.next("field");
    let bindings = quote! {
        let #var = fstart_acpi::aml::Field::new(
            fstart_acpi::aml::Path::new(#region),
            #access_tok,
            #lock_tok,
            #update_tok,
            alloc::vec![#(#field_entries),*],
        );
    };
    (bindings, var)
}

fn emit_create_dword_field(
    buffer: &DslValue,
    index: &DslValue,
    name: &str,
    gen: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (buf_bind, buf_var) = emit_value(buffer, gen);
    let (idx_bind, idx_var) = emit_value(index, gen);
    bindings.extend(buf_bind);
    bindings.extend(idx_bind);

    let name_path = gen.next("cdwn");
    let var = gen.next("cdwf");
    bindings.extend(quote! {
        let #name_path = fstart_acpi::aml::Path::new(#name);
        let #var = fstart_acpi::aml::CreateDWordField::new(
            &#name_path,
            &#buf_var,
            &#idx_var,
        );
    });
    (bindings, var)
}

fn emit_store(
    value: &DslValue,
    target: &DslValue,
    gen: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (val_bind, val_var) = emit_value(value, gen);
    let (tgt_bind, tgt_var) = emit_value(target, gen);
    bindings.extend(val_bind);
    bindings.extend(tgt_bind);

    let var = gen.next("store");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::Store::new(&#tgt_var, &#val_var);
    });
    (bindings, var)
}

fn emit_binary_op(
    op_name: &str,
    target: &DslValue,
    a: &DslValue,
    b: &DslValue,
    gen: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (tgt_bind, tgt_var) = emit_value(target, gen);
    let (a_bind, a_var) = emit_value(a, gen);
    let (b_bind, b_var) = emit_value(b, gen);
    bindings.extend(tgt_bind);
    bindings.extend(a_bind);
    bindings.extend(b_bind);

    let op_ident = format_ident!("{}", op_name);
    let var = gen.next("binop");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::#op_ident::new(&#tgt_var, &#a_var, &#b_var);
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
        ResourceDesc::IoPort {
            base,
            end,
            align,
            len,
        } => {
            let mut bindings = TokenStream::new();
            let (b_bind, b_var) = emit_value(base, gen);
            let (e_bind, e_var) = emit_value(end, gen);
            let (a_bind, a_var) = emit_value(align, gen);
            let (l_bind, l_var) = emit_value(len, gen);
            bindings.extend(b_bind);
            bindings.extend(e_bind);
            bindings.extend(a_bind);
            bindings.extend(l_bind);

            let var = gen.next("iop");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::IO::new(
                    #b_var as u16,
                    #e_var as u16,
                    #a_var as u8,
                    #l_var as u8,
                );
            });
            (bindings, var)
        }
        ResourceDesc::DWordIO { base, end } => {
            let mut bindings = TokenStream::new();
            let (b_bind, b_var) = emit_value(base, gen);
            let (e_bind, e_var) = emit_value(end, gen);
            bindings.extend(b_bind);
            bindings.extend(e_bind);

            let var = gen.next("dio");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::AddressSpace::<u32>::new_io(
                    #b_var as u32,
                    #e_var as u32,
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
