//! DSL token stream parser.
//!
//! Parses the `acpi_dsl!` input into an AST of [`DslItem`] nodes.
//! The grammar is:
//!
//! ```text
//! items       = item*
//! item        = scope | device | name_decl | method | ret
//! scope       = "scope" "(" STRING ")" "{" items "}"
//! device      = "device" "(" STRING ")" "{" items "}"
//! name_decl   = "name" "(" STRING "," value ")" ";"
//! method      = "method" "(" STRING "," INT "," serialized ")" "{" items "}"
//! ret         = "ret" "(" value ")" ";"
//! value       = resource_template | eisa_id | interpolation | literal
//! resource_template = "resource_template" "{" resource_desc* "}"
//! resource_desc = memory_32_fixed | interrupt
//! interpolation = "#{" expr "}"
//! ```

use proc_macro2::{Delimiter, Span, TokenStream, TokenTree};
use syn::{Error, Result};

/// A name that is either a literal string or an interpolated expression.
#[derive(Debug)]
pub enum NameOrInterp {
    /// Literal ACPI name string (e.g., `"COM0"`).
    Literal(String),
    /// Interpolated Rust expression (e.g., `#{name}`).
    Interpolation(TokenStream),
}

/// A parsed DSL item.
#[derive(Debug)]
pub enum DslItem {
    Scope {
        path: NameOrInterp,
        children: Vec<DslItem>,
        span: Span,
    },
    Device {
        name: NameOrInterp,
        children: Vec<DslItem>,
        span: Span,
    },
    Name {
        name: String,
        value: DslValue,
        span: Span,
    },
    Method {
        name: String,
        argc: u8,
        serialized: bool,
        body: Vec<DslItem>,
        span: Span,
    },
    Return {
        value: DslValue,
        #[allow(dead_code)]
        span: Span,
    },
    /// `op_region("NAME", space, offset, length);`
    OpRegion {
        name: String,
        space: RegionSpace,
        offset: DslValue,
        length: DslValue,
        span: Span,
    },
    /// `field("REGION", access, lock, update) { entries }`
    Field {
        region: String,
        access: FieldAccess,
        lock: FieldLock,
        update: FieldUpdate,
        entries: Vec<FieldEntryDsl>,
        span: Span,
    },
    /// `create_dword_field(buffer, index, "NAME");`
    CreateDwordField {
        buffer: DslValue,
        index: DslValue,
        name: String,
        span: Span,
    },
    /// `store(value, target);`
    Store {
        value: DslValue,
        target: DslValue,
        #[allow(dead_code)]
        span: Span,
    },
    /// `shl(target, value, count);`
    ShiftLeft {
        target: DslValue,
        value: DslValue,
        count: DslValue,
        #[allow(dead_code)]
        span: Span,
    },
    /// `subtract(target, a, b);`
    Subtract {
        target: DslValue,
        a: DslValue,
        b: DslValue,
        #[allow(dead_code)]
        span: Span,
    },
    /// `add(target, a, b);`
    Add {
        target: DslValue,
        a: DslValue,
        b: DslValue,
        #[allow(dead_code)]
        span: Span,
    },
}

/// OperationRegion address space.
#[derive(Debug, Clone, Copy)]
pub enum RegionSpace {
    SystemMemory,
    SystemIO,
    PciConfig,
    EmbeddedControl,
}

/// Field access type.
#[derive(Debug, Clone, Copy)]
#[allow(clippy::enum_variant_names)] // Names match ACPI spec terminology
pub enum FieldAccess {
    AnyAcc,
    ByteAcc,
    WordAcc,
    DWordAcc,
    QWordAcc,
}

/// Field lock rule.
#[derive(Debug, Clone, Copy)]
pub enum FieldLock {
    NoLock,
    Lock,
}

/// Field update rule.
#[derive(Debug, Clone, Copy)]
pub enum FieldUpdate {
    Preserve,
    WriteAsOnes,
    WriteAsZeroes,
}

/// A field entry: named field, reserved (gap), or offset directive.
#[derive(Debug)]
pub enum FieldEntryDsl {
    /// `NAME, bits,` -- named bitfield
    Named(String, usize),
    /// `, bits,` -- anonymous reserved gap
    Reserved(usize),
    /// `offset(byte_offset),` -- jump to a byte offset (emits a gap)
    Offset(usize),
}

