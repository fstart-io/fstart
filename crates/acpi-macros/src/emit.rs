//! AML byte-code emission for `acpi_dsl!`.
//!
//! The proc macro performs all structural encoding. The generated expression is
//! only a const `AmlFragment` plus, when needed, runtime operand bindings.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Error, LitInt, LitStr, Result};

use crate::parse::{
    BinaryOp, CacheableKind, DslExpr, DslItem, DslReturnValue, DslValue, FieldAccess,
    FieldEntryDsl, FieldLock, FieldUpdate, NameOrInterp, Operand, OperandKind, RegionSpace,
    ResourceDesc,
};

#[derive(Clone)]
struct FixupSpec {
    offset: usize,
    kind: OperandKind,
    integer: bool,
    expr: TokenStream,
    action: Option<RangeAction>,
}

#[derive(Clone)]
enum RangeValue {
    Literal(u64),
    Operand(usize),
}

#[derive(Clone)]
struct RangeAction {
    offset: usize,
    min: RangeValue,
    max: RangeValue,
}

#[derive(Clone)]
struct ConstPatch {
    offset: usize,
    kind: OperandKind,
    integer: bool,
    expr: TokenStream,
}

#[derive(Default, Clone)]
struct Encoded {
    bytes: Vec<u8>,
    fixups: Vec<FixupSpec>,
    const_patches: Vec<ConstPatch>,
}

impl Encoded {
    fn byte(byte: u8) -> Self {
        Self {
            bytes: vec![byte],
            fixups: vec![],
            const_patches: vec![],
        }
    }

    fn append(&mut self, mut other: Self) {
        let byte_base = self.bytes.len();
        let operand_base = self.fixups.len();
        other.fixups.iter_mut().for_each(|fixup| {
            fixup.offset += byte_base;
            if let Some(action) = &mut fixup.action {
                action.offset += byte_base;
                for value in [&mut action.min, &mut action.max] {
                    if let RangeValue::Operand(index) = value {
                        *index += operand_base;
                    }
                }
            }
        });
        other
            .const_patches
            .iter_mut()
            .for_each(|patch| patch.offset += byte_base);
        self.bytes.append(&mut other.bytes);
        self.fixups.append(&mut other.fixups);
        self.const_patches.append(&mut other.const_patches);
    }

    fn raw(&mut self, bytes: impl IntoIterator<Item = u8>) {
        self.bytes.extend(bytes);
    }
}

pub fn emit_items(items: &[DslItem]) -> TokenStream {
    match encode_items(items) {
        Ok(encoded) => fragment_tokens(encoded),
        Err(error) => error.to_compile_error(),
    }
}

fn kind_tokens(kind: OperandKind) -> TokenStream {
    match kind {
        OperandKind::Byte => quote!(fstart_acpi::FixupKind::Byte),
        OperandKind::Word => quote!(fstart_acpi::FixupKind::Word),
        OperandKind::DWord => quote!(fstart_acpi::FixupKind::DWord),
        OperandKind::QWord => quote!(fstart_acpi::FixupKind::QWord),
    }
}

