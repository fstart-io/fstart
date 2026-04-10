//! DSL token stream parser.
//!
//! Parses the `acpi_dsl!` input into an AST of [`DslItem`] nodes.
//! Supports both ASL 1.0 function-call syntax and ASL 2.0 C-style
//! expression/assignment syntax.
//!
//! ```text
//! items       = item*
//! item        = Scope | Device | Name | Method | Return | If | While
//!             | Break | Notify | Sleep | Stall | Assign | Increment
//!             | Decrement | CreateDwordField | OperationRegion | Field
//!             | Store | ShiftLeft | Subtract | Add | #{expr}
//! expr        = atom (binop atom)*          // Pratt parser
//! atom        = "(" expr ")" | "!" expr | "~" expr | literal | ident
//!             | ToUUID(..) | SizeOf(..) | DeRefOf(..) | CondRefOf(..)
//!             | Index(..) | #{expr}
//! ```

use proc_macro2::{Delimiter, Spacing, Span, TokenStream, TokenTree};
use syn::{Error, Result};

// -----------------------------------------------------------------------
// AST types
// -----------------------------------------------------------------------

/// A name that is either a literal string or an interpolated expression.
#[derive(Debug)]
pub enum NameOrInterp {
    /// Literal ACPI name string (e.g., `"COM0"`).
    Literal(String),
    /// Interpolated Rust expression (e.g., `#{name}`).
    Interpolation(TokenStream),
}

/// Binary operator in an expression.
#[derive(Debug, Clone, Copy)]
pub enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
    Mod,
    ShiftLeft,
    ShiftRight,
    And,
    Or,
    Xor,
    LAnd,
    LOr,
    Equal,
    NotEqual,
    Less,
    Greater,
    LessEqual,
    GreaterEqual,
}

/// A parsed expression (ASL 2.0).
#[derive(Debug)]
pub enum DslExpr {
    /// Integer literal (`0u32`, `0x1000u64`)
    IntLit(TokenStream),
    /// String literal (`"hello"`)
    StringLit(String),
    /// ACPI named path (`CDW1`, `TLUD`)
    Path(String),
    /// Local variable (`Local0`..`Local7`)
    Local(u8),
    /// Method argument (`Arg0`..`Arg6`)
    Arg(u8),
    /// AML Zero constant
    Zero,
    /// AML One constant
    One,
    /// AML Ones constant (0xFFFF_FFFF_FFFF_FFFF)
    Ones,
    /// Rust interpolation: `#{expr}`
    Interpolation(TokenStream),
    /// `ToUUID("...")`
    ToUUID(String),
    /// `SizeOf(expr)`
    SizeOf(Box<DslExpr>),
    /// `DeRefOf(expr)`
    DeRefOf(Box<DslExpr>),
    /// `CondRefOf(source, target)`
    CondRefOf(Box<DslExpr>, Box<DslExpr>),
    /// `Index(source, index)`
    Index(Box<DslExpr>, Box<DslExpr>),
    /// Binary operation: `a + b`, `a == b`, etc.
    Binary(BinaryOp, Box<DslExpr>, Box<DslExpr>),
    /// Logical NOT: `!expr`
    LNot(Box<DslExpr>),
    /// Bitwise NOT: `~expr`
    BitNot(Box<DslExpr>),
}