/// A parsed value expression.
#[derive(Debug)]
pub enum DslValue {
    /// String literal: `"ARMH0011"`
    StringLit(String),
    /// Integer literal: `0u32`, `0x1000u32`
    IntLit(TokenStream),
    /// EISA ID: `eisa_id("PNP0501")`
    EisaId(String),
    /// Package: `package(val1, val2, ...)`
    Package(Vec<DslValue>),
    /// Resource template: `resource_template { ... }`
    ResourceTemplate(Vec<ResourceDesc>),
    /// Rust expression interpolation: `#{expr}`
    Interpolation(TokenStream),
}

/// A resource descriptor within a resource_template.
#[derive(Debug)]
pub enum ResourceDesc {
    Memory32Fixed {
        read_write: bool,
        base: DslValue,
        size: DslValue,
    },
    Interrupt {
        consumer: bool,
        level: bool,
        active_high: bool,
        exclusive: bool,
        irq: DslValue,
    },
    /// `io(base, end, align, len);` -- legacy I/O port descriptor.
    IoPort {
        base: DslValue,
        end: DslValue,
        align: DslValue,
        len: DslValue,
    },
    /// `dword_io(base, end);` -- DWord I/O range.
    DWordIO { base: DslValue, end: DslValue },
    /// WordBusNumber -- PCI bus number range.
    WordBusNumber { start: DslValue, end: DslValue },
    /// DWordMemory -- 32-bit MMIO address range.
    DWordMemory {
        cacheable: CacheableKind,
        read_write: bool,
        base: DslValue,
        end: DslValue,
    },
    /// QWordMemory -- 64-bit MMIO address range.
    QWordMemory {
        cacheable: CacheableKind,
        read_write: bool,
        base: DslValue,
        end: DslValue,
    },
}

/// AddressSpace cacheability attribute.
#[derive(Debug, Clone, Copy)]
pub enum CacheableKind {
    NotCacheable,
    Cacheable,
    WriteCombining,
    Prefetchable,
}

/// Parse the top-level DSL input.
pub fn parse_dsl(input: TokenStream) -> Result<Vec<DslItem>> {
    let mut parser = Parser::new(input);
    parser.parse_items()
}

struct Parser {
    tokens: Vec<TokenTree>,
    pos: usize,
}

