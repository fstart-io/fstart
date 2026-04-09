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
                    _ => Err(Error::new(
                        ident.span(),
                        format!("unknown DSL keyword `{name}`"),
                    )),
                }
            }
            other => Err(Error::new(
                other.span(),
                "expected DSL keyword (scope, device, name, method, ret)",
            )),
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
            return self.parse_eisa_id().unwrap_or_else(|e| {
                // Fallback: treat as interpolation
                DslValue::Interpolation(e.to_compile_error())
            });
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
            other => Err(Error::new(
                other.span(),
                "expected `memory_32_fixed` or `interrupt`",
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

    fn parse_interpolation(&mut self) -> Result<DslValue> {
        self.expect_punct('#')?;
        let (expr, _) = self.expect_group(Delimiter::Brace)?;
        Ok(DslValue::Interpolation(expr))
    }
}