fn range_value_tokens(value: &RangeValue) -> TokenStream {
    match value {
        RangeValue::Literal(value) => quote!(fstart_acpi::FixupValue::Literal(#value)),
        RangeValue::Operand(index) => quote!(fstart_acpi::FixupValue::Operand(#index)),
    }
}

fn fragment_tokens(encoded: Encoded) -> TokenStream {
    let byte_len = encoded.bytes.len();
    let fixup_len = encoded.fixups.len();
    let bytes = encoded.bytes.iter();
    let fixups = encoded.fixups.iter().map(|fixup| {
        let offset = fixup.offset;
        let kind = kind_tokens(fixup.kind);
        match &fixup.action {
            None if fixup.integer => quote!(fstart_acpi::Fixup::new(#offset, #kind)),
            None => quote!(fstart_acpi::Fixup::raw(#offset, #kind)),
            Some(action) => {
                let length_offset = action.offset;
                let min = range_value_tokens(&action.min);
                let max = range_value_tokens(&action.max);
                quote!(fstart_acpi::Fixup::with_range_length(
                    #offset, #kind, #length_offset, #min, #max
                ))
            }
        }
    });
    let patches = encoded.const_patches.iter().map(|patch| {
        let offset = patch.offset;
        let kind = kind_tokens(patch.kind);
        let expr = &patch.expr;
        let method = if patch.integer {
            quote!(with_const_integer)
        } else {
            quote!(with_const)
        };
        quote!(.#method(#offset, #kind, {
            // Widen first: signed negatives become > u64::MAX rather than
            // silently wrapping to a valid QWord. u128 inputs retain high bits.
            let value = ((#expr) | 0) as u128;
            assert!(value <= u64::MAX as u128, "const AML operand is not a u64");
            value as u64
        }))
    });
    let fragment = quote! {
        fstart_acpi::AmlFragment::new([#(#bytes),*], [#(#fixups),*])#(#patches)*
    };
    if encoded.fixups.is_empty() {
        quote!(const { #fragment })
    } else {
        let operands = encoded.fixups.iter().map(|fixup| &fixup.expr);
        quote! {{
            static FRAGMENT: fstart_acpi::AmlFragment<#byte_len, #fixup_len> = #fragment;
            fstart_acpi::BoundAmlFragment::new(&FRAGMENT, [#(fstart_acpi::checked_operand(#operands)),*])
        }}
    }
}

fn encode_items(items: &[DslItem]) -> Result<Encoded> {
    items.iter().try_fold(Encoded::default(), |mut out, item| {
        out.append(encode_item(item)?);
        Ok(out)
    })
}

fn encode_item(item: &DslItem) -> Result<Encoded> {
    match item {
        DslItem::Scope { path, children, .. } => {
            named_package(&[0x10], encode_name_operand(path)?, encode_items(children)?)
        }
        DslItem::Device { name, children, .. } => named_package(
            &[0x5b, 0x82],
            encode_name_operand(name)?,
            encode_items(children)?,
        ),
        DslItem::Name { name, value, .. } => {
            let mut out = Encoded::byte(0x08);
            out.append(encode_name_string(name)?);
            out.append(encode_value(value)?);
            Ok(out)
        }
        DslItem::Method {
            name,
            argc,
            serialized,
            body,
            ..
        } => {
            let mut header = encode_name_string(name)?;
            header.raw([(*argc & 7) | (u8::from(*serialized) << 3)]);
            named_package(&[0x14], header, encode_items(body)?)
        }
        DslItem::Return { value, .. } => {
            let value = match value {
                DslReturnValue::Legacy(v) => encode_value(v)?,
                DslReturnValue::Expr(v) => encode_expr(v)?,
            };
            op_args(&[0xa4], [value])
        }
        DslItem::OpRegion {
            name,
            space,
            offset,
            length,
            ..
        } => {
            let space = match space {
                RegionSpace::SystemMemory => 0,
                RegionSpace::SystemIO => 1,
                RegionSpace::PciConfig => 2,
                RegionSpace::EmbeddedControl => 3,
            };
            let mut out = Encoded {
                bytes: vec![0x5b, 0x80],
                ..Encoded::default()
            };
            out.append(encode_name_string(name)?);
            out.raw([space]);
            out.append(encode_value(offset)?);
            out.append(encode_value(length)?);
            Ok(out)
        }
        DslItem::Field {
            region,
            access,
            lock,
            update,
            entries,
            ..
        } => encode_field(region, *access, *lock, *update, entries),
        DslItem::CreateDwordField {
            buffer,
            index,
            name,
            ..
        } => {
            let mut out = Encoded::byte(0x8a);
            out.append(encode_value(buffer)?);
            out.append(encode_value(index)?);
            out.append(encode_name_string(name)?);
            Ok(out)
        }
        DslItem::Store { value, target, .. } => {
            op_args(&[0x70], [encode_value(value)?, encode_value(target)?])
        }
        DslItem::ShiftLeft {
            target,
            value,
            count,
            ..
        } => op_args(
            &[0x79],
            [
                encode_value(value)?,
                encode_value(count)?,
                encode_value(target)?,
            ],
        ),
        DslItem::Subtract { target, a, b, .. } => op_args(
            &[0x74],
            [encode_value(a)?, encode_value(b)?, encode_value(target)?],
        ),
        DslItem::Add { target, a, b, .. } => op_args(
            &[0x72],
            [encode_value(a)?, encode_value(b)?, encode_value(target)?],
        ),
        DslItem::If {
            condition,
            body,
            else_body,
            ..
        } => {
            let mut content = encode_expr(condition)?;
            content.append(encode_items(body)?);
            let mut out = aml_package(&[0xa0], content)?;
            if let Some(body) = else_body {
                out.append(aml_package(&[0xa1], encode_items(body)?)?);
            }
            Ok(out)
        }
        DslItem::While {
            condition, body, ..
        } => {
            let mut content = encode_expr(condition)?;
            content.append(encode_items(body)?);
            aml_package(&[0xa2], content)
        }
        DslItem::Assign { target, value, .. } => encode_assignment(target, value),
        DslItem::Notify { object, value, .. } => {
            op_args(&[0x86], [encode_expr(object)?, encode_expr(value)?])
        }
        DslItem::Sleep { msec, .. } => op_args(&[0x5b, 0x22], [encode_expr(msec)?]),
        DslItem::Stall { usec, .. } => op_args(&[0x5b, 0x21], [encode_expr(usec)?]),
        DslItem::Break { .. } => Ok(Encoded::byte(0xa5)),
        DslItem::Increment { target, .. } => op_args(&[0x75], [encode_expr(target)?]),
        DslItem::Decrement { target, .. } => op_args(&[0x76], [encode_expr(target)?]),
        DslItem::MethodCall { name, args, .. } => {
            let mut out = encode_name_operand(name)?;
            for arg in args {
                out.append(encode_expr(arg)?);
            }
            Ok(out)
        }
        DslItem::DivideAssign { target, value, .. } => op_args(
            &[0x78],
            [
                encode_expr(target)?,
                encode_expr(value)?,
                Encoded::byte(0x00),
                encode_expr(target)?,
            ],
        ),
        DslItem::Mutex {
            name, sync_level, ..
        } => {
            let level = literal_u64(sync_level)?;
            if level > 15 {
                return Err(Error::new_spanned(
                    sync_level.clone(),
                    "mutex sync level must be 0-15",
                ));
            }
            let mut out = Encoded {
                bytes: vec![0x5b, 0x01],
                ..Encoded::default()
            };
            out.append(encode_name_string(name)?);
            out.raw([level as u8]);
            Ok(out)
        }
        DslItem::Acquire { mutex, timeout, .. } => {
            let timeout = literal_u64(timeout)?;
            if timeout > u16::MAX as u64 {
                return Err(Error::new_spanned(timeout, "Acquire timeout exceeds u16"));
            }
            let mut out = Encoded {
                bytes: vec![0x5b, 0x23],
                ..Encoded::default()
            };
            out.append(encode_name_string(mutex)?);
            out.raw((timeout as u16).to_le_bytes());
            Ok(out)
        }
        DslItem::Release { mutex, .. } => {
            let mut out = Encoded {
                bytes: vec![0x5b, 0x27],
                ..Encoded::default()
            };
            out.append(encode_name_string(mutex)?);
            Ok(out)
        }
        DslItem::ThermalZone { name, children, .. } => named_package(
            &[0x5b, 0x85],
            encode_name_string(name)?,
            encode_items(children)?,
        ),
        DslItem::PowerResource {
            name,
            level,
            order,
            children,
            ..
        } => {
            let level_value = literal_u64(level)?;
            let order_value = literal_u64(order)?;
            if level_value > u8::MAX as u64 || order_value > u16::MAX as u64 {
                return Err(Error::new_spanned(
                    level.clone(),
                    "PowerResource level/order out of range",
                ));
            }
            let mut header = encode_name_string(name)?;
            header.raw([level_value as u8]);
            header.raw((order_value as u16).to_le_bytes());
            named_package(&[0x5b, 0x84], header, encode_items(children)?)
        }
    }
}

fn named_package(op: &[u8], mut name: Encoded, body: Encoded) -> Result<Encoded> {
    name.append(body);
    aml_package(op, name)
}

fn aml_package(op: &[u8], content: Encoded) -> Result<Encoded> {
    let mut out = Encoded {
        bytes: op.to_vec(),
        ..Encoded::default()
    };
    out.raw(pkg_length(content.bytes.len(), true)?);
    out.append(content);
    Ok(out)
}

fn op_args<const N: usize>(op: &[u8], args: [Encoded; N]) -> Result<Encoded> {
    let mut out = Encoded {
        bytes: op.to_vec(),
        ..Encoded::default()
    };
    args.into_iter().for_each(|arg| out.append(arg));
    Ok(out)
}

fn encode_name_operand(name: &NameOrInterp) -> Result<Encoded> {
    match name {
        NameOrInterp::Literal(name) => encode_name_string(name),
        NameOrInterp::Const(tokens) => {
            let operand = parse_const_string(tokens)?;
            encode_name_string(&operand)
        }
    }
}

fn encode_name_string(name: &str) -> Result<Encoded> {
    if name.is_empty() || name.starts_with("\\^") {
        return Err(Error::new(Span::call_site(), "invalid AML path"));
    }
    let mut bytes = Vec::new();
    let mut rest = name;
    if let Some(stripped) = rest.strip_prefix('\\') {
        bytes.push(b'\\');
        rest = stripped;
    }
    while let Some(stripped) = rest.strip_prefix('^') {
        bytes.push(b'^');
        rest = stripped;
    }
    if rest.is_empty() {
        bytes.push(0x00);
        return Ok(Encoded {
            bytes,
            ..Encoded::default()
        });
    }
    let segments: Vec<_> = rest.split('.').collect();
    if segments.len() > 255 {
        return Err(Error::new(
            Span::call_site(),
            "AML path exceeds 255 segments",
        ));
    }
    match segments.len() {
        1 => {}
        2 => bytes.push(0x2e),
        n => {
            bytes.push(0x2f);
            bytes.push(n as u8);
        }
    }
    for segment in segments {
        if segment.is_empty()
            || segment.as_bytes()[0].is_ascii_digit()
            || segment.len() > 4
            || !segment
                .bytes()
                .all(|b| b == b'_' || b.is_ascii_uppercase() || b.is_ascii_digit())
        {
            return Err(Error::new(
                Span::call_site(),
                format!("invalid ACPI NameSeg `{segment}`"),
            ));
        }
        bytes.extend_from_slice(segment.as_bytes());
        bytes.extend(core::iter::repeat_n(b'_', 4 - segment.len()));
    }
    Ok(Encoded {
        bytes,
        ..Encoded::default()
    })
}

fn encode_integer(value: u64) -> Encoded {
    match value {
        0 => Encoded::byte(0x00),
        1 => Encoded::byte(0x01),
        u64::MAX => Encoded::byte(0xff),
        2..=0xff => prefixed_integer(0x0a, value, 1),
        0x100..=0xffff => prefixed_integer(0x0b, value, 2),
        0x1_0000..=0xffff_ffff => prefixed_integer(0x0c, value, 4),
        _ => prefixed_integer(0x0e, value, 8),
    }
}

fn prefixed_integer(prefix: u8, value: u64, width: usize) -> Encoded {
    let mut bytes = vec![prefix];
    bytes.extend_from_slice(&value.to_le_bytes()[..width]);
    Encoded {
        bytes,
        ..Encoded::default()
    }
}

fn encode_operand(operand: &Operand) -> Result<Encoded> {
    match operand {
        Operand::Runtime { kind, expr } => {
            let (prefix, width) = kind_info(*kind);
            Ok(Encoded {
                bytes: core::iter::once(prefix)
                    .chain(core::iter::repeat_n(0, width))
                    .collect(),
                fixups: vec![FixupSpec {
                    offset: 1,
                    kind: *kind,
                    integer: true,
                    expr: expr.clone(),
                    action: None,
                }],
                const_patches: vec![],
            })
        }
        Operand::Const {
            kind: Some(kind),
            expr,
        } => {
            let (prefix, width) = kind_info(*kind);
            Ok(Encoded {
                bytes: core::iter::once(prefix)
                    .chain(core::iter::repeat_n(0, width))
                    .collect(),
                fixups: vec![],
                const_patches: vec![ConstPatch {
                    offset: 1,
                    kind: *kind,
                    integer: true,
                    expr: expr.clone(),
                }],
            })
        }
        Operand::Const { kind: None, expr } => {
            if let Ok(path) = syn::parse2::<LitStr>(expr.clone()) {
                encode_name_string(&path.value())
            } else {
                Ok(encode_integer(literal_u64(expr)?))
            }
        }
    }
}

fn kind_info(kind: OperandKind) -> (u8, usize) {
    match kind {
        OperandKind::Byte => (0x0a, 1),
        OperandKind::Word => (0x0b, 2),
        OperandKind::DWord => (0x0c, 4),
        OperandKind::QWord => (0x0e, 8),
    }
}

fn encode_value(value: &DslValue) -> Result<Encoded> {
    match value {
        DslValue::StringLit(value) => {
            if !value.is_ascii() || value.as_bytes().contains(&0) {
                return Err(Error::new(
                    Span::call_site(),
                    "AML strings must be ASCII without NUL",
                ));
            }
            let mut bytes = vec![0x0d];
            bytes.extend_from_slice(value.as_bytes());
            bytes.push(0);
            Ok(Encoded {
                bytes,
                ..Encoded::default()
            })
        }
        DslValue::IntLit(tokens) => Ok(encode_integer(literal_u64(tokens)?)),
        DslValue::EisaId(id) => Ok(encode_integer(encode_eisa_id(id)? as u64)),
        DslValue::Package(elements) => {
            if elements.len() > 255 {
                return Err(Error::new(
                    Span::call_site(),
                    "Package has more than 255 elements",
                ));
            }
            let mut content = Encoded::byte(elements.len() as u8);
            for element in elements {
                content.append(encode_value(element)?);
            }
            aml_package(&[0x12], content)
        }
        DslValue::ResourceTemplate(descs) => encode_resource_template(descs),
        DslValue::Operand(operand) => encode_operand(operand),
        DslValue::Expr(expr) => encode_expr(expr),
        DslValue::Buffer(values) => encode_buffer(values),
    }
}

fn encode_buffer(values: &[DslValue]) -> Result<Encoded> {
    let mut data = Encoded::default();
    for value in values {
        match value {
            DslValue::IntLit(tokens) => {
                let value = literal_u64(tokens)?;
                if value > 255 {
                    return Err(Error::new_spanned(
                        tokens.clone(),
                        "Buffer literal must fit in a byte",
                    ));
                }
                data.raw([value as u8]);
            }
            DslValue::Operand(Operand::Runtime { kind, expr }) => {
                let (_, width) = kind_info(*kind);
                let offset = data.bytes.len();
                data.raw(core::iter::repeat_n(0, width));
                data.fixups.push(FixupSpec {
                    offset,
                    kind: *kind,
                    integer: false,
                    expr: expr.clone(),
                    action: None,
                });
            }
            DslValue::Operand(Operand::Const {
                kind: Some(kind),
                expr,
            }) => {
                let (_, width) = kind_info(*kind);
                let offset = data.bytes.len();
                data.raw(core::iter::repeat_n(0, width));
                data.const_patches.push(ConstPatch {
                    offset,
                    kind: *kind,
                    integer: false,
                    expr: expr.clone(),
                });
            }
            DslValue::Operand(Operand::Const { kind: None, expr }) => {
                let value = literal_u64(expr)?;
                if value > 255 {
                    return Err(Error::new_spanned(
                        expr.clone(),
                        "Buffer const literal must fit in a byte",
                    ));
                }
                data.raw([value as u8]);
            }
            _ => {
                return Err(Error::new(
                    Span::call_site(),
                    "Buffer accepts byte literals and typed operands only",
                ));
            }
        }
    }
    let mut content = encode_integer(data.bytes.len() as u64);
    content.append(data);
    aml_package(&[0x11], content)
}

fn encode_expr(expr: &DslExpr) -> Result<Encoded> {
    match expr {
        DslExpr::IntLit(tokens) => Ok(encode_integer(literal_u64(tokens)?)),
        DslExpr::StringLit(value) => encode_value(&DslValue::StringLit(value.clone())),
        DslExpr::Path(path) => encode_name_string(path),
        DslExpr::Local(n) => Ok(Encoded::byte(0x60 + n)),
        DslExpr::Arg(n) => Ok(Encoded::byte(0x68 + n)),
        DslExpr::Zero => Ok(Encoded::byte(0)),
        DslExpr::One => Ok(Encoded::byte(1)),
        DslExpr::Ones => Ok(Encoded::byte(0xff)),
        DslExpr::Operand(operand) => encode_operand(operand),
        DslExpr::ToUUID(uuid) => encode_uuid(uuid),
        DslExpr::SizeOf(inner) => op_args(&[0x87], [encode_expr(inner)?]),
        DslExpr::DeRefOf(inner) => op_args(&[0x83], [encode_expr(inner)?]),
        DslExpr::CondRefOf(source, target) => {
            op_args(&[0x5b, 0x12], [encode_expr(source)?, encode_expr(target)?])
        }
        DslExpr::Index(source, index) => op_args(
            &[0x88],
            [encode_expr(source)?, encode_expr(index)?, Encoded::byte(0)],
        ),
        DslExpr::Call(name, args) => {
            let mut out = encode_name_string(name)?;
            for arg in args {
                out.append(encode_expr(arg)?);
            }
            Ok(out)
        }
        DslExpr::Binary(op, lhs, rhs) => encode_binary(*op, lhs, rhs),
        DslExpr::LNot(inner) => op_args(&[0x92], [encode_expr(inner)?]),
        DslExpr::BitNot(inner) => op_args(&[0x80], [encode_expr(inner)?, Encoded::byte(0)]),
    }
}

fn encode_assignment(target: &DslExpr, value: &DslExpr) -> Result<Encoded> {
    let DslExpr::Binary(op, lhs, rhs) = value else {
        return op_args(&[0x70], [encode_expr(value)?, encode_expr(target)?]);
    };
    let a = encode_expr(lhs)?;
    let b = encode_expr(rhs)?;
    let target = encode_expr(target)?;
    match op {
        BinaryOp::Add => op_args(&[0x72], [a, b, target]),
        BinaryOp::Subtract => op_args(&[0x74], [a, b, target]),
        BinaryOp::Multiply => op_args(&[0x77], [a, b, target]),
        BinaryOp::Divide => op_args(&[0x78], [a, b, Encoded::byte(0), target]),
        BinaryOp::Mod => op_args(&[0x85], [a, b, target]),
        BinaryOp::ShiftLeft => op_args(&[0x79], [a, b, target]),
        BinaryOp::ShiftRight => op_args(&[0x7a], [a, b, target]),
        BinaryOp::And => op_args(&[0x7b], [a, b, target]),
        BinaryOp::Or => op_args(&[0x7d], [a, b, target]),
        BinaryOp::Xor => op_args(&[0x7f], [a, b, target]),
        _ => op_args(&[0x70], [encode_binary(*op, lhs, rhs)?, target]),
    }
}

fn encode_binary(op: BinaryOp, lhs: &DslExpr, rhs: &DslExpr) -> Result<Encoded> {
    let a = encode_expr(lhs)?;
    let b = encode_expr(rhs)?;
    match op {
        BinaryOp::Add => op_args(&[0x72], [a, b, Encoded::byte(0)]),
        BinaryOp::Subtract => op_args(&[0x74], [a, b, Encoded::byte(0)]),
        BinaryOp::Multiply => op_args(&[0x77], [a, b, Encoded::byte(0)]),
        BinaryOp::Divide => op_args(&[0x78], [a, b, Encoded::byte(0), Encoded::byte(0)]),
        BinaryOp::Mod => op_args(&[0x85], [a, b, Encoded::byte(0)]),
        BinaryOp::ShiftLeft => op_args(&[0x79], [a, b, Encoded::byte(0)]),
        BinaryOp::ShiftRight => op_args(&[0x7a], [a, b, Encoded::byte(0)]),
        BinaryOp::And => op_args(&[0x7b], [a, b, Encoded::byte(0)]),
        BinaryOp::Or => op_args(&[0x7d], [a, b, Encoded::byte(0)]),
        BinaryOp::Xor => op_args(&[0x7f], [a, b, Encoded::byte(0)]),
        BinaryOp::LAnd => op_args(&[0x90], [a, b]),
        BinaryOp::LOr => op_args(&[0x91], [a, b]),
        BinaryOp::Equal => op_args(&[0x93], [a, b]),
        BinaryOp::Less => op_args(&[0x95], [a, b]),
        BinaryOp::Greater => op_args(&[0x94], [a, b]),
        BinaryOp::NotEqual => op_args(&[0x92], [op_args(&[0x93], [a, b])?]),
        BinaryOp::LessEqual => op_args(&[0x92], [op_args(&[0x94], [a, b])?]),
        BinaryOp::GreaterEqual => op_args(&[0x92], [op_args(&[0x95], [a, b])?]),
    }
}

fn encode_field(
    region: &str,
    access: FieldAccess,
    lock: FieldLock,
    update: FieldUpdate,
    entries: &[FieldEntryDsl],
) -> Result<Encoded> {
    let access = match access {
        FieldAccess::AnyAcc => 0,
        FieldAccess::ByteAcc => 1,
        FieldAccess::WordAcc => 2,
        FieldAccess::DWordAcc => 3,
        FieldAccess::QWordAcc => 4,
    };
    let lock = matches!(lock, FieldLock::Lock) as u8;
    let update = match update {
        FieldUpdate::Preserve => 0,
        FieldUpdate::WriteAsOnes => 1,
        FieldUpdate::WriteAsZeroes => 2,
    };
    let mut content = encode_name_string(region)?;
    content.raw([access | (lock << 4) | (update << 5)]);
    let mut bit_offset = 0usize;
    for entry in entries {
        match entry {
            FieldEntryDsl::Named(name, bits) => {
                content.append(encode_name_string(name)?);
                content.raw(pkg_length(*bits, false)?);
                bit_offset = bit_offset
                    .checked_add(*bits)
                    .ok_or_else(|| Error::new(Span::call_site(), "Field length overflow"))?;
            }
            FieldEntryDsl::Reserved(bits) => {
                content.raw([0]);
                content.raw(pkg_length(*bits, false)?);
                bit_offset = bit_offset
                    .checked_add(*bits)
                    .ok_or_else(|| Error::new(Span::call_site(), "Field length overflow"))?;
            }
            FieldEntryDsl::Offset(byte) => {
                let target = byte
                    .checked_mul(8)
                    .ok_or_else(|| Error::new(Span::call_site(), "Field Offset overflow"))?;
                if target < bit_offset {
                    return Err(Error::new(
                        Span::call_site(),
                        "Field Offset moves backwards",
                    ));
                }
                let gap = target - bit_offset;
                if gap != 0 {
                    content.raw([0]);
                    content.raw(pkg_length(gap, false)?);
                }
                bit_offset = target;
            }
        }
    }
    aml_package(&[0x5b, 0x81], content)
}

fn encode_resource_template(descs: &[ResourceDesc]) -> Result<Encoded> {
    let mut data = Encoded::default();
    for desc in descs {
        data.append(encode_resource(desc)?);
    }
    // EndTag checksum zero explicitly disables resource checksum validation
    // (ACPI 6.5 section 6.4.2.9), including after runtime field patches.
    data.raw([0x79, 0]);
    let mut content = encode_integer(data.bytes.len() as u64);
    content.append(data);
    aml_package(&[0x11], content)
}

fn encode_resource(desc: &ResourceDesc) -> Result<Encoded> {
    let mut out = Encoded::default();
    match desc {
        ResourceDesc::Memory32Fixed {
            read_write,
            base,
            size,
        } => {
            out.raw([0x86, 9, 0, u8::from(*read_write)]);
            out.append(raw_value(base, OperandKind::DWord)?);
            out.append(raw_value(size, OperandKind::DWord)?);
        }
        ResourceDesc::Interrupt {
            consumer,
            level,
            active_high,
            exclusive,
            irq,
        } => {
            let flags = u8::from(*consumer)
                | (u8::from(!*level) << 1)
                | (u8::from(!*active_high) << 2)
                | (u8::from(!*exclusive) << 3);
            out.raw([0x89, 6, 0, flags, 1]);
            out.append(raw_value(irq, OperandKind::DWord)?);
        }
        ResourceDesc::Irq {
            edge,
            active_high,
            exclusive,
            irq,
        } => {
            let irq = literal_value(irq)?;
            if irq > 15 {
                return Err(Error::new(Span::call_site(), "IRQ number must be 0-15"));
            }
            let flags =
                u8::from(*edge) | (u8::from(!*active_high) << 3) | (u8::from(!*exclusive) << 4);
            out.raw([
                0x23,
                (1u16 << irq).to_le_bytes()[0],
                (1u16 << irq).to_le_bytes()[1],
                flags,
            ]);
        }
        ResourceDesc::IrqNoFlags { irq } => {
            let irq = literal_value(irq)?;
            if irq > 15 {
                return Err(Error::new(Span::call_site(), "IRQ number must be 0-15"));
            }
            let mask = 1u16 << irq;
            out.raw([0x22, mask as u8, (mask >> 8) as u8]);
        }
        ResourceDesc::IoPort {
            base,
            end,
            align,
            len,
        } => {
            out.raw([0x47, 1]);
            out.append(raw_value(base, OperandKind::Word)?);
            out.append(raw_value(end, OperandKind::Word)?);
            out.append(raw_value(align, OperandKind::Byte)?);
            out.append(raw_value(len, OperandKind::Byte)?);
        }
        ResourceDesc::DWordIO { base, end } => {
            address_space(&mut out, 0x87, 1, 3, OperandKind::DWord, base, end)?
        }
        ResourceDesc::WordBusNumber { start, end } => {
            address_space(&mut out, 0x88, 2, 0, OperandKind::Word, start, end)?
        }
        ResourceDesc::DWordMemory {
            cacheable,
            read_write,
            base,
            end,
        } => address_space(
            &mut out,
            0x87,
            0,
            cache_flags(*cacheable, *read_write),
            OperandKind::DWord,
            base,
            end,
        )?,
        ResourceDesc::QWordMemory {
            cacheable,
            read_write,
            base,
            end,
        } => address_space(
            &mut out,
            0x8a,
            0,
            cache_flags(*cacheable, *read_write),
            OperandKind::QWord,
            base,
            end,
        )?,
    }
    Ok(out)
}

fn cache_flags(cacheable: CacheableKind, rw: bool) -> u8 {
    let cache = match cacheable {
        CacheableKind::NotCacheable => 0,
        CacheableKind::Cacheable => 1,
        CacheableKind::WriteCombining => 2,
        CacheableKind::Prefetchable => 3,
    };
    (cache << 1) | u8::from(rw)
}

fn address_space(
    out: &mut Encoded,
    tag: u8,
    ty: u8,
    type_flags: u8,
    kind: OperandKind,
    min: &DslValue,
    max: &DslValue,
) -> Result<()> {
    let (_, width) = kind_info(kind);
    let payload_len = (3 + 5 * width) as u16;
    out.raw([tag]);
    out.raw(payload_len.to_le_bytes());
    out.raw([ty, 0x0c, type_flags]);
    out.raw(core::iter::repeat_n(0, width));

    let min_fixup = out.fixups.len();
    out.append(raw_value(min, kind)?);
    let min_value = range_value(min, min_fixup)?;
    let max_fixup = out.fixups.len();
    out.append(raw_value(max, kind)?);
    let max_value = range_value(max, max_fixup)?;

    out.raw(core::iter::repeat_n(0, width));
    let length_offset = out.bytes.len();
    match (&min_value, &max_value) {
        (RangeValue::Literal(min), RangeValue::Literal(max)) => {
            let length = max
                .checked_sub(*min)
                .and_then(|value| value.checked_add(1))
                .ok_or_else(|| {
                    Error::new(Span::call_site(), "address resource range is invalid")
                })?;
            check_width(length, width)?;
            out.raw(length.to_le_bytes()[..width].iter().copied());
        }
        _ => {
            out.raw(core::iter::repeat_n(0, width));
            let action_fixup = out
                .fixups
                .len()
                .checked_sub(1)
                .expect("a dynamic range has at least one runtime operand");
            out.fixups[action_fixup].action = Some(RangeAction {
                offset: length_offset,
                min: min_value,
                max: max_value,
            });
        }
    }
    Ok(())
}

fn range_value(value: &DslValue, fixup_index: usize) -> Result<RangeValue> {
    match value {
        DslValue::IntLit(tokens) => literal_u64(tokens).map(RangeValue::Literal),
        DslValue::Operand(Operand::Const { kind: None, expr }) => {
            literal_u64(expr).map(RangeValue::Literal)
        }
        DslValue::Operand(Operand::Runtime { .. }) => Ok(RangeValue::Operand(fixup_index)),
        DslValue::Operand(Operand::Const { kind: Some(_), .. }) => Err(Error::new(
            Span::call_site(),
            "const expressions are not supported as address range bounds; use runtime operands",
        )),
        _ => Err(Error::new(
            Span::call_site(),
            "address range bounds must be integer literals or typed operands",
        )),
    }
}

fn check_width(value: u64, width: usize) -> Result<()> {
    if width < 8 && value >= 1u64 << (8 * width) {
        return Err(Error::new(
            Span::call_site(),
            "resource value exceeds descriptor width",
        ));
    }
    Ok(())
}

fn raw_value(value: &DslValue, expected: OperandKind) -> Result<Encoded> {
    let (_, width) = kind_info(expected);
    match value {
        DslValue::IntLit(tokens) => {
            let value = literal_u64(tokens)?;
            check_width(value, width)?;
            Ok(Encoded {
                bytes: value.to_le_bytes()[..width].to_vec(),
                ..Encoded::default()
            })
        }
        DslValue::Operand(Operand::Const { kind: None, expr }) => {
            let value = literal_u64(expr)?;
            check_width(value, width)?;
            Ok(Encoded {
                bytes: value.to_le_bytes()[..width].to_vec(),
                ..Encoded::default()
            })
        }
        DslValue::Operand(Operand::Const {
            kind: Some(kind),
            expr,
        }) if *kind == expected => Ok(Encoded {
            bytes: vec![0; width],
            fixups: vec![],
            const_patches: vec![ConstPatch {
                offset: 0,
                kind: *kind,
                integer: false,
                expr: expr.clone(),
            }],
        }),
        DslValue::Operand(Operand::Const { kind: Some(_), .. }) => Err(Error::new(
            Span::call_site(),
            "const resource operand width does not match descriptor field",
        )),
        DslValue::Operand(Operand::Runtime { kind, expr }) if *kind == expected => Ok(Encoded {
            bytes: vec![0; width],
            fixups: vec![FixupSpec {
                offset: 0,
                kind: *kind,
                integer: false,
                expr: expr.clone(),
                action: None,
            }],
            const_patches: vec![],
        }),
        DslValue::Operand(Operand::Runtime { .. }) => Err(Error::new(
            Span::call_site(),
            "runtime resource operand width does not match descriptor field",
        )),
        _ => Err(Error::new(
            Span::call_site(),
            "resource descriptor fields must be integer literals or typed operands",
        )),
    }
}

fn literal_value(value: &DslValue) -> Result<u64> {
    match value {
        DslValue::IntLit(tokens) => literal_u64(tokens),
        DslValue::Operand(Operand::Const { kind: None, expr }) => literal_u64(expr),
        _ => Err(Error::new(
            Span::call_site(),
            "this descriptor field must be an integer literal",
        )),
    }
}

fn literal_u64(tokens: &TokenStream) -> Result<u64> {
    let literal: LitInt = syn::parse2(tokens.clone()).map_err(|_| Error::new_spanned(tokens.clone(),
        "named const expressions require an explicit width: #{const byte EXPR}, #{const word EXPR}, #{const dword EXPR}, or #{const qword EXPR}"))?;
    literal.base10_parse::<u64>()
}

fn parse_const_string(tokens: &TokenStream) -> Result<String> {
    let operand = match crate::parse::parse_operand_tokens(tokens.clone())? {
        Operand::Const { kind: None, expr } => expr,
        _ => {
            return Err(Error::new_spanned(
                tokens.clone(),
                "names require #{const \"PATH\"}",
            ));
        }
    };
    syn::parse2::<LitStr>(operand).map(|s| s.value()).map_err(|_| Error::new_spanned(tokens.clone(),
        "const name operands currently require a string literal, for example #{const \"^PARN.FOO\"}"))
}

fn encode_eisa_id(id: &str) -> Result<u32> {
    let bytes = id.as_bytes();
    if bytes.len() != 7
        || !bytes[..3].iter().all(|byte| byte.is_ascii_uppercase())
        || !bytes[3..].iter().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(Error::new(
            Span::call_site(),
            "EisaId must contain three uppercase letters followed by four hexadecimal digits",
        ));
    }
    let digit = |byte: u8| match byte {
        b'0'..=b'9' => byte - b'0',
        b'A'..=b'F' => byte - b'A' + 10,
        b'a'..=b'f' => byte - b'a' + 10,
        _ => unreachable!(),
    };
    let value = (u32::from(bytes[0] - 0x40) << 26)
        | (u32::from(bytes[1] - 0x40) << 21)
        | (u32::from(bytes[2] - 0x40) << 16)
        | (u32::from(digit(bytes[3])) << 12)
        | (u32::from(digit(bytes[4])) << 8)
        | (u32::from(digit(bytes[5])) << 4)
        | u32::from(digit(bytes[6]));
    Ok(value.swap_bytes())
}

fn encode_uuid(uuid: &str) -> Result<Encoded> {
    let compact: String = uuid.chars().filter(|c| *c != '-').collect();
    if compact.len() != 32 || !compact.is_ascii() {
        return Err(Error::new(
            Span::call_site(),
            "UUID must contain 32 hexadecimal digits",
        ));
    }
    let mut raw = [0u8; 16];
    for (i, byte) in raw.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&compact[i * 2..i * 2 + 2], 16)
            .map_err(|_| Error::new(Span::call_site(), "invalid UUID"))?;
    }
    raw[..4].reverse();
    raw[4..6].reverse();
    raw[6..8].reverse();
    encode_buffer(
        &raw.iter()
            .map(|v| DslValue::IntLit(v.to_string().parse().unwrap()))
            .collect::<Vec<_>>(),
    )
}

fn pkg_length(content_len: usize, include_self: bool) -> Result<Vec<u8>> {
    let (count, value) = (1..=4)
        .find_map(|count| {
            let value = content_len.checked_add(if include_self { count } else { 0 })?;
            let limit = if count == 1 {
                0x40
            } else {
                1usize << (4 + 8 * (count - 1))
            };
            (value < limit).then_some((count, value))
        })
        .ok_or_else(|| Error::new(Span::call_site(), "AML PkgLength exceeds 28 bits"))?;
    if count == 1 {
        return Ok(vec![value as u8]);
    }
    let mut out = vec![((count as u8 - 1) << 6) | (value as u8 & 0x0f)];
    for shift in (4..4 + 8 * (count - 1)).step_by(8) {
        out.push((value >> shift) as u8);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integer_prefixes_are_shortest() {
        assert_eq!(encode_integer(0).bytes, [0]);
        assert_eq!(encode_integer(1).bytes, [1]);
        assert_eq!(encode_integer(0xff).bytes, [0x0a, 0xff]);
        assert_eq!(encode_integer(0x100).bytes, [0x0b, 0, 1]);
        assert_eq!(encode_integer(0x1_0000).bytes[0], 0x0c);
        assert_eq!(encode_integer(0x1_0000_0000).bytes[0], 0x0e);
        assert_eq!(encode_integer(u64::MAX).bytes, [0xff]);
    }
    #[test]
    fn package_lengths_cover_boundaries() {
        assert_eq!(pkg_length(62, true).unwrap(), [63]);
        assert_eq!(pkg_length(63, true).unwrap(), [0x41, 4]);
        assert_eq!(pkg_length(4093, true).unwrap().len(), 2);
        assert_eq!(pkg_length(4094, true).unwrap().len(), 3);
        assert_eq!(pkg_length(64, false).unwrap(), [0x40, 4]);
    }
    #[test]
    fn oversized_lengths_paths_and_resource_literals_are_rejected() {
        assert!(pkg_length(0x1000_0000, false).is_err());
        assert!(pkg_length(usize::MAX, true).is_err());
        assert_eq!(
            pkg_length(0x0fff_ffff, false).unwrap(),
            [0xcf, 0xff, 0xff, 0xff]
        );
        for path in ["", "1ABC", "A..B", "\\^A", "A.", "a"] {
            assert!(encode_name_string(path).is_err(), "{path}");
        }
        assert!(encode_name_string(&vec!["A"; 256].join(".")).is_err());
        for source in [
            quote!(Name("RSC0", ResourceTemplate { WordBusNumber(0u16, 65535u16); });),
            quote!(Name("RSC0", ResourceTemplate { DWordIO(0u32, 0xffff_ffffu32); });),
            quote!(Name("RSC0", ResourceTemplate { IO(65536u32, 65536u32, 1u8, 8u8); });),
            quote!(Name("RSC0", ResourceTemplate { Memory32Fixed(ReadWrite, 0x100000000u64, 1u32); });),
            quote!(Field("REGN", ByteAcc, NoLock, Preserve) { HUGE, 268435456, }),
        ] {
            let items = crate::parse::parse_dsl(source).unwrap();
            assert!(encode_items(&items).is_err());
        }
    }

    #[test]
    fn name_strings_encode_prefixes_and_padding() {
        assert_eq!(encode_name_string("\\").unwrap().bytes, [b'\\', 0x00]);
        assert_eq!(encode_name_string("^").unwrap().bytes, [b'^', 0x00]);
        assert_eq!(encode_name_string("ABC").unwrap().bytes, b"ABC_");
        assert_eq!(
            encode_name_string("A.B").unwrap().bytes,
            [vec![0x2e], b"A___".to_vec(), b"B___".to_vec()].concat()
        );
        assert_eq!(
            encode_name_string("^PARN.FOO").unwrap().bytes,
            [vec![b'^', 0x2e], b"PARN".to_vec(), b"FOO_".to_vec()].concat()
        );
        assert_eq!(
            encode_name_string("\\_SB_.PCI0.LPCB").unwrap().bytes[..3],
            [b'\\', 0x2f, 3]
        );
    }
}