impl Parser {
    fn new(input: TokenStream) -> Self {
        let tokens: Vec<TokenTree> = input.into_iter().collect();
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&TokenTree> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<TokenTree> {
        if self.pos < self.tokens.len() {
            let tt = self.tokens[self.pos].clone();
            self.pos += 1;
            Some(tt)
        } else {
            None
        }
    }

    fn span(&self) -> Span {
        self.tokens
            .get(self.pos)
            .map(|t| t.span())
            .unwrap_or_else(Span::call_site)
    }

    fn expect_ident(&mut self, expected: &str) -> Result<Span> {
        match self.advance() {
            Some(TokenTree::Ident(ident)) if ident == expected => Ok(ident.span()),
            Some(other) => Err(Error::new(other.span(), format!("expected `{expected}`"))),
            None => Err(Error::new(
                self.span(),
                format!("expected `{expected}`, found end of input"),
            )),
        }
    }

    fn expect_punct(&mut self, ch: char) -> Result<()> {
        match self.advance() {
            Some(TokenTree::Punct(p)) if p.as_char() == ch => Ok(()),
            Some(other) => Err(Error::new(other.span(), format!("expected `{ch}`"))),
            None => Err(Error::new(self.span(), format!("expected `{ch}`"))),
        }
    }

    fn expect_group(&mut self, delim: Delimiter) -> Result<(TokenStream, Span)> {
        match self.advance() {
            Some(TokenTree::Group(g)) if g.delimiter() == delim => Ok((g.stream(), g.span())),
            Some(other) => {
                let name = match delim {
                    Delimiter::Parenthesis => "(..)",
                    Delimiter::Brace => "{..}",
                    Delimiter::Bracket => "[..]",
                    Delimiter::None => "group",
                };
                Err(Error::new(other.span(), format!("expected `{name}`")))
            }
            None => Err(Error::new(self.span(), "unexpected end of input")),
        }
    }

    fn expect_string_lit(&mut self) -> Result<String> {
        match self.advance() {
            Some(TokenTree::Literal(lit)) => {
                // Use syn::LitStr to properly unescape the string literal.
                let lit_str: syn::LitStr = syn::parse2(TokenTree::Literal(lit.clone()).into())?;
                Ok(lit_str.value())
            }
            Some(other) => Err(Error::new(other.span(), "expected string literal")),
            None => Err(Error::new(self.span(), "expected string literal")),
        }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn peek_ident_eq(&self, name: &str) -> bool {
        matches!(self.peek(), Some(TokenTree::Ident(i)) if *i == name)
    }

    fn parse_items(&mut self) -> Result<Vec<DslItem>> {
        let mut items = Vec::new();
        while !self.at_end() {
            items.push(self.parse_item()?);
        }
        Ok(items)
    }

    fn parse_item(&mut self) -> Result<DslItem> {
        let tt = self
            .peek()
            .ok_or_else(|| Error::new(self.span(), "unexpected end of input"))?;
        match tt {
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                match name.as_str() {
                    "scope" => self.parse_scope(),
                    "device" => self.parse_device(),
                    "name" => self.parse_name(),
                    "method" => self.parse_method(),
                    "ret" => self.parse_return(),
                    "op_region" => self.parse_op_region(),
                    "field" => self.parse_field(),
                    "create_dword_field" => self.parse_create_dword_field(),
                    "store" => self.parse_store(),
                    "shl" => self.parse_shift_left(),
                    "subtract" => self.parse_subtract(),
                    "add" => self.parse_add(),
                    _ => Err(Error::new(
                        ident.span(),
                        format!("unknown DSL keyword `{name}`"),
                    )),
                }
            }
            other => Err(Error::new(other.span(), "expected DSL keyword")),
        }
    }

    fn parse_scope(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("scope")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let path = args_parser.parse_name_or_interp()?;

        let (body, _) = self.expect_group(Delimiter::Brace)?;
        let mut body_parser = Parser::new(body);
        let children = body_parser.parse_items()?;

        Ok(DslItem::Scope {
            path,
            children,
            span,
        })
    }

    /// Parse a name argument that is either a string literal or `#{expr}`.
    fn parse_name_or_interp(&mut self) -> Result<NameOrInterp> {
        if matches!(self.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '#') {
            self.expect_punct('#')?;
            let (expr, _) = self.expect_group(Delimiter::Brace)?;
            Ok(NameOrInterp::Interpolation(expr))
        } else {
            let s = self.expect_string_lit()?;
            Ok(NameOrInterp::Literal(s))
        }
    }

    fn parse_device(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("device")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let name = args_parser.parse_name_or_interp()?;

        let (body, _) = self.expect_group(Delimiter::Brace)?;
        let mut body_parser = Parser::new(body);
        let children = body_parser.parse_items()?;

        Ok(DslItem::Device {
            name,
            children,
            span,
        })
    }

    fn parse_name(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("name")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let name = args_parser.expect_string_lit()?;
        args_parser.expect_punct(',')?;
        let value = args_parser.parse_value();
        self.expect_punct(';')?;

        Ok(DslItem::Name { name, value, span })
    }

    fn parse_method(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("method")?;
        let (args, args_span) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let name = args_parser.expect_string_lit()?;
        args_parser.expect_punct(',')?;

        // argc
        let argc_tt = args_parser
            .advance()
            .ok_or_else(|| Error::new(args_span, "expected argument count"))?;
        let argc: u8 = match &argc_tt {
            TokenTree::Literal(lit) => {
                let s = lit.to_string();
                s.parse()
                    .map_err(|_| Error::new(lit.span(), "expected integer 0-7"))?
            }
            _ => return Err(Error::new(argc_tt.span(), "expected integer")),
        };
        args_parser.expect_punct(',')?;

        // Serialized|NotSerialized
        let ser_tt = args_parser
            .advance()
            .ok_or_else(|| Error::new(args_span, "expected Serialized or NotSerialized"))?;
        let serialized = match &ser_tt {
            TokenTree::Ident(i) if *i == "Serialized" => true,
            TokenTree::Ident(i) if *i == "NotSerialized" => false,
            _ => {
                return Err(Error::new(
                    ser_tt.span(),
                    "expected `Serialized` or `NotSerialized`",
                ))
            }
        };

        let (body, _) = self.expect_group(Delimiter::Brace)?;
        let mut body_parser = Parser::new(body);
        let body_items = body_parser.parse_items()?;

        Ok(DslItem::Method {
            name,
            argc,
            serialized,
            body: body_items,
            span,
        })
    }

    fn parse_return(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("ret")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let value = args_parser.parse_value();
        self.expect_punct(';')?;

        Ok(DslItem::Return { value, span })
    }

    fn parse_value(&mut self) -> DslValue {
        // Check for special value forms
        if self.peek_ident_eq("eisa_id") {
            return self
                .parse_eisa_id()
                .unwrap_or_else(|e| DslValue::Interpolation(e.to_compile_error()));
        }

        if self.peek_ident_eq("package") {
            return self
                .parse_package()
                .unwrap_or_else(|e| DslValue::Interpolation(e.to_compile_error()));
        }

        if self.peek_ident_eq("resource_template") {
            return self
                .parse_resource_template()
                .unwrap_or_else(|e| DslValue::Interpolation(e.to_compile_error()));
        }

        // Check for #{expr} interpolation
        if matches!(self.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '#') {
            return self
                .parse_interpolation()
                .unwrap_or_else(|e| DslValue::Interpolation(e.to_compile_error()));
        }

        // String or integer literal
        match self.peek() {
            Some(TokenTree::Literal(lit)) => {
                let s = lit.to_string();
                let lit_clone = lit.clone();
                self.advance();
                if s.starts_with('"') {
                    // Use syn::LitStr to properly unescape.
                    let lit_str: syn::LitStr = syn::parse2(TokenTree::Literal(lit_clone).into())
                        .expect("already checked starts with '\"'");
                    DslValue::StringLit(lit_str.value())
                } else {
                    DslValue::IntLit(TokenTree::Literal(lit_clone).into())
                }
            }
            _ => {
                // Collect remaining tokens as an expression
                let mut expr = TokenStream::new();
                while !self.at_end() {
                    if matches!(self.peek(), Some(TokenTree::Punct(p)) if p.as_char() == ',' || p.as_char() == ')')
                    {
                        break;
                    }
                    expr.extend(self.advance());
                }
                DslValue::Interpolation(expr)
            }
        }
    }

    fn parse_package(&mut self) -> Result<DslValue> {
        self.expect_ident("package")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let mut elements = Vec::new();
        while !args_parser.at_end() {
            elements.push(args_parser.parse_value());
            // Consume optional trailing comma.
            if !args_parser.at_end() {
                let _ = args_parser.expect_punct(',');
            }
        }
        Ok(DslValue::Package(elements))
    }

    fn parse_eisa_id(&mut self) -> Result<DslValue> {
        self.expect_ident("eisa_id")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let id = args_parser.expect_string_lit()?;
        Ok(DslValue::EisaId(id))
    }

    fn parse_resource_template(&mut self) -> Result<DslValue> {
        self.expect_ident("resource_template")?;
        let (body, _) = self.expect_group(Delimiter::Brace)?;
        let mut body_parser = Parser::new(body);
        let mut descs = Vec::new();
        while !body_parser.at_end() {
            descs.push(body_parser.parse_resource_desc()?);
        }
        Ok(DslValue::ResourceTemplate(descs))
    }

    fn parse_resource_desc(&mut self) -> Result<ResourceDesc> {
        let tt = self
            .peek()
            .ok_or_else(|| Error::new(self.span(), "expected resource descriptor"))?;
        match tt {
            TokenTree::Ident(i) if *i == "memory_32_fixed" => self.parse_memory_32_fixed(),
            TokenTree::Ident(i) if *i == "interrupt" => self.parse_interrupt(),
            TokenTree::Ident(i) if *i == "io" => self.parse_io_port(),
            TokenTree::Ident(i) if *i == "dword_io" => self.parse_dword_io(),
            TokenTree::Ident(i) if *i == "word_bus_number" => self.parse_word_bus_number(),
            TokenTree::Ident(i) if *i == "dword_memory" => self.parse_dword_memory(),
            TokenTree::Ident(i) if *i == "qword_memory" => self.parse_qword_memory(),
            other => Err(Error::new(
                other.span(),
                "expected resource descriptor (memory_32_fixed, interrupt, \
                 word_bus_number, dword_memory, qword_memory)",
            )),
        }
    }

    fn parse_memory_32_fixed(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("memory_32_fixed")?;
        let (args, args_span) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);

        let rw_tt = p
            .advance()
            .ok_or_else(|| Error::new(args_span, "expected ReadWrite or ReadOnly"))?;
        let read_write = match &rw_tt {
            TokenTree::Ident(i) if *i == "ReadWrite" => true,
            TokenTree::Ident(i) if *i == "ReadOnly" => false,
            _ => {
                return Err(Error::new(
                    rw_tt.span(),
                    "expected `ReadWrite` or `ReadOnly`",
                ))
            }
        };
        p.expect_punct(',')?;
        let base = p.parse_value();
        p.expect_punct(',')?;
        let size = p.parse_value();

        self.expect_punct(';')?;

        Ok(ResourceDesc::Memory32Fixed {
            read_write,
            base,
            size,
        })
    }