/// A parsed DSL item (statement).
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
        value: DslReturnValue,
        #[allow(dead_code)]
        span: Span,
    },
    /// `OperationRegion("NAME", space, offset, length);`
    OpRegion {
        name: String,
        space: RegionSpace,
        offset: DslValue,
        length: DslValue,
        span: Span,
    },
    /// `Field("REGION", access, lock, update) { entries }`
    Field {
        region: String,
        access: FieldAccess,
        lock: FieldLock,
        update: FieldUpdate,
        entries: Vec<FieldEntryDsl>,
        span: Span,
    },
    /// `CreateDwordField(buffer, index, "NAME");`
    CreateDwordField {
        buffer: DslValue,
        index: DslValue,
        name: String,
        span: Span,
    },
    /// `Store(value, target);` (legacy ASL 1.0 syntax)
    Store {
        value: DslValue,
        target: DslValue,
        #[allow(dead_code)]
        span: Span,
    },
    /// `ShiftLeft(target, value, count);` (legacy ASL 1.0 syntax)
    ShiftLeft {
        target: DslValue,
        value: DslValue,
        count: DslValue,
        #[allow(dead_code)]
        span: Span,
    },
    /// `Subtract(target, a, b);` (legacy ASL 1.0 syntax)
    Subtract {
        target: DslValue,
        a: DslValue,
        b: DslValue,
        #[allow(dead_code)]
        span: Span,
    },
    /// `Add(target, a, b);` (legacy ASL 1.0 syntax)
    Add {
        target: DslValue,
        a: DslValue,
        b: DslValue,
        #[allow(dead_code)]
        span: Span,
    },
    /// `#{expr}` -- bare interpolation at statement level.
    RawExpr { expr: TokenStream },
    /// `If (condition) { body } [Else { body }]`
    If {
        condition: DslExpr,
        body: Vec<DslItem>,
        else_body: Option<Vec<DslItem>>,
        #[allow(dead_code)]
        span: Span,
    },
    /// `While (condition) { body }`
    While {
        condition: DslExpr,
        body: Vec<DslItem>,
        #[allow(dead_code)]
        span: Span,
    },
    /// `target = expr;` (ASL 2.0 assignment)
    Assign {
        target: DslExpr,
        value: DslExpr,
        #[allow(dead_code)]
        span: Span,
    },
    /// `Notify(object, value);`
    Notify {
        object: DslExpr,
        value: DslExpr,
        #[allow(dead_code)]
        span: Span,
    },
    /// `Sleep(msec);`
    Sleep {
        msec: DslExpr,
        #[allow(dead_code)]
        span: Span,
    },
    /// `Stall(usec);`
    Stall {
        usec: DslExpr,
        #[allow(dead_code)]
        span: Span,
    },
    /// `Break;`
    Break {
        #[allow(dead_code)]
        span: Span,
    },
    /// `target++` or `Increment(target);`
    Increment {
        target: DslExpr,
        #[allow(dead_code)]
        span: Span,
    },
    /// `target--` or `Decrement(target);`
    Decrement {
        target: DslExpr,
        #[allow(dead_code)]
        span: Span,
    },
}

/// Return value can be either a legacy DslValue or a new-style DslExpr.
#[derive(Debug)]
pub enum DslReturnValue {
    Legacy(DslValue),
    Expr(DslExpr),
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

/// A parsed value expression (legacy form for Name values, etc.).
#[derive(Debug)]
pub enum DslValue {
    /// String literal: `"ARMH0011"`
    StringLit(String),
    /// Integer literal: `0u32`, `0x1000u32`
    IntLit(TokenStream),
    /// EISA ID: `EisaId("PNP0501")`
    EisaId(String),
    /// Package: `Package(val1, val2, ...)`
    Package(Vec<DslValue>),
    /// Resource template: `ResourceTemplate { ... }`
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
    /// `IO(base, end, align, len);` -- legacy I/O port descriptor.
    IoPort {
        base: DslValue,
        end: DslValue,
        align: DslValue,
        len: DslValue,
    },
    /// `DWordIO(base, end);` -- DWord I/O range.
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

// -----------------------------------------------------------------------
// Top-level parse entry point
// -----------------------------------------------------------------------

/// Parse the top-level DSL input.
pub fn parse_dsl(input: TokenStream) -> Result<Vec<DslItem>> {
    let mut parser = Parser::new(input);
    parser.parse_items()
}

// -----------------------------------------------------------------------
// Helpers -- identifier classification
// -----------------------------------------------------------------------

/// Check if an identifier is a Local variable (Local0..Local7).
fn parse_local(name: &str) -> Option<u8> {
    if let Some(rest) = name.strip_prefix("Local") {
        if rest.len() == 1 {
            let n = rest.as_bytes()[0];
            if (b'0'..=b'7').contains(&n) {
                return Some(n - b'0');
            }
        }
    }
    None
}

/// Check if an identifier is an Arg variable (Arg0..Arg6).
fn parse_arg(name: &str) -> Option<u8> {
    if let Some(rest) = name.strip_prefix("Arg") {
        if rest.len() == 1 {
            let n = rest.as_bytes()[0];
            if (b'0'..=b'6').contains(&n) {
                return Some(n - b'0');
            }
        }
    }
    None
}

/// Returns true if the identifier is reserved as a DSL keyword that
/// starts a statement and should NOT be treated as an ACPI path name.
fn is_statement_keyword(name: &str) -> bool {
    matches!(
        name,
        "Scope"
            | "Device"
            | "Name"
            | "Method"
            | "Return"
            | "OperationRegion"
            | "Field"
            | "CreateDwordField"
            | "Store"
            | "ShiftLeft"
            | "Subtract"
            | "Add"
            | "If"
            | "Else"
            | "While"
            | "Break"
            | "Notify"
            | "Sleep"
            | "Stall"
            | "Increment"
            | "Decrement"
    )
}

// -----------------------------------------------------------------------
// Parser core
// -----------------------------------------------------------------------

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

