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
    BinaryOp, CacheableKind, DslExpr, DslItem, DslReturnValue, DslValue, FieldAccess,
    FieldEntryDsl, FieldLock, FieldUpdate, NameOrInterp, RegionSpace, ResourceDesc,
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
    let mut generator = VarGen::new();
    let mut bindings = TokenStream::new();
    let mut child_refs: Vec<proc_macro2::Ident> = Vec::new();

    for item in items {
        let (binding, var) = emit_item(item, &mut generator);
        bindings.extend(binding);
        child_refs.push(var);
    }

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
fn emit_item(item: &DslItem, generator: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    match item {
        DslItem::Scope { path, children, .. } => emit_scope(path, children, generator),
        DslItem::Device { name, children, .. } => emit_device(name, children, generator),
        DslItem::Name { name, value, .. } => emit_name(name, value, generator),
        DslItem::Method {
            name,
            argc,
            serialized,
            body,
            ..
        } => emit_method(name, *argc, *serialized, body, generator),
        DslItem::Return { value, .. } => emit_return(value, generator),
        DslItem::OpRegion {
            name,
            space,
            offset,
            length,
            ..
        } => emit_op_region(name, *space, offset, length, generator),
        DslItem::Field {
            region,
            access,
            lock,
            update,
            entries,
            ..
        } => emit_field(region, *access, *lock, *update, entries, generator),
        DslItem::CreateDwordField {
            buffer,
            index,
            name,
            ..
        } => emit_create_dword_field(buffer, index, name, generator),
        DslItem::Store { value, target, .. } => emit_store(value, target, generator),
        DslItem::ShiftLeft {
            target,
            value,
            count,
            ..
        } => emit_binary_op("ShiftLeft", target, value, count, generator),
        DslItem::Subtract { target, a, b, .. } => {
            emit_binary_op("Subtract", target, a, b, generator)
        }
        DslItem::Add { target, a, b, .. } => emit_binary_op("Add", target, a, b, generator),
        DslItem::RawExpr { expr } => {
            let var = generator.next("raw");
            let bindings = quote! {
                let #var = #expr;
            };
            (bindings, var)
        }
        // New ASL 2.0 items
        DslItem::If {
            condition,
            body,
            else_body,
            ..
        } => emit_if(condition, body, else_body.as_deref(), generator),
        DslItem::While {
            condition, body, ..
        } => emit_while(condition, body, generator),
        DslItem::Assign { target, value, .. } => emit_assign(target, value, generator),
        DslItem::Notify { object, value, .. } => emit_notify_expr(object, value, generator),
        DslItem::Sleep { msec, .. } => emit_sleep_expr(msec, generator),
        DslItem::Stall { usec, .. } => emit_stall_expr(usec, generator),
        DslItem::Break { .. } => emit_break(generator),
        DslItem::Increment { target, .. } => emit_increment(target, generator),
        DslItem::Decrement { target, .. } => emit_decrement(target, generator),
        DslItem::MethodCall { name, args, .. } => emit_method_call(name, args, generator),
    }
}

// -----------------------------------------------------------------------
// Existing emitters (unchanged)
// -----------------------------------------------------------------------