    fn parse_interrupt(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("interrupt")?;
        let (args, args_span) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);

        // consumer: ResourceConsumer | ResourceProducer
        let cons_tt = p.advance().ok_or_else(|| {
            Error::new(args_span, "expected ResourceConsumer or ResourceProducer")
        })?;
        let consumer = match &cons_tt {
            TokenTree::Ident(i) if *i == "ResourceConsumer" => true,
            TokenTree::Ident(i) if *i == "ResourceProducer" => false,
            _ => {
                return Err(Error::new(
                    cons_tt.span(),
                    "expected `ResourceConsumer` or `ResourceProducer`",
                ))
            }
        };
        p.expect_punct(',')?;

        // level: Level | Edge
        let lvl_tt = p
            .advance()
            .ok_or_else(|| Error::new(args_span, "expected Level or Edge"))?;
        let level = match &lvl_tt {
            TokenTree::Ident(i) if *i == "Level" => true,
            TokenTree::Ident(i) if *i == "Edge" => false,
            _ => return Err(Error::new(lvl_tt.span(), "expected `Level` or `Edge`")),
        };
        p.expect_punct(',')?;

        // polarity: ActiveHigh | ActiveLow
        let pol_tt = p
            .advance()
            .ok_or_else(|| Error::new(args_span, "expected ActiveHigh or ActiveLow"))?;
        let active_high = match &pol_tt {
            TokenTree::Ident(i) if *i == "ActiveHigh" => true,
            TokenTree::Ident(i) if *i == "ActiveLow" => false,
            _ => {
                return Err(Error::new(
                    pol_tt.span(),
                    "expected `ActiveHigh` or `ActiveLow`",
                ))
            }
        };
        p.expect_punct(',')?;