    fn peek_at(&self, offset: usize) -> Option<&TokenTree> {
        self.tokens.get(self.pos + offset)
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

    // -------------------------------------------------------------------
    // Statement-level parsing
    // -------------------------------------------------------------------

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
                    "Scope" => self.parse_scope(),
                    "Device" => self.parse_device(),
                    "Name" => self.parse_name(),
                    "Method" => self.parse_method(),
                    "Return" => self.parse_return(),
                    "OperationRegion" => self.parse_op_region(),
                    "Field" => self.parse_field(),
                    "CreateDwordField" => self.parse_create_dword_field(),
                    // Legacy ASL 1.0 function-call forms
                    "Store" => self.parse_store(),
                    "ShiftLeft" => self.parse_shift_left(),
                    "Subtract" => self.parse_subtract(),
                    "Add" => self.parse_add(),
                    // New ASL 2.0 control flow
                    "If" => self.parse_if(),
                    "While" => self.parse_while(),
                    "Break" => self.parse_break(),
                    "Notify" => self.parse_notify(),
                    "Sleep" => self.parse_sleep_stmt(),
                    "Stall" => self.parse_stall_stmt(),
                    "Increment" => self.parse_increment_call(),
                    "Decrement" => self.parse_decrement_call(),
                    _ => {
                        // Could be an assignment, increment, or decrement:
                        //   IDENT = expr;
                        //   LocalN = expr;
                        //   ArgN = expr;
                        //   IDENT++;
                        //   IDENT--;
                        if self.is_assign_or_postfix() {
                            self.parse_assign_or_postfix()
                        } else {
                            Err(Error::new(
                                ident.span(),
                                format!("unknown DSL keyword `{name}`"),
                            ))
                        }
                    }
                }
            }
            // #{expr} -- bare interpolation at statement level
            TokenTree::Punct(p) if p.as_char() == '#' => {
                self.advance(); // consume '#'
                let (expr, _) = self.expect_group(Delimiter::Brace)?;
                Ok(DslItem::RawExpr { expr })
            }
            other => Err(Error::new(other.span(), "expected DSL keyword or #{expr}")),
        }
    }

    /// Look ahead to determine if the current identifier starts an
    /// assignment (`IDENT = expr;`) or postfix (`IDENT++`, `IDENT--`).
    fn is_assign_or_postfix(&self) -> bool {
        if let Some(TokenTree::Ident(ident)) = self.peek() {
            let name = ident.to_string();
            // Must not be a known keyword (those are handled above)
            if is_statement_keyword(&name) {
                return false;
            }
            // Look at the token after the identifier
            if let Some(next) = self.peek_at(1) {
                match next {
                    // `IDENT = ...` (single `=`, not `==`)
                    TokenTree::Punct(p) if p.as_char() == '=' => {
                        // Make sure it's not `==` (comparison)
                        if p.spacing() == Spacing::Joint {
                            // Could be `==`, check next
                            if let Some(TokenTree::Punct(p2)) = self.peek_at(2) {
                                if p2.as_char() == '=' {
                                    return false; // it's `==`
                                }
                            }
                        }
                        true
                    }
                    // `IDENT++` or `IDENT--`
                    TokenTree::Punct(p)
                        if (p.as_char() == '+' || p.as_char() == '-')
                            && p.spacing() == Spacing::Joint =>
                    {
                        if let Some(TokenTree::Punct(p2)) = self.peek_at(2) {
                            p2.as_char() == p.as_char()
                        } else {
                            false
                        }
                    }
                    _ => false,
                }
            } else {
                false
            }
        } else {
            false
        }
    }

    /// Parse an assignment or postfix increment/decrement.
    fn parse_assign_or_postfix(&mut self) -> Result<DslItem> {
        let span = self.span();
        // Parse the target as an expression atom (just the identifier/Local/Arg)
        let target = self.parse_expr_atom()?;

        // Check what follows
        match self.peek() {
            Some(TokenTree::Punct(p)) if p.as_char() == '=' => {
                self.advance(); // consume '='
                let value = self.parse_expr(0)?;
                self.expect_punct(';')?;
                Ok(DslItem::Assign {
                    target,
                    value,
                    span,
                })
            }
            Some(TokenTree::Punct(p)) if p.as_char() == '+' && p.spacing() == Spacing::Joint => {
                self.advance(); // consume first '+'
                self.advance(); // consume second '+'
                self.expect_punct(';')?;
                Ok(DslItem::Increment { target, span })
            }
            Some(TokenTree::Punct(p)) if p.as_char() == '-' && p.spacing() == Spacing::Joint => {
                self.advance(); // consume first '-'
                self.advance(); // consume second '-'
                self.expect_punct(';')?;
                Ok(DslItem::Decrement { target, span })
            }
            _ => Err(Error::new(span, "expected `=`, `++`, or `--` after target")),
        }
    }

    // -------------------------------------------------------------------
    // Block statements (Scope, Device, Method, etc.)
    // -------------------------------------------------------------------

    fn parse_scope(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Scope")?;
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
        let span = self.expect_ident("Device")?;
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
        let span = self.expect_ident("Name")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let name = args_parser.expect_string_lit()?;
        args_parser.expect_punct(',')?;
        let value = args_parser.parse_value();
        self.expect_punct(';')?;

        Ok(DslItem::Name { name, value, span })
    }

    fn parse_method(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Method")?;
        let (args, args_span) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let name = args_parser.expect_string_lit()?;
        args_parser.expect_punct(',')?;

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
        let span = self.expect_ident("Return")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);

        // Try to determine if the value inside is an expression (has operators,
        // Local/Arg refs) or a legacy DslValue.
        let value = if args_parser.looks_like_expr() {
            DslReturnValue::Expr(args_parser.parse_expr(0)?)
        } else {
            DslReturnValue::Legacy(args_parser.parse_value())
        };
        self.expect_punct(';')?;

        Ok(DslItem::Return { value, span })
    }

    /// Heuristic: does the parenthesized content look like an expression?
    /// True if it contains Local/Arg references or is a bare identifier
    /// that isn't a known value-form keyword (EisaId, Package, etc.).
    fn looks_like_expr(&self) -> bool {
        if let Some(TokenTree::Ident(ident)) = self.peek() {
            let name = ident.to_string();
            if parse_local(&name).is_some() || parse_arg(&name).is_some() {
                return true;
            }
            if matches!(name.as_str(), "Zero" | "One" | "Ones") {
                return true;
            }
            // Check for ACPI path names (all-uppercase identifiers not
            // matching known value keywords).
            if !matches!(
                name.as_str(),
                "EisaId" | "Package" | "ResourceTemplate" | "ToUUID"
            ) && name
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
            {
                return true;
            }
        }
        false
    }

    // -------------------------------------------------------------------
    // Control flow
    // -------------------------------------------------------------------

    fn parse_if(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("If")?;
        let (cond_tokens, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut cond_parser = Parser::new(cond_tokens);
        let condition = cond_parser.parse_expr(0)?;

        let (body_tokens, _) = self.expect_group(Delimiter::Brace)?;
        let mut body_parser = Parser::new(body_tokens);
        let body = body_parser.parse_items()?;

        // Check for Else / ElseIf
        let else_body = if self.peek_ident_eq("Else") {
            self.advance(); // consume "Else"
                            // Check for ElseIf: `Else If (...) { ... }`
            if self.peek_ident_eq("If") {
                // Parse as a nested If inside the else body
                let nested_if = self.parse_if()?;
                Some(vec![nested_if])
            } else {
                let (else_tokens, _) = self.expect_group(Delimiter::Brace)?;
                let mut else_parser = Parser::new(else_tokens);
                Some(else_parser.parse_items()?)
            }
        } else {
            None
        };

        Ok(DslItem::If {
            condition,
            body,
            else_body,
            span,
        })
    }

    fn parse_while(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("While")?;
        let (cond_tokens, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut cond_parser = Parser::new(cond_tokens);
        let condition = cond_parser.parse_expr(0)?;

        let (body_tokens, _) = self.expect_group(Delimiter::Brace)?;
        let mut body_parser = Parser::new(body_tokens);
        let body = body_parser.parse_items()?;

        Ok(DslItem::While {
            condition,
            body,
            span,
        })
    }

    fn parse_break(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Break")?;
        self.expect_punct(';')?;
        Ok(DslItem::Break { span })
    }

    fn parse_notify(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Notify")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let object = p.parse_expr(0)?;
        p.expect_punct(',')?;
        let value = p.parse_expr(0)?;
        self.expect_punct(';')?;
        Ok(DslItem::Notify {
            object,
            value,
            span,
        })
    }

    fn parse_sleep_stmt(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Sleep")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let msec = p.parse_expr(0)?;
        self.expect_punct(';')?;
        Ok(DslItem::Sleep { msec, span })
    }

    fn parse_stall_stmt(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Stall")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let usec = p.parse_expr(0)?;
        self.expect_punct(';')?;
        Ok(DslItem::Stall { usec, span })
    }

    fn parse_increment_call(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Increment")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let target = p.parse_expr(0)?;
        self.expect_punct(';')?;
        Ok(DslItem::Increment { target, span })
    }

    fn parse_decrement_call(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Decrement")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let target = p.parse_expr(0)?;
        self.expect_punct(';')?;
        Ok(DslItem::Decrement { target, span })
    }

    // -------------------------------------------------------------------
    // Legacy ASL 1.0 operation parsers (kept for backward compat)
    // -------------------------------------------------------------------

    fn parse_op_region(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("OperationRegion")?;
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

    fn parse_field(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Field")?;
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

    fn parse_field_entries(tokens: TokenStream) -> Result<Vec<FieldEntryDsl>> {
        let toks: Vec<TokenTree> = tokens.into_iter().collect();
        let mut entries = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            if matches!(&toks[i], TokenTree::Ident(id) if *id == "Offset") {
                i += 1;
                if let Some(TokenTree::Group(g)) = toks.get(i) {
                    let inner: Vec<TokenTree> = g.stream().into_iter().collect();
                    if let Some(TokenTree::Literal(lit)) = inner.first() {
                        let n = parse_usize_literal(&lit.to_string())
                            .map_err(|_| Error::new(lit.span(), "expected integer for offset"))?;
                        entries.push(FieldEntryDsl::Offset(n));
                    }
                    i += 1;
                }
                if matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ',') {
                    i += 1;
                }
                continue;
            }
            if matches!(&toks[i], TokenTree::Punct(p) if p.as_char() == ',') {
                i += 1;
                if let Some(TokenTree::Literal(lit)) = toks.get(i) {
                    let bits: usize = lit
                        .to_string()
                        .parse()
                        .map_err(|_| Error::new(lit.span(), "expected bit count"))?;
                    entries.push(FieldEntryDsl::Reserved(bits));
                    i += 1;
                }
                if matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ',') {
                    i += 1;
                }
                continue;
            }
            if let TokenTree::Ident(ident) = &toks[i] {
                let field_name = ident.to_string();
                i += 1;
                if matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ',') {
                    i += 1;
                }
                if let Some(TokenTree::Literal(lit)) = toks.get(i) {
                    let bits: usize = lit
                        .to_string()
                        .parse()
                        .map_err(|_| Error::new(lit.span(), "expected bit count"))?;
                    entries.push(FieldEntryDsl::Named(field_name, bits));
                    i += 1;
                }
                if matches!(toks.get(i), Some(TokenTree::Punct(p)) if p.as_char() == ',') {
                    i += 1;
                }
                continue;
            }
            i += 1;
        }
        Ok(entries)
    }

    fn parse_create_dword_field(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("CreateDwordField")?;
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

    fn parse_store(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Store")?;
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

    fn parse_shift_left(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("ShiftLeft")?;
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

    fn parse_subtract(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Subtract")?;
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

    fn parse_add(&mut self) -> Result<DslItem> {
        let span = self.expect_ident("Add")?;
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

    // -------------------------------------------------------------------
    // Value parsing (for Name values, legacy function args, etc.)
    // -------------------------------------------------------------------

    fn parse_value(&mut self) -> DslValue {
        if self.peek_ident_eq("EisaId") {
            return self
                .parse_eisa_id()
                .unwrap_or_else(|e| DslValue::Interpolation(e.to_compile_error()));
        }
        if self.peek_ident_eq("Package") {
            return self
                .parse_package()
                .unwrap_or_else(|e| DslValue::Interpolation(e.to_compile_error()));
        }
        if self.peek_ident_eq("ResourceTemplate") {
            return self
                .parse_resource_template()
                .unwrap_or_else(|e| DslValue::Interpolation(e.to_compile_error()));
        }
        if matches!(self.peek(), Some(TokenTree::Punct(p)) if p.as_char() == '#') {
            return self
                .parse_interpolation()
                .unwrap_or_else(|e| DslValue::Interpolation(e.to_compile_error()));
        }

        match self.peek() {
            Some(TokenTree::Literal(lit)) => {
                let s = lit.to_string();
                let lit_clone = lit.clone();
                self.advance();
                if s.starts_with('"') {
                    let lit_str: syn::LitStr = syn::parse2(TokenTree::Literal(lit_clone).into())
                        .expect("already checked starts with '\"'");
                    DslValue::StringLit(lit_str.value())
                } else {
                    DslValue::IntLit(TokenTree::Literal(lit_clone).into())
                }
            }
            _ => {
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
        self.expect_ident("Package")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let mut elements = Vec::new();
        while !args_parser.at_end() {
            elements.push(args_parser.parse_value());
            if !args_parser.at_end() {
                let _ = args_parser.expect_punct(',');
            }
        }
        Ok(DslValue::Package(elements))
    }

    fn parse_eisa_id(&mut self) -> Result<DslValue> {
        self.expect_ident("EisaId")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut args_parser = Parser::new(args);
        let id = args_parser.expect_string_lit()?;
        Ok(DslValue::EisaId(id))
    }

    fn parse_resource_template(&mut self) -> Result<DslValue> {
        self.expect_ident("ResourceTemplate")?;
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
            TokenTree::Ident(i) if *i == "Memory32Fixed" => self.parse_memory_32_fixed(),
            TokenTree::Ident(i) if *i == "Interrupt" => self.parse_interrupt(),
            TokenTree::Ident(i) if *i == "IO" => self.parse_io_port(),
            TokenTree::Ident(i) if *i == "DWordIO" => self.parse_dword_io(),
            TokenTree::Ident(i) if *i == "WordBusNumber" => self.parse_word_bus_number(),
            TokenTree::Ident(i) if *i == "DWordMemory" => self.parse_dword_memory(),
            TokenTree::Ident(i) if *i == "QWordMemory" => self.parse_qword_memory(),
            other => Err(Error::new(
                other.span(),
                "expected resource descriptor (Memory32Fixed, Interrupt, \
                 WordBusNumber, DWordMemory, QWordMemory)",
            )),
        }
    }

    fn parse_memory_32_fixed(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("Memory32Fixed")?;
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
        self.expect_ident("Interrupt")?;
        let (args, args_span) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);

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

        let lvl_tt = p
            .advance()
            .ok_or_else(|| Error::new(args_span, "expected Level or Edge"))?;
        let level = match &lvl_tt {
            TokenTree::Ident(i) if *i == "Level" => true,
            TokenTree::Ident(i) if *i == "Edge" => false,
            _ => return Err(Error::new(lvl_tt.span(), "expected `Level` or `Edge`")),
        };
        p.expect_punct(',')?;

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

    fn parse_io_port(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("IO")?;
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

    fn parse_dword_io(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("DWordIO")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let base = p.parse_value();
        p.expect_punct(',')?;
        let end = p.parse_value();
        self.expect_punct(';')?;
        Ok(ResourceDesc::DWordIO { base, end })
    }

    fn parse_word_bus_number(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("WordBusNumber")?;
        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
        let mut p = Parser::new(args);
        let start = p.parse_value();
        p.expect_punct(',')?;
        let end = p.parse_value();
        self.expect_punct(';')?;
        Ok(ResourceDesc::WordBusNumber { start, end })
    }

    fn parse_dword_memory(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("DWordMemory")?;
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

    fn parse_qword_memory(&mut self) -> Result<ResourceDesc> {
        self.expect_ident("QWordMemory")?;
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

    // ===================================================================
    // Expression parser (Pratt / precedence climbing)
    // ===================================================================

    /// Parse an expression with minimum precedence `min_prec`.
    fn parse_expr(&mut self, min_prec: u8) -> Result<DslExpr> {
        let mut lhs = self.parse_expr_atom()?;
        while let Some((op, prec)) = self.peek_binop() {
            if prec < min_prec {
                break;
            }
            self.consume_binop(op);
            // Left-associative: use prec + 1 for the right side
            let rhs = self.parse_expr(prec + 1)?;
            lhs = DslExpr::Binary(op, Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    /// Parse an expression atom (leaf or prefix unary).
    fn parse_expr_atom(&mut self) -> Result<DslExpr> {
        let tt = self
            .peek()
            .ok_or_else(|| Error::new(self.span(), "expected expression"))?;

        match tt {
            // Parenthesized sub-expression
            TokenTree::Group(g) if g.delimiter() == Delimiter::Parenthesis => {
                let (inner, _) = self.expect_group(Delimiter::Parenthesis)?;
                let mut sub = Parser::new(inner);
                sub.parse_expr(0)
            }

            // Unary `!` (logical NOT)
            TokenTree::Punct(p) if p.as_char() == '!' => {
                // Make sure it's not `!=`
                if p.spacing() == Spacing::Joint {
                    if let Some(TokenTree::Punct(p2)) = self.peek_at(1) {
                        if p2.as_char() == '=' {
                            return Err(Error::new(p.span(), "unexpected `!=` in atom position"));
                        }
                    }
                }
                self.advance(); // consume '!'
                let inner = self.parse_expr_atom()?;
                Ok(DslExpr::LNot(Box::new(inner)))
            }

            // Unary `~` (bitwise NOT)
            TokenTree::Punct(p) if p.as_char() == '~' => {
                self.advance(); // consume '~'
                let inner = self.parse_expr_atom()?;
                Ok(DslExpr::BitNot(Box::new(inner)))
            }

            // Interpolation `#{expr}`
            TokenTree::Punct(p) if p.as_char() == '#' => {
                self.advance();
                let (expr, _) = self.expect_group(Delimiter::Brace)?;
                Ok(DslExpr::Interpolation(expr))
            }

            // Integer literal
            TokenTree::Literal(_) => {
                let lit = self.advance().unwrap();
                let s = lit.to_string();
                if s.starts_with('"') {
                    let lit_str: syn::LitStr = syn::parse2(lit.into())?;
                    Ok(DslExpr::StringLit(lit_str.value()))
                } else {
                    Ok(DslExpr::IntLit(lit.into()))
                }
            }

            // Identifier: Local, Arg, Zero/One/Ones, function, or ACPI path
            TokenTree::Ident(ident) => {
                let name = ident.to_string();

                // Local0..Local7
                if let Some(n) = parse_local(&name) {
                    self.advance();
                    return Ok(DslExpr::Local(n));
                }

                // Arg0..Arg6
                if let Some(n) = parse_arg(&name) {
                    self.advance();
                    return Ok(DslExpr::Arg(n));
                }

                match name.as_str() {
                    "Zero" => {
                        self.advance();
                        Ok(DslExpr::Zero)
                    }
                    "One" => {
                        self.advance();
                        Ok(DslExpr::One)
                    }
                    "Ones" => {
                        self.advance();
                        Ok(DslExpr::Ones)
                    }
                    "ToUUID" => {
                        self.advance();
                        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
                        let mut p = Parser::new(args);
                        let s = p.expect_string_lit()?;
                        Ok(DslExpr::ToUUID(s))
                    }
                    "SizeOf" => {
                        self.advance();
                        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
                        let mut p = Parser::new(args);
                        let inner = p.parse_expr(0)?;
                        Ok(DslExpr::SizeOf(Box::new(inner)))
                    }
                    "DeRefOf" => {
                        self.advance();
                        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
                        let mut p = Parser::new(args);
                        let inner = p.parse_expr(0)?;
                        Ok(DslExpr::DeRefOf(Box::new(inner)))
                    }
                    "CondRefOf" => {
                        self.advance();
                        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
                        let mut p = Parser::new(args);
                        let a = p.parse_expr(0)?;
                        p.expect_punct(',')?;
                        let b = p.parse_expr(0)?;
                        Ok(DslExpr::CondRefOf(Box::new(a), Box::new(b)))
                    }
                    "Index" => {
                        self.advance();
                        let (args, _) = self.expect_group(Delimiter::Parenthesis)?;
                        let mut p = Parser::new(args);
                        let a = p.parse_expr(0)?;
                        p.expect_punct(',')?;
                        let b = p.parse_expr(0)?;
                        Ok(DslExpr::Index(Box::new(a), Box::new(b)))
                    }
                    _ => {
                        // Treat as an ACPI path name (e.g., CDW1, TLUD, _OSC)
                        self.advance();
                        Ok(DslExpr::Path(name))
                    }
                }
            }

            other => Err(Error::new(other.span(), "unexpected token in expression")),
        }
    }

    /// Peek at the current position to see if there's a binary operator.
    /// Returns `(BinaryOp, precedence)` or None.
    fn peek_binop(&self) -> Option<(BinaryOp, u8)> {
        let tt = self.peek()?;
        match tt {
            TokenTree::Punct(p) => {
                let ch = p.as_char();
                let joint = p.spacing() == Spacing::Joint;

                // Check two-character operators first
                if joint {
                    if let Some(TokenTree::Punct(p2)) = self.peek_at(1) {
                        let ch2 = p2.as_char();
                        match (ch, ch2) {
                            ('|', '|') => return Some((BinaryOp::LOr, 1)),
                            ('&', '&') => return Some((BinaryOp::LAnd, 2)),
                            ('=', '=') => return Some((BinaryOp::Equal, 6)),
                            ('!', '=') => return Some((BinaryOp::NotEqual, 6)),
                            ('<', '=') => return Some((BinaryOp::LessEqual, 7)),
                            ('>', '=') => return Some((BinaryOp::GreaterEqual, 7)),
                            ('<', '<') => return Some((BinaryOp::ShiftLeft, 8)),
                            ('>', '>') => return Some((BinaryOp::ShiftRight, 8)),
                            _ => {}
                        }
                    }
                }

                // Single-character operators (only if not joint, or if joint
                // didn't match a two-char op above)
                match ch {
                    '|' if !joint => Some((BinaryOp::Or, 3)),
                    '^' => Some((BinaryOp::Xor, 4)),
                    '&' if !joint => Some((BinaryOp::And, 5)),
                    '<' if !joint => Some((BinaryOp::Less, 7)),
                    '>' if !joint => Some((BinaryOp::Greater, 7)),
                    '+' => Some((BinaryOp::Add, 9)),
                    '-' => Some((BinaryOp::Subtract, 9)),
                    '*' => Some((BinaryOp::Multiply, 10)),
                    '/' => Some((BinaryOp::Divide, 10)),
                    '%' => Some((BinaryOp::Mod, 10)),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Consume the tokens that make up the binary operator.
    fn consume_binop(&mut self, op: BinaryOp) {
        match op {
            // Two-token operators
            BinaryOp::LOr
            | BinaryOp::LAnd
            | BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::LessEqual
            | BinaryOp::GreaterEqual
            | BinaryOp::ShiftLeft
            | BinaryOp::ShiftRight => {
                self.advance();
                self.advance();
            }
            // Single-token operators
            _ => {
                self.advance();
            }
        }
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