fn emit_name_or_interp(name: &NameOrInterp) -> TokenStream {
    match name {
        NameOrInterp::Literal(s) => quote! { #s },
        NameOrInterp::Interpolation(expr) => quote! { #expr },
    }
}

fn emit_path_from_name_or_interp(name: &NameOrInterp) -> TokenStream {
    match name {
        NameOrInterp::Literal(s) => quote! { fstart_acpi::aml::Path::new(#s) },
        NameOrInterp::Interpolation(expr) => quote! { #expr },
    }
}

fn emit_scope(
    path: &NameOrInterp,
    children: &[DslItem],
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut child_vars: Vec<proc_macro2::Ident> = Vec::new();

    for child in children {
        let (binding, var) = emit_item(child, generator);
        bindings.extend(binding);
        child_vars.push(var);
    }

    let var = generator.next("scope");
    let refs: Vec<_> = child_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();

    match path {
        NameOrInterp::Literal(s) if s == "\\" => {
            bindings.extend(quote! {
                let #var = fstart_acpi::RootScope::new(alloc::vec![#(#refs),*]);
            });
        }
        _ => {
            let path_expr = emit_name_or_interp(path);
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::Scope::new(
                    fstart_acpi::aml::Path::new(#path_expr),
                    alloc::vec![#(#refs),*],
                );
            });
        }
    }

    (bindings, var)
}

fn emit_device(
    name: &NameOrInterp,
    children: &[DslItem],
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut child_vars: Vec<proc_macro2::Ident> = Vec::new();

    for child in children {
        let (binding, var) = emit_item(child, generator);
        bindings.extend(binding);
        child_vars.push(var);
    }

    let var = generator.next("dev");
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

fn emit_name(
    name: &str,
    value: &DslValue,
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (val_binding, val_var) = emit_value(value, generator);
    bindings.extend(val_binding);

    let var = generator.next("name");
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
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut body_vars: Vec<proc_macro2::Ident> = Vec::new();

    for item in body {
        let (binding, var) = emit_item(item, generator);
        bindings.extend(binding);
        body_vars.push(var);
    }

    let var = generator.next("method");
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

fn emit_return(
    value: &DslReturnValue,
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    match value {
        DslReturnValue::Legacy(dsl_value) => {
            let mut bindings = TokenStream::new();
            let (val_binding, val_var) = emit_value(dsl_value, generator);
            bindings.extend(val_binding);

            let var = generator.next("ret");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::Return::new(&#val_var);
            });
            (bindings, var)
        }
        DslReturnValue::Expr(expr) => {
            let mut bindings = TokenStream::new();
            let (expr_binding, expr_var) = emit_expr(expr, generator);
            bindings.extend(expr_binding);

            let var = generator.next("ret");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::Return::new(&#expr_var);
            });
            (bindings, var)
        }
    }
}

fn emit_value(value: &DslValue, generator: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    match value {
        DslValue::StringLit(s) => {
            let var = generator.next("str");
            let binding = quote! {
                let #var: &str = #s;
            };
            (binding, var)
        }
        DslValue::IntLit(tokens) => {
            let var = generator.next("int");
            let binding = quote! {
                let #var = #tokens;
            };
            (binding, var)
        }
        DslValue::EisaId(id) => {
            let var = generator.next("eisa");
            let binding = quote! {
                let #var = fstart_acpi::aml::EISAName::new(#id);
            };
            (binding, var)
        }
        DslValue::Package(elements) => emit_package(elements, generator),
        DslValue::ResourceTemplate(descs) => emit_resource_template(descs, generator),
        DslValue::Interpolation(expr) => {
            let var = generator.next("expr");
            let binding = quote! {
                let #var = #expr;
            };
            (binding, var)
        }
    }
}

fn emit_package(
    elements: &[DslValue],
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut elem_vars: Vec<proc_macro2::Ident> = Vec::new();

    for elem in elements {
        let (binding, var) = emit_value(elem, generator);
        bindings.extend(binding);
        elem_vars.push(var);
    }

    let var = generator.next("pkg");
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
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (off_bind, off_var) = emit_value(offset, generator);
    let (len_bind, len_var) = emit_value(length, generator);
    bindings.extend(off_bind);
    bindings.extend(len_bind);

    let space_tok = match space {
        RegionSpace::SystemMemory => quote! { fstart_acpi::aml::OpRegionSpace::SystemMemory },
        RegionSpace::SystemIO => quote! { fstart_acpi::aml::OpRegionSpace::SystemIO },
        RegionSpace::PciConfig => quote! { fstart_acpi::aml::OpRegionSpace::PCIConfig },
        RegionSpace::EmbeddedControl => quote! { fstart_acpi::aml::OpRegionSpace::EmbeddedControl },
    };

    let var = generator.next("opreg");
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
    generator: &mut VarGen,
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

    let var = generator.next("field");
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
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (buf_bind, buf_var) = emit_value(buffer, generator);
    let (idx_bind, idx_var) = emit_value(index, generator);
    bindings.extend(buf_bind);
    bindings.extend(idx_bind);

    let name_path = generator.next("cdwn");
    let var = generator.next("cdwf");
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
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (val_bind, val_var) = emit_value(value, generator);
    let (tgt_bind, tgt_var) = emit_value(target, generator);
    bindings.extend(val_bind);
    bindings.extend(tgt_bind);

    let var = generator.next("store");
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
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (tgt_bind, tgt_var) = emit_value(target, generator);
    let (a_bind, a_var) = emit_value(a, generator);
    let (b_bind, b_var) = emit_value(b, generator);
    bindings.extend(tgt_bind);
    bindings.extend(a_bind);
    bindings.extend(b_bind);

    let op_ident = format_ident!("{}", op_name);
    let var = generator.next("binop");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::#op_ident::new(&#tgt_var, &#a_var, &#b_var);
    });
    (bindings, var)
}

fn emit_resource_template(
    descs: &[ResourceDesc],
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut desc_vars: Vec<proc_macro2::Ident> = Vec::new();

    for desc in descs {
        let (binding, var) = emit_resource_desc(desc, generator);
        bindings.extend(binding);
        desc_vars.push(var);
    }

    let var = generator.next("rt");
    let refs: Vec<_> = desc_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();

    bindings.extend(quote! {
        let #var = fstart_acpi::aml::ResourceTemplate::new(alloc::vec![#(#refs),*]);
    });

    (bindings, var)
}

fn emit_resource_desc(
    desc: &ResourceDesc,
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    match desc {
        ResourceDesc::Memory32Fixed {
            read_write,
            base,
            size,
        } => {
            let mut bindings = TokenStream::new();
            let (base_binding, base_var) = emit_value(base, generator);
            let (size_binding, size_var) = emit_value(size, generator);
            bindings.extend(base_binding);
            bindings.extend(size_binding);

            let var = generator.next("mem");
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
            let (irq_binding, irq_var) = emit_value(irq, generator);
            bindings.extend(irq_binding);

            let shared = !exclusive;

            let var = generator.next("irq");
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
        ResourceDesc::Irq {
            edge,
            active_high,
            exclusive,
            irq,
        } => {
            let mut bindings = TokenStream::new();
            let (irq_binding, irq_var) = emit_value(irq, generator);
            bindings.extend(irq_binding);

            let var = generator.next("isa_irq");
            bindings.extend(quote! {
                let #var = fstart_acpi::IsaIrq::new(
                    #edge,
                    #active_high,
                    #exclusive,
                    #irq_var as u8,
                );
            });
            (bindings, var)
        }
        ResourceDesc::IrqNoFlags { irq } => {
            let mut bindings = TokenStream::new();
            let (irq_binding, irq_var) = emit_value(irq, generator);
            bindings.extend(irq_binding);

            let var = generator.next("isa_irq");
            bindings.extend(quote! {
                let #var = fstart_acpi::IsaIrq::no_flags(#irq_var as u8);
            });
            (bindings, var)
        }
        ResourceDesc::WordBusNumber { start, end } => {
            let mut bindings = TokenStream::new();
            let (s_bind, s_var) = emit_value(start, generator);
            let (e_bind, e_var) = emit_value(end, generator);
            bindings.extend(s_bind);
            bindings.extend(e_bind);

            let var = generator.next("bus");
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
            let (b_bind, b_var) = emit_value(base, generator);
            let (e_bind, e_var) = emit_value(end, generator);
            bindings.extend(b_bind);
            bindings.extend(e_bind);

            let cache_tok = emit_cacheable(*cacheable);
            let var = generator.next("dw");
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
            let (b_bind, b_var) = emit_value(base, generator);
            let (e_bind, e_var) = emit_value(end, generator);
            bindings.extend(b_bind);
            bindings.extend(e_bind);

            let cache_tok = emit_cacheable(*cacheable);
            let var = generator.next("qw");
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
            let (b_bind, b_var) = emit_value(base, generator);
            let (e_bind, e_var) = emit_value(end, generator);
            let (a_bind, a_var) = emit_value(align, generator);
            let (l_bind, l_var) = emit_value(len, generator);
            bindings.extend(b_bind);
            bindings.extend(e_bind);
            bindings.extend(a_bind);
            bindings.extend(l_bind);

            let var = generator.next("iop");
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
            let (b_bind, b_var) = emit_value(base, generator);
            let (e_bind, e_var) = emit_value(end, generator);
            bindings.extend(b_bind);
            bindings.extend(e_bind);

            let var = generator.next("dio");
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

// -----------------------------------------------------------------------
// New ASL 2.0 emitters
// -----------------------------------------------------------------------

/// Emit code for a `DslExpr`, returning binding code and the variable
/// name holding the resulting AML object.
fn emit_expr(expr: &DslExpr, generator: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    match expr {
        DslExpr::IntLit(tokens) => {
            let var = generator.next("int");
            (quote! { let #var = #tokens; }, var)
        }
        DslExpr::StringLit(s) => {
            let var = generator.next("str");
            (quote! { let #var: &str = #s; }, var)
        }
        DslExpr::Path(name) => {
            let var = generator.next("path");
            (
                quote! { let #var = fstart_acpi::aml::Path::new(#name); },
                var,
            )
        }
        DslExpr::Local(n) => {
            let var = generator.next("loc");
            (quote! { let #var = fstart_acpi::aml::Local(#n); }, var)
        }
        DslExpr::Arg(n) => {
            let var = generator.next("arg");
            (quote! { let #var = fstart_acpi::aml::Arg(#n); }, var)
        }
        DslExpr::Zero => {
            let var = generator.next("zero");
            (quote! { let #var = fstart_acpi::aml::Zero {}; }, var)
        }
        DslExpr::One => {
            let var = generator.next("one");
            (quote! { let #var = fstart_acpi::aml::One {}; }, var)
        }
        DslExpr::Ones => {
            let var = generator.next("ones");
            (quote! { let #var = fstart_acpi::aml::Ones {}; }, var)
        }
        DslExpr::Interpolation(tokens) => {
            let var = generator.next("itp");
            (quote! { let #var = #tokens; }, var)
        }
        DslExpr::ToUUID(s) => {
            let var = generator.next("uuid");
            (quote! { let #var = fstart_acpi::aml::Uuid::new(#s); }, var)
        }
        DslExpr::SizeOf(inner) => {
            let mut bindings = TokenStream::new();
            let (inner_bind, inner_var) = emit_expr(inner, generator);
            bindings.extend(inner_bind);
            let var = generator.next("szo");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::SizeOf::new(&#inner_var);
            });
            (bindings, var)
        }
        DslExpr::DeRefOf(inner) => {
            let mut bindings = TokenStream::new();
            let (inner_bind, inner_var) = emit_expr(inner, generator);
            bindings.extend(inner_bind);
            let var = generator.next("drf");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::DeRefOf::new(&#inner_var);
            });
            (bindings, var)
        }
        DslExpr::CondRefOf(source, target) => {
            let mut bindings = TokenStream::new();
            let (src_bind, src_var) = emit_expr(source, generator);
            let (tgt_bind, tgt_var) = emit_expr(target, generator);
            bindings.extend(src_bind);
            bindings.extend(tgt_bind);
            let var = generator.next("crf");
            bindings.extend(quote! {
                let #var = fstart_acpi::ext::cond_ref_of::CondRefOf::new(&#src_var, &#tgt_var);
            });
            (bindings, var)
        }
        DslExpr::Index(source, index) => {
            let mut bindings = TokenStream::new();
            let (src_bind, src_var) = emit_expr(source, generator);
            let (idx_bind, idx_var) = emit_expr(index, generator);
            bindings.extend(src_bind);
            bindings.extend(idx_bind);
            // acpi_tables Index uses the binary_op pattern: Index::new(target, source, index)
            // With NullTarget to discard the store.
            let nt_var = generator.next("nt");
            let var = generator.next("idx");
            bindings.extend(quote! {
                let #nt_var = fstart_acpi::NullTarget;
                let #var = fstart_acpi::aml::Index::new(&#nt_var, &#src_var, &#idx_var);
            });
            (bindings, var)
        }
        DslExpr::Binary(op, lhs, rhs) => emit_binary_expr(*op, lhs, rhs, generator),
        DslExpr::LNot(inner) => {
            let mut bindings = TokenStream::new();
            let (inner_bind, inner_var) = emit_expr(inner, generator);
            bindings.extend(inner_bind);
            let var = generator.next("lnt");
            bindings.extend(quote! {
                let #var = fstart_acpi::ext::logical::LNot::new(&#inner_var);
            });
            (bindings, var)
        }
        DslExpr::BitNot(inner) => {
            // AML NotOp is a binary_op pattern with target: Not::new(target, operand)
            // We don't have Not in acpi_tables, so we use ~x == x XOR Ones pattern:
            // Xor(NullTarget, x, Ones)
            let mut bindings = TokenStream::new();
            let (inner_bind, inner_var) = emit_expr(inner, generator);
            bindings.extend(inner_bind);
            let nt_var = generator.next("nt");
            let ones_var = generator.next("ones");
            let var = generator.next("bnot");
            bindings.extend(quote! {
                let #nt_var = fstart_acpi::NullTarget;
                let #ones_var = fstart_acpi::aml::Ones {};
                let #var = fstart_acpi::aml::Xor::new(&#nt_var, &#inner_var, &#ones_var);
            });
            (bindings, var)
        }
    }
}

/// Emit a binary expression.
fn emit_binary_expr(
    op: BinaryOp,
    lhs: &DslExpr,
    rhs: &DslExpr,
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (lhs_bind, lhs_var) = emit_expr(lhs, generator);
    let (rhs_bind, rhs_var) = emit_expr(rhs, generator);
    bindings.extend(lhs_bind);
    bindings.extend(rhs_bind);

    match op {
        // Comparison operators: `Op::new(left, right)` -- 2 args
        BinaryOp::Equal => {
            let var = generator.next("eq");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::Equal::new(&#lhs_var, &#rhs_var);
            });
            (bindings, var)
        }
        BinaryOp::NotEqual => {
            let var = generator.next("ne");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::NotEqual::new(&#lhs_var, &#rhs_var);
            });
            (bindings, var)
        }
        BinaryOp::Less => {
            let var = generator.next("lt");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::LessThan::new(&#lhs_var, &#rhs_var);
            });
            (bindings, var)
        }
        BinaryOp::Greater => {
            let var = generator.next("gt");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::GreaterThan::new(&#lhs_var, &#rhs_var);
            });
            (bindings, var)
        }
        BinaryOp::LessEqual => {
            let var = generator.next("le");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::LessEqual::new(&#lhs_var, &#rhs_var);
            });
            (bindings, var)
        }
        BinaryOp::GreaterEqual => {
            let var = generator.next("ge");
            bindings.extend(quote! {
                let #var = fstart_acpi::aml::GreaterEqual::new(&#lhs_var, &#rhs_var);
            });
            (bindings, var)
        }
        // Logical operators: ext types with 2 args
        BinaryOp::LAnd => {
            let var = generator.next("land");
            bindings.extend(quote! {
                let #var = fstart_acpi::ext::logical::LAnd::new(&#lhs_var, &#rhs_var);
            });
            (bindings, var)
        }
        BinaryOp::LOr => {
            let var = generator.next("lor");
            bindings.extend(quote! {
                let #var = fstart_acpi::ext::logical::LOr::new(&#lhs_var, &#rhs_var);
            });
            (bindings, var)
        }
        // Arithmetic/bitwise operators: `Op::new(target, a, b)` -- 3 args with NullTarget
        op => {
            let op_ident = match op {
                BinaryOp::Add => format_ident!("Add"),
                BinaryOp::Subtract => format_ident!("Subtract"),
                BinaryOp::Multiply => format_ident!("Multiply"),
                BinaryOp::Divide => {
                    // AML Divide has 4 args: Divide(dividend, divisor, remainder, result)
                    // In acpi_tables it might not exist. Let's use Mod-like approach.
                    // Actually, Divide is not in acpi_tables binary_op!. Skip for now.
                    // We'll emit a compile_error for unsupported ops.
                    let var = generator.next("err");
                    bindings.extend(quote! {
                        compile_error!("Divide operator not supported in acpi_dsl! (AML Divide has 4 operands)");
                        let #var = ();
                    });
                    return (bindings, var);
                }
                BinaryOp::Mod => format_ident!("Mod"),
                BinaryOp::ShiftLeft => format_ident!("ShiftLeft"),
                BinaryOp::ShiftRight => format_ident!("ShiftRight"),
                BinaryOp::And => format_ident!("And"),
                BinaryOp::Or => format_ident!("Or"),
                BinaryOp::Xor => format_ident!("Xor"),
                _ => unreachable!(),
            };
            let nt_var = generator.next("nt");
            let var = generator.next("bop");
            bindings.extend(quote! {
                let #nt_var = fstart_acpi::NullTarget;
                let #var = fstart_acpi::aml::#op_ident::new(&#nt_var, &#lhs_var, &#rhs_var);
            });
            (bindings, var)
        }
    }
}

/// Emit an `If` statement.
fn emit_if(
    condition: &DslExpr,
    body: &[DslItem],
    else_body: Option<&[DslItem]>,
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();

    // Emit condition
    let (cond_bind, cond_var) = emit_expr(condition, generator);
    bindings.extend(cond_bind);

    // Emit body items
    let mut body_vars: Vec<proc_macro2::Ident> = Vec::new();
    for item in body {
        let (bind, var) = emit_item(item, generator);
        bindings.extend(bind);
        body_vars.push(var);
    }

    let body_refs: Vec<_> = body_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();

    if let Some(else_items) = else_body {
        // Emit else body items
        let mut else_vars: Vec<proc_macro2::Ident> = Vec::new();
        for item in else_items {
            let (bind, var) = emit_item(item, generator);
            bindings.extend(bind);
            else_vars.push(var);
        }

        let else_refs: Vec<_> = else_vars
            .iter()
            .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
            .collect();

        let else_var = generator.next("else");
        let var = generator.next("if");

        // The Else object goes as the last child of the If
        bindings.extend(quote! {
            let #else_var = fstart_acpi::aml::Else::new(
                alloc::vec![#(#else_refs),*],
            );
            let #var = fstart_acpi::aml::If::new(
                &#cond_var,
                alloc::vec![#(#body_refs,)* &#else_var as &dyn fstart_acpi::Aml],
            );
        });
        (bindings, var)
    } else {
        let var = generator.next("if");
        bindings.extend(quote! {
            let #var = fstart_acpi::aml::If::new(
                &#cond_var,
                alloc::vec![#(#body_refs),*],
            );
        });
        (bindings, var)
    }
}

/// Emit a `While` statement.
fn emit_while(
    condition: &DslExpr,
    body: &[DslItem],
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();

    let (cond_bind, cond_var) = emit_expr(condition, generator);
    bindings.extend(cond_bind);

    let mut body_vars: Vec<proc_macro2::Ident> = Vec::new();
    for item in body {
        let (bind, var) = emit_item(item, generator);
        bindings.extend(bind);
        body_vars.push(var);
    }

    let body_refs: Vec<_> = body_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();

    let var = generator.next("while");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::While::new(
            &#cond_var,
            alloc::vec![#(#body_refs),*],
        );
    });
    (bindings, var)
}

/// Emit an assignment: `target = value;`
///
/// Generates `Store::new(&target, &value_expr)`.
fn emit_assign(
    target: &DslExpr,
    value: &DslExpr,
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (val_bind, val_var) = emit_expr(value, generator);
    let (tgt_bind, tgt_var) = emit_expr(target, generator);
    bindings.extend(val_bind);
    bindings.extend(tgt_bind);

    let var = generator.next("store");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::Store::new(&#tgt_var, &#val_var);
    });
    (bindings, var)
}

/// Emit `Notify(object, value);`
fn emit_notify_expr(
    object: &DslExpr,
    value: &DslExpr,
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (obj_bind, obj_var) = emit_expr(object, generator);
    let (val_bind, val_var) = emit_expr(value, generator);
    bindings.extend(obj_bind);
    bindings.extend(val_bind);

    let var = generator.next("ntfy");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::Notify::new(&#obj_var, &#val_var);
    });
    (bindings, var)
}

/// Emit `Sleep(msec);`
fn emit_sleep_expr(msec: &DslExpr, generator: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (msec_bind, msec_var) = emit_expr(msec, generator);
    bindings.extend(msec_bind);

    let var = generator.next("slp");
    bindings.extend(quote! {
        let #var = fstart_acpi::ext::sleep::Sleep::new(&#msec_var);
    });
    (bindings, var)
}

/// Emit `Stall(usec);`
fn emit_stall_expr(usec: &DslExpr, generator: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (usec_bind, usec_var) = emit_expr(usec, generator);
    bindings.extend(usec_bind);

    let var = generator.next("stl");
    bindings.extend(quote! {
        let #var = fstart_acpi::ext::sleep::Stall::new(&#usec_var);
    });
    (bindings, var)
}

/// Emit `Break;`
fn emit_break(generator: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    let var = generator.next("brk");
    let bindings = quote! {
        let #var = fstart_acpi::ext::break_op::Break;
    };
    (bindings, var)
}

/// Emit `Increment(target);` or `target++`
fn emit_increment(target: &DslExpr, generator: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (tgt_bind, tgt_var) = emit_expr(target, generator);
    bindings.extend(tgt_bind);

    let var = generator.next("inc");
    bindings.extend(quote! {
        let #var = fstart_acpi::ext::inc_dec::Increment::new(&#tgt_var);
    });
    (bindings, var)
}

/// Emit `Decrement(target);` or `target--`
fn emit_decrement(target: &DslExpr, generator: &mut VarGen) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let (tgt_bind, tgt_var) = emit_expr(target, generator);
    bindings.extend(tgt_bind);

    let var = generator.next("dec");
    bindings.extend(quote! {
        let #var = fstart_acpi::ext::inc_dec::Decrement::new(&#tgt_var);
    });
    (bindings, var)
}

/// Emit a method invocation statement: `METHOD(arg0, arg1, ...)`.
fn emit_method_call(
    name: &NameOrInterp,
    args: &[DslExpr],
    generator: &mut VarGen,
) -> (TokenStream, proc_macro2::Ident) {
    let mut bindings = TokenStream::new();
    let mut arg_vars = Vec::new();
    for arg in args {
        let (arg_bind, arg_var) = emit_expr(arg, generator);
        bindings.extend(arg_bind);
        arg_vars.push(arg_var);
    }

    let arg_refs: Vec<_> = arg_vars
        .iter()
        .map(|v| quote! { &#v as &dyn fstart_acpi::Aml })
        .collect();
    let path_expr = emit_path_from_name_or_interp(name);
    let var = generator.next("call");
    bindings.extend(quote! {
        let #var = fstart_acpi::aml::MethodCall::new(
            #path_expr,
            alloc::vec![#(#arg_refs),*],
        );
    });
    (bindings, var)
}