        // sharing: Exclusive | Shared
        let share_tt = p
            .advance()
            .ok_or_else(|| Error::new(args_span, "expected Exclusive or Shared"))?;
        let exclusive = match &share_tt {
            TokenTree::Ident(i) if *i == "Exclusive" => true,
            TokenTree::Ident(i) if *i == "Shared" => false,
            _ => {
                return Err(Error::new(
                    share_tt.span(),
                    "expected `Exclusive` or `Shared`",
                ))
            }
        };
        p.expect_punct(',')?;

        let irq = p.parse_value();

        self.expect_punct(';')?;

        Ok(ResourceDesc::Interrupt {
            consumer,
            level,
            active_high,
            exclusive,
            irq,
        })
    }

    /// `io(base, end, align, len);`
    fn parse_io_port(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("io")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let base = p.parse_value();
        p.expect_punct(',')?;
        let end = p.parse_value();
        p.expect_punct(',')?;
        let align = p.parse_value();
        p.expect_punct(',')?;
        let len = p.parse_value();
        self.expect_punct(';')?;
        Ok(ResourceDesc::IoPort {
            base,
            end,
            align,
            len,
        })
    }

    /// `dword_io(base, end);`
    fn parse_dword_io(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("dword_io")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let base = p.parse_value();
        p.expect_punct(',')?;
        let end = p.parse_value();
        self.expect_punct(';')?;
        Ok(ResourceDesc::DWordIO { base, end })
    }

    /// `word_bus_number(start, end);`
    fn parse_word_bus_number(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("word_bus_number")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let start = p.parse_value();
        p.expect_punct(',')?;
        let end = p.parse_value();
        self.expect_punct(';')?;
        Ok(ResourceDesc::WordBusNumber { start, end })
    }

    /// `dword_memory(Cacheable, ReadWrite, base, end);`
    fn parse_dword_memory(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("dword_memory")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let cacheable = p.parse_cacheable()?;
        p.expect_punct(',')?;
        let read_write = p.parse_read_write()?;
        p.expect_punct(',')?;
        let base = p.parse_value();
        p.expect_punct(',')?;
        let end = p.parse_value();
        self.expect_punct(';')?;
        Ok(ResourceDesc::DWordMemory {
            cacheable,
            read_write,
            base,
            end,
        })
    }

    /// `qword_memory(Cacheable, ReadWrite, base, end);`
    fn parse_qword_memory(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("qword_memory")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let cacheable = p.parse_cacheable()?;
        p.expect_punct(',')?;
        let read_write = p.parse_read_write()?;
        p.expect_punct(',')?;
        let base = p.parse_value();
        p.expect_punct(',')?;
        let end = p.parse_value();
        self.expect_punct(';')?;
        Ok(ResourceDesc::QWordMemory {
            cacheable,
            read_write,
            base,
            end,
        })
    }

    fn parse_cacheable(&mut self) -> Result<CacheableKind> {
        let tt = self
            .advance()
            .ok_or_else(|| Error::new(self.span(), "expected cacheability"))?;
        match &tt {
            TokenTree::Ident(i) if *i == "NotCacheable" => Ok(CacheableKind::NotCacheable),
            TokenTree::Ident(i) if *i == "Cacheable" => Ok(CacheableKind::Cacheable),
            TokenTree::Ident(i) if *i == "WriteCombining" => Ok(CacheableKind::WriteCombining),
            TokenTree::Ident(i) if *i == "Prefetchable" => Ok(CacheableKind::Prefetchable),
            _ => Err(Error::new(
                tt.span(),
                "expected NotCacheable, Cacheable, WriteCombining, or Prefetchable",
            )),
        }
    }

    fn parse_read_write(&mut self) -> Result<bool> {
        let tt = self
            .advance()
            .ok_or_else(|| Error::new(self.span(), "expected ReadWrite or ReadOnly"))?;
        match &tt {
            TokenTree::Ident(i) if *i == "ReadWrite" => Ok(true),
            TokenTree::Ident(i) if *i == "ReadOnly" => Ok(false),
            _ => Err(Error::new(tt.span(), "expected `ReadWrite` or `ReadOnly`")),
        }
    }

    // ---------------------------------------------------------------
    // op_region("NAME", PciConfig, offset, length);
    // ---------------------------------------------------------------
    fn parse_op_region(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("op_region")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let name = p.expect_string_lit()?;
        p.expect_punct(',')?;
        let space = p.parse_region_space()?;
        p.expect_punct(',')?;
        let offset = p.parse_value();
        p.expect_punct(',')?;
        let length = p.parse_value();
        self.expect_punct(';')?;
        Ok(DslItem::OpRegion {
            name,
            space,
            offset,
            length,
            span,
        })
    }

    fn parse_region_space(&mut self) -> Result<RegionSpace> {
        let tt = self
            .advance()
            .ok_or_else(|| Error::new(self.span(), "expected region space"))?;
        match &tt {
            TokenTree::Ident(i) if *i == "SystemMemory" => Ok(RegionSpace::SystemMemory),
            TokenTree::Ident(i) if *i == "SystemIO" => Ok(RegionSpace::SystemIO),
            TokenTree::Ident(i) if *i == "PciConfig" => Ok(RegionSpace::PciConfig),
            TokenTree::Ident(i) if *i == "EmbeddedControl" => Ok(RegionSpace::EmbeddedControl),
            _ => Err(Error::new(
                tt.span(),
                "expected SystemMemory, SystemIO, PciConfig, or EmbeddedControl",
            )),
        }
    }

    // ---------------------------------------------------------------
    // field("REGION", DWordAcc, NoLock, Preserve) { entries }
    // ---------------------------------------------------------------
    fn parse_field(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("field")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let region = p.expect_string_lit()?;
        p.expect_punct(',')?;
        let access = p.parse_field_access()?;
        p.expect_punct(',')?;
        let lock = p.parse_field_lock()?;
        p.expect_punct(',')?;
        let update = p.parse_field_update()?;

        let (body, _) = self.expect_group(Delimiter::Brace)?;
        let entries = Self::parse_field_entries(body)?;

        Ok(DslItem::Field {
            region,
            access,
            lock,
            update,
            entries,
            span,
        })
    }

    fn parse_field_access(&mut self) -> Result<FieldAccess> {
        let tt = self
            .advance()
            .ok_or_else(|| Error::new(self.span(), "expected field access type"))?;
        match &tt {
            TokenTree::Ident(i) if *i == "AnyAcc" => Ok(FieldAccess::AnyAcc),
            TokenTree::Ident(i) if *i == "ByteAcc" => Ok(FieldAccess::ByteAcc),
            TokenTree::Ident(i) if *i == "WordAcc" => Ok(FieldAccess::WordAcc),
            TokenTree::Ident(i) if *i == "DWordAcc" => Ok(FieldAccess::DWordAcc),
            TokenTree::Ident(i) if *i == "QWordAcc" => Ok(FieldAccess::QWordAcc),
            _ => Err(Error::new(
                tt.span(),
                "expected AnyAcc, ByteAcc, WordAcc, DWordAcc, or QWordAcc",
            )),
        }
    }

    fn parse_field_lock(&mut self) -> Result<FieldLock> {
        let tt = self
            .advance()
            .ok_or_else(|| Error::new(self.span(), "expected field lock rule"))?;
        match &tt {
            TokenTree::Ident(i) if *i == "NoLock" => Ok(FieldLock::NoLock),
            TokenTree::Ident(i) if *i == "Lock" => Ok(FieldLock::Lock),
            _ => Err(Error::new(tt.span(), "expected NoLock or Lock")),
        }
    }

    fn parse_field_update(&mut self) -> Result<FieldUpdate> {
        let tt = self
            .advance()
            .ok_or_else(|| Error::new(self.span(), "expected field update rule"))?;
        match &tt {
            TokenTree::Ident(i) if *i == "Preserve" => Ok(FieldUpdate::Preserve),
            TokenTree::Ident(i) if *i == "WriteAsOnes" => Ok(FieldUpdate::WriteAsOnes),
            TokenTree::Ident(i) if *i == "WriteAsZeroes" => Ok(FieldUpdate::WriteAsZeroes),
            _ => Err(Error::new(
                tt.span(),
                "expected Preserve, WriteAsOnes, or WriteAsZeroes",
            )),
        }
    }

    /// Parse field entries: `NAME, bits,` or `, bits,` or `offset(N),`
    fn parse_field_entries(tokens: TokenStream) -> Result<Vec<FieldEntryDsl>> {
        let toks: Vec<TokenTree> = tokens.into_iter().collect();
        let mut entries = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            // offset(N)
            if matches!(&toks[i], TokenTree::Ident(id) if *id == "offset") {
                i += 1; // skip "offset"
                if let Some(TokenTree::Group(g)) = toks.get(i) {
                    let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                    if let Some(TokenTree::Literal(lit)) = inner.first() {
                        let n = parse_usize_literal(&lit.to_string())
                            .map_err(|_| Error::new(lit.span(), "expected integer for offset"))?;
                        entries.push(FieldEntryDsl::Offset(n));
                    }
                    i += 1; // skip group
                }
                // skip trailing comma
                if matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ',') {
                    i += 1;
                }
                continue;
            }
            // `, bits,` -- anonymous reserved field (starts with comma)
            if matches!(&toks[i], TokenTree::Punct(p) if p.as_char() == ',') {
                i += 1; // skip comma
                if let Some(TokenTree::Literal(lit)) = toks.get(i) {
                    let bits: usize = lit
                        .to_string()
                        .parse()
                        .map_err(|_| Error::new(lit.span(), "expected bit count"))?;
                    entries.push(FieldEntryDsl::Reserved(bits));
                    i += 1; // skip literal
                }
                // skip trailing comma
                if matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ',') {
                    i += 1;
                }
                continue;
            }
            // `NAME, bits,` -- named field
            if let TokenTree::Ident(ident) = &toks[i] {
                let field_name = ident.to_string();
                i += 1; // skip name
                        // expect comma
                if matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ',') {
                    i += 1;
                }
                if let Some(TokenTree::Literal(lit)) = toks.get(i) {
                    let bits: usize = lit
                        .to_string()
                        .parse()
                        .map_err(|_| Error::new(lit.span(), "expected bit count"))?;
                    entries.push(FieldEntryDsl::Named(field_name, bits));
                    i += 1; // skip literal
                }
                // skip trailing comma
                if matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ',') {
                    i += 1;
                }
                continue;
            }
            // skip any other token (e.g. comments)
            i += 1;
        }
        Ok(entries)
    }

    // ---------------------------------------------------------------
    // create_dword_field(buffer, index, "NAME");
    // ---------------------------------------------------------------
    fn parse_create_dword_field(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("create_dword_field")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let buffer = p.parse_value();
        p.expect_punct(',')?;
        let index = p.parse_value();
        p.expect_punct(',')?;
        let name = p.expect_string_lit()?;
        self.expect_punct(';')?;
        Ok(DslItem::CreateDwordField {
            buffer,
            index,
            name,
            span,
        })
    }

    // ---------------------------------------------------------------
    // store(value, target);
    // ---------------------------------------------------------------
    fn parse_store(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("store")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let value = p.parse_value();
        p.expect_punct(',')?;
        let target = p.parse_value();
        self.expect_punct(';')?;
        Ok(DslItem::Store {
            value,
            target,
            span,
        })
    }

    // ---------------------------------------------------------------
    // shl(target, value, count);
    // ---------------------------------------------------------------
    fn parse_shift_left(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("shl")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let target = p.parse_value();
        p.expect_punct(',')?;
        let value = p.parse_value();
        p.expect_punct(',')?;
        let count = p.parse_value();
        self.expect_punct(';')?;
        Ok(DslItem::ShiftLeft {
            target,
            value,
            count,
            span,
        })
    }

    // ---------------------------------------------------------------
    // subtract(target, a, b);
    // ---------------------------------------------------------------
    fn parse_subtract(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("subtract")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let target = p.parse_value();
        p.expect_punct(',')?;
        let a = p.parse_value();
        p.expect_punct(',')?;
        let b = p.parse_value();
        self.expect_punct(';')?;
        Ok(DslItem::Subtract { target, a, b, span })
    }

    // ---------------------------------------------------------------
    // add(target, a, b);
    // ---------------------------------------------------------------
    fn parse_add(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("add")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let target = p.parse_value();
        p.expect_punct(',')?;
        let a = p.parse_value();
        p.expect_punct(',')?;
        let b = p.parse_value();
        self.expect_punct(';')?;
        Ok(DslItem::Add { target, a, b, span })
    }

    fn parse_interpolation(&mut self) -> Result<DslValue> {
        self.expect_punct('#')?;
        let (expr, _) = self.expect_group(Delimiter::Brace)?;
        Ok(DslValue::Interpolation(expr))
    }
}

/// Parse a usize from a literal string, handling decimal, hex (0x..),
/// octal (0o..), and binary (0b..) prefixes.
fn parse_usize_literal(s: &str) -> core::result::Result<usize, core::num::ParseIntError> {
    if let Some(hex) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        usize::from_str_radix(hex, 16)
    } else if let Some(oct) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        usize::from_str_radix(oct, 8)
    } else if let Some(bin) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        usize::from_str_radix(bin, 2)
    } else {
        s.parse()
    }
}
