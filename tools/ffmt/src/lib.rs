// SPDX-License-Identifier: MIT OR Apache-2.0
//! Layout of `register_bitfields!` bodies: one bitfield per line, one enum
//! variant per line (see AGENTS.md).
//!
//! rustfmt cannot do this. The macro body is not Rust syntax (a bare
//! `NAME OFFSET(0) NUMBITS(1) []` list), so the invocation stays an opaque
//! token tree and rustfmt only normalises the indentation around it. Clippy
//! lints run after expansion, where the source layout no longer exists, and a
//! pre-expansion lint would only see crates that this workspace actually
//! compiles -- `boards/` is not even a member.
//!
//! So this reads the source text instead. Everything works off a small lexer
//! ([`Ctx`]) that splits the file into code, comments and literals, so a
//! `//` inside a string, a `[` inside a comment or a `register_bitfields!`
//! shown in a doc example cannot confuse the scan. Fields and variants are
//! counted by structural splits, never by counting `=` or `OFFSET(`.
//!
//! [`fix_source`] reflows and then compares canonical token streams: literals
//! and comments are compared as units with their contents, so a rewrite that
//! moved code into a comment or changed a literal is refused, not written.

/// Macro name; the `!` and `[` are matched separately so `register_bitfields ![]`
/// is found too.
pub const MACRO_NAME: &str = "register_bitfields";

/// A line that packs more than one bitfield or enum variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// 1-based line number.
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.message)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Code,
    LineComment,
    BlockComment,
    Literal,
}

#[derive(Debug, Clone, Copy)]
struct Span {
    kind: Kind,
    start: usize,
    end: usize,
}

/// Source plus its lexing: which bytes are code, and where comments and
/// literals sit.
struct Ctx<'a> {
    src: &'a str,
    b: &'a [u8],
    mask: Vec<bool>,
    spans: Vec<Span>,
}

impl<'a> Ctx<'a> {
    fn new(src: &'a str) -> Self {
        let b = src.as_bytes();
        let spans = lex(b);
        let mut mask = vec![false; b.len()];
        for span in &spans {
            if span.kind == Kind::Code {
                mask[span.start..span.end].fill(true);
            }
        }
        Self {
            src,
            b,
            mask,
            spans,
        }
    }

    fn slice(&self, start: usize, end: usize) -> String {
        let (start, end) = (start.min(self.b.len()), end.min(self.b.len()));
        if start >= end {
            return String::new();
        }
        match self.src.get(start..end) {
            Some(text) => text.to_string(),
            None => String::from_utf8_lossy(&self.b[start..end]).into_owned(),
        }
    }

    fn is_code(&self, i: usize) -> bool {
        self.mask.get(i).copied().unwrap_or(false)
    }

    fn is_space(&self, i: usize) -> bool {
        self.b[i].is_ascii_whitespace()
    }

    /// The first byte in `[start, end)` that is code and not whitespace.
    fn first_code(&self, start: usize, end: usize) -> Option<usize> {
        (start..end).find(|i| self.is_code(*i) && !self.is_space(*i))
    }

    /// The last byte in `[start, end)` that is code and not whitespace.
    fn last_code(&self, start: usize, end: usize) -> Option<usize> {
        (start..end)
            .rev()
            .find(|i| self.is_code(*i) && !self.is_space(*i))
    }

    /// Any comment that overlaps `[start, end)` lies between code and must not
    /// be reflowed.
    fn comment_in(&self, start: usize, end: usize) -> Option<Span> {
        self.spans
            .iter()
            .copied()
            .find(|s| is_comment(s.kind) && s.start < end && s.end > start)
    }

    /// The comment starting at `start`, if the byte there opens one.
    fn comment_at(&self, start: usize) -> Option<Span> {
        self.spans
            .iter()
            .copied()
            .find(|s| is_comment(s.kind) && s.start == start)
    }

    fn line_starts(&self) -> Vec<usize> {
        let mut starts = vec![0];
        starts.extend(
            self.b
                .iter()
                .enumerate()
                .filter(|(_, c)| **c == b'\n')
                .map(|(i, _)| i + 1),
        );
        starts
    }

    fn line_of(&self, starts: &[usize], offset: usize) -> usize {
        starts.partition_point(|start| *start <= offset)
    }
}

/// Split the source into code, comment and literal spans.
fn lex(b: &[u8]) -> Vec<Span> {
    let n = b.len();
    let mut spans = Vec::new();
    let mut i = 0;
    let mut code = 0;
    let push_code = |spans: &mut Vec<Span>, start: usize, end: usize| {
        if start < end {
            spans.push(Span {
                kind: Kind::Code,
                start,
                end,
            });
        }
    };
    while i < n {
        if b[i] == b'/' && b.get(i + 1) == Some(&b'/') {
            push_code(&mut spans, code, i);
            let end = b[i..].iter().position(|c| *c == b'\n').map_or(n, |p| i + p);
            spans.push(Span {
                kind: Kind::LineComment,
                start: i,
                end,
            });
            i = end;
            code = i;
        } else if b[i] == b'/' && b.get(i + 1) == Some(&b'*') {
            push_code(&mut spans, code, i);
            // Rust block comments nest.
            let mut depth = 1usize;
            let mut j = i + 2;
            while j < n && depth > 0 {
                if b[j] == b'/' && b.get(j + 1) == Some(&b'*') {
                    depth += 1;
                    j += 2;
                } else if b[j] == b'*' && b.get(j + 1) == Some(&b'/') {
                    depth -= 1;
                    j += 2;
                } else {
                    j += 1;
                }
            }
            let end = j.min(n);
            spans.push(Span {
                kind: Kind::BlockComment,
                start: i,
                end,
            });
            i = end;
            code = i;
        } else if b[i] == b'"' {
            push_code(&mut spans, code, i);
            let end = string_end(b, i);
            spans.push(Span {
                kind: Kind::Literal,
                start: i,
                end,
            });
            i = end;
            code = i;
        } else if b[i] == b'\'' {
            match char_end(b, i) {
                Some(end) => {
                    push_code(&mut spans, code, i);
                    spans.push(Span {
                        kind: Kind::Literal,
                        start: i,
                        end,
                    });
                    i = end;
                    code = i;
                }
                // A lifetime, not a character literal.
                None => i += 1,
            }
        } else if b[i] == b'r' && raw_end(b, i).is_some() {
            let end = raw_end(b, i).unwrap();
            push_code(&mut spans, code, i);
            spans.push(Span {
                kind: Kind::Literal,
                start: i,
                end,
            });
            i = end;
            code = i;
        } else {
            i += 1;
        }
    }
    push_code(&mut spans, code, n);
    spans
}

/// End (exclusive) of the plain string literal starting at `i`.
fn string_end(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            b'"' => return j + 1,
            _ => j += 1,
        }
    }
    b.len()
}

/// End (exclusive) of the character literal starting at `i`, if it is one.
fn char_end(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i + 1;
    if j >= b.len() {
        return None;
    }
    if b[j] == b'\\' {
        j += 2;
        // `\u{..}` and friends: run to the closing quote.
        while j < b.len() && b[j] != b'\'' && b[j] != b'\n' {
            j += 1;
        }
    } else {
        let ch = std::str::from_utf8(&b[i + 1..]).ok()?.chars().next()?;
        if ch == '\n' || ch == '\'' {
            return None;
        }
        j = i + 1 + ch.len_utf8();
    }
    if b.get(j) == Some(&b'\'') {
        Some(j + 1)
    } else {
        None
    }
}

/// End (exclusive) of the raw string starting at `i`, if there is one.
fn raw_end(b: &[u8], i: usize) -> Option<usize> {
    if b[i] != b'r' {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() && b[j] == b'#' {
        j += 1;
    }
    if b.get(j) != Some(&b'"') {
        return None;
    }
    let hashes = j - i - 1;
    let mut k = j + 1;
    while k < b.len() {
        if b[k] == b'"'
            && b.len() >= k + 1 + hashes
            && b[k + 1..k + 1 + hashes].iter().all(|c| *c == b'#')
        {
            return Some(k + 1 + hashes);
        }
        k += 1;
    }
    Some(b.len())
}

fn is_comment(kind: Kind) -> bool {
    matches!(kind, Kind::LineComment | Kind::BlockComment)
}

/// Skip whitespace and comments between the tokens of one invocation.
fn skip_trivia(ctx: &Ctx<'_>, mut i: usize, end: usize) -> usize {
    while i < end {
        if ctx.b[i].is_ascii_whitespace() {
            i += 1;
        } else if let Some(span) = ctx.comment_at(i) {
            i = span.end.max(i + 1);
        } else {
            break;
        }
    }
    i
}

fn is_ident_byte(c: u8) -> bool {
    // Bytes >= 0x80 are part of a multi-byte char, which Rust accepts in an
    // identifier; treating them as identifier bytes keeps `éregister_...` from
    // being mistaken for our macro.
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

/// Bracket range of every `register_bitfields![ ... ]`, outer brackets included.
fn macro_bodies(ctx: &Ctx<'_>) -> Vec<(usize, usize)> {
    let b = ctx.b;
    let needle = MACRO_NAME.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i + needle.len() <= b.len() {
        let boundary = (i == 0 || !is_ident_byte(b[i - 1]))
            && (i + needle.len() == b.len() || !is_ident_byte(b[i + needle.len()]));
        if ctx.is_code(i) && boundary && &b[i..i + needle.len()] == needle {
            // Whitespace *and comments* may separate the tokens of the
            // invocation: `register_bitfields /* hi */ ! [ ... ]`.
            let mut j = skip_trivia(ctx, i + needle.len(), b.len());
            if b.get(j) == Some(&b'!') {
                j = skip_trivia(ctx, j + 1, b.len());
                if b.get(j) == Some(&b'[')
                    && let Some(close) = match_bracket(ctx, j)
                {
                    out.push((j, close));
                    // Step on instead of jumping past the body: a register
                    // macro nested inside another one must still be checked.
                    i += 1;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

/// Index of the `]` that closes the `[` at `open`.
fn match_bracket(ctx: &Ctx<'_>, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for i in open..ctx.b.len() {
        if !ctx.is_code(i) {
            continue;
        }
        match ctx.b[i] {
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Ranges between top-level commas, ignoring commas inside brackets,
/// comments or literals.
fn split_top(ctx: &Ctx<'_>, start: usize, end: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut angle = 0i32;
    let mut item = start;
    for i in start..end {
        if !ctx.is_code(i) {
            continue;
        }
        match ctx.b[i] {
            b'[' | b'(' | b'{' => depth += 1,
            b']' | b')' | b'}' => depth -= 1,
            // `value::<u8, u16>()` passes a comma through the turbofish; only
            // a `<` that follows `::` opens one, so `a < b` stays code.
            b'<' if angle > 0 => angle += 1,
            b'<' if followed_by_double_colon(ctx, i) => angle += 1,
            b'>' if angle > 0 => angle -= 1,
            b',' if depth <= 0 && angle == 0 => {
                out.push((item, i));
                item = i + 1;
            }
            _ => {}
        }
    }
    if has_content(ctx, item, end) {
        out.push((item, end));
    }
    out
}

/// Whether the code before `i` ends in `::`, i.e. `i` opens a turbofish.
fn followed_by_double_colon(ctx: &Ctx<'_>, i: usize) -> bool {
    let mut colons = 0;
    let mut k = i;
    while k > 0 && colons < 2 {
        k -= 1;
        if ctx.is_code(k) && !ctx.is_space(k) {
            if ctx.b[k] != b':' {
                return false;
            }
            colons += 1;
        }
    }
    colons == 2
}

fn has_content(ctx: &Ctx<'_>, start: usize, end: usize) -> bool {
    (start..end).any(|i| ctx.is_code(i) && !ctx.is_space(i))
}

/// Whether the `[` at `i` opens an attribute, i.e. `#[...]`.
fn opens_attribute(ctx: &Ctx<'_>, i: usize) -> bool {
    let mut k = i;
    while k > 0 {
        k -= 1;
        if ctx.is_code(k) && !ctx.is_space(k) {
            return ctx.b[k] == b'#';
        }
    }
    false
}

/// The first bracketed group `[ ... ]` at this level, as `(open, close)`,
/// skipping `#[attribute]` groups.
fn top_group(ctx: &Ctx<'_>, start: usize, end: usize) -> Option<(usize, usize)> {
    let mut depth = 0i32;
    let mut i = start;
    while i < end {
        if !ctx.is_code(i) {
            i += 1;
            continue;
        }
        match ctx.b[i] {
            b'[' if depth == 0 => match match_bracket(ctx, i) {
                Some(close) if opens_attribute(ctx, i) => {
                    i = close + 1;
                    continue;
                }
                Some(close) => return Some((i, close)),
                None => return None,
            },
            b'[' | b'(' | b'{' => depth += 1,
            b']' | b')' | b'}' => depth -= 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Report every `register_bitfields!` line that holds more than one field or
/// more than one enum variant.
pub fn check_source(src: &str) -> Vec<Violation> {
    let ctx = Ctx::new(src);
    let starts = ctx.line_starts();
    let mut field_lines = Vec::new();
    let mut variant_lines = Vec::new();
    for (open, close) in macro_bodies(&ctx) {
        for (reg_start, reg_end) in split_top(&ctx, open + 1, close) {
            let Some((fields_open, fields_close)) = top_group(&ctx, reg_start, reg_end) else {
                continue;
            };
            for (field_start, field_end) in split_top(&ctx, fields_open + 1, fields_close) {
                if let Some(at) = ctx.first_code(field_start, field_end) {
                    field_lines.push(ctx.line_of(&starts, at));
                }
                if let Some((open, close)) = top_group(&ctx, field_start, field_end) {
                    for (variant_start, variant_end) in split_top(&ctx, open + 1, close) {
                        if let Some(at) = ctx.first_code(variant_start, variant_end) {
                            variant_lines.push(ctx.line_of(&starts, at));
                        }
                    }
                }
            }
        }
    }
    let mut found = Vec::new();
    for (lines, plural, singular) in [
        (field_lines, "bitfields", "field"),
        (variant_lines, "enum variants", "variant"),
    ] {
        let mut counts: std::collections::BTreeMap<usize, usize> =
            std::collections::BTreeMap::new();
        for line in lines {
            *counts.entry(line).or_default() += 1;
        }
        for (line, count) in counts {
            if count > 1 {
                found.push(Violation {
                    line,
                    message: format!("{count} {plural} on one line; one {singular} per line"),
                });
            }
        }
    }
    found.sort_by_key(|v| (v.line, v.message.clone()));
    found
}

/// Reflow every `register_bitfields!` body: one field per line, one variant per
/// line, enums under their field. A register with one field, or a field with
/// one variant, that was already written on one line stays there, so the
/// result is idempotent and untouched files stay untouched.
///
/// `Ok(None)` means the source is unchanged. `Err` means a body did not parse,
/// a comment sat somewhere it cannot be reflowed, or the rewrite would have
/// changed more than whitespace; the caller must then leave the file alone.
pub fn fix_source(src: &str) -> Result<Option<String>, String> {
    if check_source(src).is_empty() {
        return Ok(None);
    }
    if src.contains('\r') {
        return Err("CRLF line endings; reflow it by hand".into());
    }
    // A register macro nested inside another one is reflowed by its own pass:
    // the outer pass keeps the inner tokens verbatim, so the inner body still
    // has to be laid out afterwards.
    let mut current = src.to_string();
    for _ in 0..8 {
        if check_source(&current).is_empty() {
            break;
        }
        match reflow_once(&current)? {
            Some(next) if next != current => current = next,
            _ => break,
        }
    }
    if current == src {
        return Ok(None);
    }
    Ok(Some(current))
}

fn reflow_once(src: &str) -> Result<Option<String>, String> {
    let ctx = Ctx::new(src);
    let all = macro_bodies(&ctx);
    // Innermost only: a body that contains another one is reflowed in a later
    // pass, so the inner replacement is not spliced into text the outer pass
    // would then re-emit.
    let bodies: Vec<(usize, usize)> = all
        .iter()
        .copied()
        .filter(|(open, close)| {
            !all.iter()
                .any(|(inner_open, inner_close)| inner_open > open && inner_close < close)
        })
        .collect();
    let mut out = String::with_capacity(src.len() + 256);
    let mut prev = 0;
    for (open, close) in bodies {
        let line_start = src[..open].rfind('\n').map_or(0, |n| n + 1);
        let indent = src[line_start..open]
            .chars()
            .take_while(|c| *c == ' ')
            .count();
        let (width, body) = reflow_body(&ctx, open + 1, close, indent + 4)?;
        out.push_str(&ctx.slice(prev, open + 1));
        if let Some(width) = width {
            out.push_str(&width);
            out.push(',');
        }
        out.push('\n');
        out.push_str(&body);
        out.push_str(&" ".repeat(indent));
        prev = close;
    }
    out.push_str(&ctx.slice(prev, ctx.b.len()));
    let (before, after) = (tokens(src), tokens(&out));
    if before != after {
        return Err(format!(
            "reflow would change more than whitespace: {}",
            first_difference(&before, &after)
        ));
    }
    if out == src {
        return Ok(None);
    }
    Ok(Some(out))
}

/// Split code into Rust-ish tokens so that a rewrite may move whitespace
/// between them, but never glue two tokens into one: `[Drop` and `[ Drop` are
/// the same tokens, `a b` and `ab` are not.
fn tokenize_code(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_whitespace() {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
        } else if c.is_alphanumeric() || c == '_' {
            word.push(c);
        } else {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            out.push(c.to_string());
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

fn first_difference(before: &[String], after: &[String]) -> String {
    for (i, (a, b)) in before.iter().zip(after).enumerate() {
        if a != b {
            return format!("token {i}: {a:?} becomes {b:?}");
        }
    }
    format!("{} tokens become {}", before.len(), after.len())
}

/// Registers of one macro body, and its width entry.
fn reflow_body(
    ctx: &Ctx<'_>,
    start: usize,
    end: usize,
    indent: usize,
) -> Result<(Option<String>, String), String> {
    let mut entries = split_top(ctx, start, end);
    let mut width = None;
    if let Some((first, last)) = entries.first().copied() {
        let text = normalize(ctx, first, last);
        if matches!(text.as_str(), "u8" | "u16" | "u32" | "u64") {
            width = Some(text);
            entries.remove(0);
        }
    }
    let trailing_comma = ctx
        .last_code(start, end)
        .is_some_and(|last| ctx.b[last] == b',');
    let pad = " ".repeat(indent);
    let mut out: Vec<String> = Vec::new();
    for (i, (entry_start, entry_end)) in entries.iter().enumerate() {
        let lead = ctx.slice(*entry_start, *entry_end);
        if i > 0
            && lead
                .chars()
                .take_while(|c| c.is_whitespace())
                .filter(|c| *c == '\n')
                .count()
                >= 2
        {
            out.push(String::new());
        }
        out.push(reflow_register(ctx, *entry_start, *entry_end, &pad)?);
        if i + 1 < entries.len() || trailing_comma {
            out.last_mut().unwrap().push(',');
        }
    }
    Ok((width, format!("{}\n", out.join("\n").trim_end())))
}

fn reflow_register(ctx: &Ctx<'_>, start: usize, end: usize, pad: &str) -> Result<String, String> {
    let (open, close) = top_group(ctx, start, end)
        .ok_or_else(|| format!("no field list in {:?}", normalize(ctx, start, end)))?;
    let (docs, code_start, code_end) = split_prologue(ctx, start, open);
    let (attrs, code_start) = split_attributes(ctx, code_start, code_end);
    if let Some(span) = ctx.comment_in(code_start, code_end) {
        return Err(format!(
            "comment at byte {} sits inside a register line; reflow it by hand",
            span.start
        ));
    }
    let head = normalize(ctx, code_start, code_end);
    if head.is_empty() {
        return Err(format!(
            "register without a name in {:?}",
            normalize(ctx, start, end)
        ));
    }
    let (pub_prefix, name) = match head.strip_prefix("pub(").and_then(|r| r.split_once(')')) {
        Some((scope, rest)) => (format!("pub({scope}) "), rest.trim().to_string()),
        None => match head.strip_prefix("pub ") {
            Some(rest) => ("pub ".to_string(), rest.trim().to_string()),
            None => (String::new(), head.clone()),
        },
    };
    if name.is_empty() || !name.bytes().all(is_ident_byte) {
        return Err(format!("unparsed register name {name:?}"));
    }
    let fields = split_top(ctx, open + 1, close);
    if fields.is_empty() {
        return Err(format!("register {name} without fields"));
    }
    let oneline = on_one_line(ctx, open, close);
    let mut lines: Vec<String> = docs.iter().map(|doc| emit_doc(pad, doc)).collect();
    let field_trailing_comma = ctx
        .last_code(fields[fields.len() - 1].1, close)
        .is_some_and(|last| ctx.b[last] == b',');
    let first_field = reflow_field(ctx, fields[0].0, fields[0].1, &format!("{pad}    "))?;
    if fields.len() == 1 && oneline && first_field.is_inline {
        let comma = if field_trailing_comma { "," } else { "" };
        lines.push(format!(
            "{pad}{attrs}{pub_prefix}{name} [{}{comma}]",
            first_field.text.trim_start()
        ));
        return Ok(lines.join("\n"));
    }
    let field_trailing_comma = ctx
        .last_code(fields[fields.len() - 1].1, close)
        .is_some_and(|last| ctx.b[last] == b',');
    lines.push(format!("{pad}{attrs}{pub_prefix}{name} ["));
    for (i, (field_start, field_end)) in fields.iter().enumerate() {
        let field = reflow_field(ctx, *field_start, *field_end, &format!("{pad}    "))?;
        let mut text = field.text;
        if i + 1 < fields.len() || field_trailing_comma {
            text.push(',');
        }
        lines.push(text);
    }
    lines.push(format!("{pad}]"));
    Ok(lines.join("\n"))
}

struct Emitted {
    text: String,
    is_inline: bool,
}

fn reflow_field(ctx: &Ctx<'_>, start: usize, end: usize, pad: &str) -> Result<Emitted, String> {
    let (variants_open, variants_close) = match top_group(ctx, start, end) {
        Some(group) => group,
        None => (end, end),
    };
    let (docs, code_start, code_end) = split_prologue(ctx, start, variants_open);
    let (attrs, code_start) = split_attributes(ctx, code_start, code_end);
    if let Some(span) = ctx.comment_in(code_start, code_end) {
        return Err(format!(
            "comment at byte {} sits inside a bitfield line; reflow it by hand",
            span.start
        ));
    }
    let head = normalize(ctx, code_start, code_end);
    let (name, rest) = head
        .split_once(' ')
        .ok_or_else(|| format!("field without a body: {head:?}"))?;
    let (offset, rest) = call(rest, "OFFSET").ok_or_else(|| format!("no OFFSET in {head:?}"))?;
    let (bits, rest) =
        call(rest.trim_start(), "NUMBITS").ok_or_else(|| format!("no NUMBITS in {head:?}"))?;
    if !rest.trim().is_empty() {
        return Err(format!("unparsed field tail {:?} in {head:?}", rest.trim()));
    }
    if !name.bytes().all(is_ident_byte) {
        return Err(format!("unparsed field name {name:?}"));
    }
    let mut variants = Vec::new();
    if variants_open != end {
        for (variant_start, variant_end) in split_top(ctx, variants_open + 1, variants_close) {
            let (variant_docs, variant_code_start, variant_code_end) =
                split_prologue(ctx, variant_start, variant_end);
            if let Some(span) = ctx.comment_in(variant_code_start, variant_code_end) {
                return Err(format!(
                    "comment at byte {} sits inside an enum variant; reflow it by hand",
                    span.start
                ));
            }
            let text = normalize(ctx, variant_code_start, variant_code_end);
            if text.is_empty() {
                continue;
            }
            variants.push((variant_docs, text));
        }
    }
    let vtrail = ctx
        .last_code(variants_open + 1, variants_close)
        .is_some_and(|last| ctx.b[last] == b',');
    let head = format!("{attrs}{name} OFFSET({offset}) NUMBITS({bits})");
    let oneline = on_one_line(ctx, start, end);
    let is_inline = oneline && docs.is_empty() && variants.len() <= 1;
    let mut lines: Vec<String> = docs.iter().map(|doc| emit_doc(pad, doc)).collect();
    if is_inline {
        let mut inner = variants
            .first()
            .map(|(_, text)| text.clone())
            .unwrap_or_default();
        if !inner.is_empty() && vtrail {
            inner.push(',');
        }
        lines.push(if inner.is_empty() {
            format!("{pad}{head} []")
        } else {
            format!("{pad}{head} [{inner}]")
        });
    } else if variants.is_empty() {
        lines.push(format!("{pad}{head} []"));
    } else {
        lines.push(format!("{pad}{head} ["));
        for (i, (variant_docs, text)) in variants.iter().enumerate() {
            for doc in variant_docs {
                lines.push(emit_doc(&format!("{pad}    "), doc));
            }
            let sep = if i + 1 < variants.len() || vtrail {
                ","
            } else {
                ""
            };
            lines.push(format!("{pad}    {text}{sep}"));
        }
        lines.push(format!("{pad}]"));
    }
    Ok(Emitted {
        text: lines.join("\n"),
        is_inline,
    })
}

/// Whether the entry's own code sits on one line, ignoring the whitespace
/// around it (which is what `contains('\n')` on the raw range would count).
fn on_one_line(ctx: &Ctx<'_>, start: usize, end: usize) -> bool {
    match (ctx.first_code(start, end), ctx.last_code(start, end)) {
        (Some(first), Some(last)) => !(first..=last).any(|i| ctx.b[i] == b'\n'),
        _ => true,
    }
}

fn emit_doc(pad: &str, doc: &str) -> String {
    if doc.is_empty() {
        String::new()
    } else {
        format!("{pad}{doc}")
    }
}

/// Leading `#[...]` attributes of an entry, normalised and space-terminated,
/// plus where the entry's code starts once they are skipped.
fn split_attributes(ctx: &Ctx<'_>, mut start: usize, end: usize) -> (String, usize) {
    let mut attrs = String::new();
    loop {
        let mut i = start;
        while i < end && !(ctx.is_code(i) && !ctx.is_space(i)) {
            i += 1;
        }
        if i + 1 >= end || ctx.b[i] != b'#' || ctx.b[i + 1] != b'[' {
            break;
        }
        // `match_bracket` scans the whole file, so an unbalanced `#[` inside
        // this entry must not be allowed to swallow the rest of it.
        let Some(close) = match_bracket(ctx, i + 1).filter(|close| *close < end) else {
            break;
        };
        if !attrs.is_empty() {
            attrs.push(' ');
        }
        attrs.push_str(&normalize(ctx, i, close + 1));
        start = close + 1;
    }
    if !attrs.is_empty() {
        attrs.push(' ');
    }
    (attrs, start)
}

/// `NAME(inner)rest` at the start of `text`, if `text` starts with `name(`.
fn call(text: &str, name: &str) -> Option<(String, String)> {
    let rest = text.strip_prefix(name)?.strip_prefix('(')?;
    let mut depth = 1;
    for (i, c) in rest.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some((rest[..i].trim().to_string(), rest[i + 1..].to_string()));
                }
            }
            _ => {}
        }
    }
    None
}

/// Leading full-line comments (`{pad}{doc}` ready) and the code range. Blank
/// lines inside the comment block are kept as empty entries, so a section
/// banner keeps its spacing.
fn split_prologue(ctx: &Ctx<'_>, start: usize, end: usize) -> (Vec<String>, usize, usize) {
    let mut docs = Vec::new();
    let mut line_start = start;
    let mut code_start = None;
    let mut i = start;
    while i <= end {
        if i == end || ctx.b[i] == b'\n' {
            let code_on_line = (line_start..i).any(|k| ctx.is_code(k) && !ctx.is_space(k));
            if code_on_line {
                code_start = Some(line_start);
                break;
            }
            let first = (line_start..i).find(|k| !ctx.is_space(*k));
            match first.and_then(|k| ctx.comment_at(k)) {
                // No trim: trailing spaces in a doc comment are a Markdown
                // hard break, not formatting.
                Some(span) => docs.push(ctx.slice(span.start, span.end)),
                None => docs.push(String::new()),
            }
            line_start = i + 1;
        }
        i += 1;
    }
    while docs.last().is_some_and(String::is_empty) {
        docs.pop();
    }
    while docs.first().is_some_and(String::is_empty) {
        docs.remove(0);
    }
    match code_start {
        Some(code_start) => {
            let mut code_end = end;
            while code_end > code_start
                && !(ctx.is_code(code_end - 1) && !ctx.is_space(code_end - 1))
            {
                code_end -= 1;
            }
            (docs, code_start, code_end)
        }
        None => (docs, end, end),
    }
}

/// `[start, end)` with whitespace runs in code collapsed to one space.
/// Literal spans are copied verbatim, so nothing inside a string or char is
/// ever rewritten; a comment in the range is refused by the caller before we
/// get here.
fn normalize(ctx: &Ctx<'_>, start: usize, end: usize) -> String {
    if start >= end {
        return String::new();
    }
    let mut out = String::new();
    let mut space = false;
    let emit = |out: &mut String, space: &mut bool, piece: &str| {
        if *space && !out.is_empty() {
            out.push(' ');
        }
        *space = false;
        out.push_str(piece);
    };
    for span in &ctx.spans {
        let (a, b) = (span.start.max(start), span.end.min(end));
        if a >= b {
            continue;
        }
        let text = ctx.slice(a, b);
        match span.kind {
            Kind::Code => {
                for c in text.chars() {
                    if c.is_whitespace() {
                        space = true;
                    } else {
                        emit(&mut out, &mut space, &c.to_string());
                    }
                }
            }
            // Literals and comments are copied verbatim: their contents are
            // never reflowed, only the code around them.
            Kind::Literal | Kind::LineComment | Kind::BlockComment => {
                emit(&mut out, &mut space, &text)
            }
        }
    }
    out.trim().to_string()
}

/// Canonical token stream: code split into words, literals verbatim, each
/// comment as one marker-prefixed unit. Two sources with equal token streams
/// differ only in whitespace that cannot change meaning.
fn tokens(src: &str) -> Vec<String> {
    let ctx = Ctx::new(src);
    let mut out = Vec::new();
    for span in &ctx.spans {
        let text = ctx.slice(span.start, span.end);
        match span.kind {
            Kind::Code => out.extend(tokenize_code(&text)),
            Kind::Literal => out.push(format!("\u{1}{text}")),
            // Verbatim: a reflow may move a comment, never edit its text.
            Kind::LineComment | Kind::BlockComment => out.push(format!("\u{2}{text}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const PACKED: &str = r#"register_bitfields![u32,
    pub BRIDGE_BUSES [
        PRIMARY OFFSET(0) NUMBITS(8) [], SECONDARY OFFSET(8) NUMBITS(8) [],
        SUBORDINATE OFFSET(16) NUMBITS(8) []
    ],
    pub MODES [
        P2P OFFSET(1) NUMBITS(2) [Drop = 0, Forward = 1],
        OTHER OFFSET(5) NUMBITS(1) []
    ]
];
"#;

    const FORMATTED: &str = r#"register_bitfields![u32,
    pub BRIDGE_BUSES [
        PRIMARY OFFSET(0) NUMBITS(8) [],
        SECONDARY OFFSET(8) NUMBITS(8) [],
        SUBORDINATE OFFSET(16) NUMBITS(8) []
    ],
    pub MODES [
        P2P OFFSET(1) NUMBITS(2) [
            Drop = 0,
            Forward = 1
        ],
        OTHER OFFSET(5) NUMBITS(1) []
    ]
];
"#;

    fn violations(src: &str) -> String {
        check_source(src)
            .iter()
            .map(|v| v.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn packed_lines_are_reported() {
        let found = check_source(PACKED);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[0].line, 3);
        assert!(found[0].message.contains("2 bitfields"));
        assert_eq!(found[1].line, 7);
        assert!(found[1].message.contains("2 enum variants"));
    }

    #[test]
    fn formatted_source_is_clean() {
        assert!(check_source(FORMATTED).is_empty());
        assert_eq!(fix_source(FORMATTED).unwrap(), None);
    }

    #[test]
    fn fix_splits_fields_and_variants() {
        assert_eq!(fix_source(PACKED).unwrap().as_deref(), Some(FORMATTED));
    }

    #[test]
    fn fix_is_idempotent() {
        let once = fix_source(PACKED).unwrap().unwrap();
        assert_eq!(fix_source(&once).unwrap(), None);
    }

    #[test]
    fn comments_that_look_like_fields_are_ignored() {
        let src = r#"register_bitfields![u8,
    pub LCR [
        /// Word Length Select: 00=5, 01=6, 10=7, 11=8 bits.
        WLS OFFSET(0) NUMBITS(2) []
    ]
];
"#;
        assert!(check_source(src).is_empty());
        assert_eq!(fix_source(src).unwrap(), None);
    }

    #[test]
    fn a_macro_shown_in_a_doc_comment_is_not_a_macro() {
        let src = "//! ```ignore\n//! register_bitfields![u32,\n//!     CTRL [EN OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]\n//! ];\n//! ```\n";
        assert!(check_source(src).is_empty());
        assert_eq!(fix_source(src).unwrap(), None);
    }

    #[test]
    fn single_field_registers_stay_on_one_line() {
        let src = r#"register_bitfields![u32,
    pub SMRR_MASK [VALID OFFSET(11) NUMBITS(1) []]
];
"#;
        assert_eq!(fix_source(src).unwrap(), None);
    }

    #[test]
    fn trailing_commas_survive_the_fix() {
        let src = r#"register_bitfields![u32,
    pub A [
        X OFFSET(0) NUMBITS(1) [], Y OFFSET(1) NUMBITS(1) [],
    ],
];
"#;
        let fixed = fix_source(src).unwrap().unwrap();
        assert_eq!(tokens(&fixed), tokens(src));
        assert!(
            fixed.contains("Y OFFSET(1) NUMBITS(1) [],\n    ],"),
            "{fixed}"
        );
    }

    #[test]
    fn indentation_follows_the_invocation() {
        let src = r#"fn f() {
    register_bitfields![u32,
        pub A [X OFFSET(0) NUMBITS(1) [], Y OFFSET(1) NUMBITS(1) []]
    ];
}
"#;
        let fixed = fix_source(src).unwrap().unwrap();
        assert!(
            fixed.contains("\n        pub A [\n            X OFFSET(0) NUMBITS(1) [],\n"),
            "{fixed}"
        );
        assert!(fixed.contains("\n    ];\n}\n"), "{fixed}");
    }

    // -- regressions from review -------------------------------------------

    #[test]
    fn whitespace_inside_a_literal_is_never_touched() {
        let src = r#"register_bitfields![u32,
    pub A [
        X OFFSET(0) NUMBITS(2) [Wide = "a  b".len() as u32, B = 0]
    ]
];
"#;
        let fixed = fix_source(src).unwrap().unwrap();
        assert!(fixed.contains(r#"Wide = "a  b".len() as u32,"#), "{fixed}");
        assert_eq!(tokens(&fixed), tokens(src));
        assert!(check_source(&fixed).is_empty());
    }

    #[test]
    fn a_trailing_comment_does_not_swallow_the_next_variant() {
        let src = r#"register_bitfields![u32,
    pub A [
        X OFFSET(0) NUMBITS(2) [Off = 0, On = 1, // disabled
         Extra = 2]
    ]
];
"#;
        let fixed = fix_source(src).unwrap().unwrap();
        for expected in ["Off = 0,", "On = 1,", "// disabled", "Extra = 2"] {
            assert!(fixed.contains(expected), "missing {expected} in {fixed}");
        }
        assert_eq!(tokens(&fixed), tokens(src));
    }

    #[test]
    fn unicode_before_a_macro_does_not_panic() {
        let src = format!("const C: char = 'é';\n{PACKED}");
        assert_eq!(check_source(&src).len(), 2);
        assert!(fix_source(&src).unwrap().is_some());
    }

    #[test]
    fn an_empty_string_literal_does_not_hide_a_macro() {
        let src = format!("const EMPTY: &str = \"\";\n{PACKED}");
        assert_eq!(check_source(&src).len(), 2, "{}", violations(&src));
    }

    #[test]
    fn a_quote_char_literal_does_not_hide_a_macro() {
        let src = format!("const QUOTE: char = '\"';\n{PACKED}");
        assert_eq!(check_source(&src).len(), 2, "{}", violations(&src));
    }

    #[test]
    fn a_macro_inside_a_nested_block_comment_is_not_a_macro() {
        let src = "/* outer /* inner */\nregister_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]];\n*/\n";
        assert!(check_source(src).is_empty(), "{}", violations(src));
        assert_eq!(fix_source(src).unwrap(), None);
    }

    #[test]
    fn token_spacing_does_not_bypass_the_check() {
        let src = "register_bitfields![u32, R [A OFFSET (0) NUMBITS(1) [], B OFFSET (1) NUMBITS(1) []]];\n";
        assert_eq!(check_source(src).len(), 1, "{}", violations(src));
    }

    #[test]
    fn a_space_before_the_bang_is_still_the_macro() {
        let src = "register_bitfields ! [u32, R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]];\n";
        assert_eq!(check_source(src).len(), 1, "{}", violations(src));
    }

    #[test]
    fn an_equality_in_a_variant_value_is_not_a_second_variant() {
        let alone = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [V = (1 == 1) as u32]]];\n";
        assert!(check_source(alone).is_empty(), "{}", violations(alone));
        let pair =
            "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [V = (1 == 1) as u32, W = 2]]];\n";
        assert_eq!(check_source(pair).len(), 1, "{}", violations(pair));
        let fixed = fix_source(pair).unwrap().unwrap();
        assert!(check_source(&fixed).is_empty(), "{fixed}");
        assert!(fixed.contains("V = (1 == 1) as u32,"), "{fixed}");
    }

    #[test]
    fn a_bracket_inside_a_literal_does_not_break_the_split() {
        let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [V = \"[\".len() as u32, W = 2]]];\n";
        let fixed = fix_source(src).unwrap().unwrap();
        assert!(check_source(&fixed).is_empty(), "{fixed}");
        assert_eq!(tokens(&fixed), tokens(src));
    }

    #[test]
    fn crlf_is_checked_but_not_rewritten() {
        let src = "register_bitfields![u32,\r\n R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]\r\n];\r\n";
        assert_eq!(check_source(src).len(), 1);
        assert!(fix_source(src).is_err());
    }

    #[test]
    fn a_blank_line_inside_a_doc_block_survives() {
        let src = r#"register_bitfields![u32,
    // ---------------------------------------------
    // Legacy GMCH connector and panel registers.
    // ---------------------------------------------

    /// ADPA VGA/CRT port control.
    pub ADPA [
        A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []
    ]
];
"#;
        let fixed = fix_source(src).unwrap().unwrap();
        assert!(
            fixed.contains("    // ---------------------------------------------\n\n    /// ADPA"),
            "{fixed}"
        );
        assert_eq!(tokens(&fixed), tokens(src));
    }

    #[test]
    fn trailing_spaces_in_a_doc_comment_are_preserved() {
        let src = "register_bitfields![u32,\n    /// First line.  \n    /// Second line.\n    R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]\n];\n";
        let fixed = fix_source(src).unwrap().unwrap();
        assert!(fixed.contains("/// First line.  \n"), "{fixed:?}");
        assert_eq!(tokens(&fixed), tokens(src));
    }

    #[test]
    fn attributes_do_not_hide_fields_or_variants() {
        let on_register = "register_bitfields![u32, #[doc = \"R\"] R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]];\n";
        assert_eq!(
            check_source(on_register).len(),
            1,
            "{}",
            violations(on_register)
        );
        let on_field =
            "register_bitfields![u32, R [#[doc = \"a\"] A OFFSET(0) NUMBITS(2) [V = 0, W = 1]]];\n";
        assert_eq!(check_source(on_field).len(), 1, "{}", violations(on_field));
    }

    #[test]
    fn a_nested_register_macro_is_fixed() {
        let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [V = { register_bitfields![u32, S [B OFFSET(0) NUMBITS(1) [], C OFFSET(1) NUMBITS(1) []]]; 0 }]]];\n";
        let fixed = fix_source(src).unwrap().unwrap();
        assert!(check_source(&fixed).is_empty(), "{fixed}");
        assert_eq!(tokens(&fixed), tokens(src));
        assert!(fixed.contains("B OFFSET(0) NUMBITS(1) [],\n"), "{fixed}");
        assert!(fixed.contains("C OFFSET(1) NUMBITS(1) []\n"), "{fixed}");
    }

    #[test]
    fn a_comment_between_invocation_tokens_is_still_the_macro() {
        let src = "register_bitfields /* hi */ ! [u32, R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]];\n";
        assert_eq!(check_source(src).len(), 1, "{}", violations(src));
        let src = "register_bitfields! /* hi */ [u32, R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]];\n";
        assert_eq!(check_source(src).len(), 1, "{}", violations(src));
    }

    #[test]
    fn attributes_survive_a_fix() {
        let src = "register_bitfields![u32, #[doc = \"S\"] S [C OFFSET(0) NUMBITS(1) [], D OFFSET(1) NUMBITS(1) []]];\n";
        let fixed = fix_source(src).unwrap().unwrap();
        assert!(fixed.contains("#[doc = \"S\"] S ["), "{fixed}");
        assert_eq!(tokens(&fixed), tokens(src));
        assert!(check_source(&fixed).is_empty(), "{fixed}");
        let src =
            "register_bitfields![u32, R [#[doc = \"a\"] A OFFSET(0) NUMBITS(2) [V = 0, W = 1]]];\n";
        let fixed = fix_source(src).unwrap().unwrap();
        assert!(
            fixed.contains("#[doc = \"a\"] A OFFSET(0) NUMBITS(2) ["),
            "{fixed}"
        );
        assert_eq!(tokens(&fixed), tokens(src));
    }

    #[test]
    fn an_unbalanced_attribute_bracket_does_not_panic() {
        let src = "register_bitfields![u32, R [#[doc = \"x\" A OFFSET(0) NUMBITS(1) []]]];\n";
        let _ = check_source(src);
        let _ = fix_source(src);
    }

    #[test]
    fn a_turbofish_comma_is_not_a_second_variant() {
        let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [V = crate::value::<u8, u16>()]]];\n";
        assert!(check_source(src).is_empty(), "{}", violations(src));
        let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [V = a < b]]];\n";
        assert!(check_source(src).is_empty(), "{}", violations(src));
    }

    #[test]
    fn a_similarly_named_macro_is_ignored() {
        let src = "macro_rules! éregister_bitfields { ($($t:tt)*) => {}; }\néregister_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [], B OFFSET(1) NUMBITS(1) []]];\n";
        assert!(check_source(src).is_empty(), "{}", violations(src));
    }

    #[test]
    fn a_nested_register_macro_is_checked() {
        let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [V = { register_bitfields![u32, S [B OFFSET(0) NUMBITS(1) [], C OFFSET(1) NUMBITS(1) []]]; 0 }]]];\n";
        assert_eq!(check_source(src).len(), 1, "{}", violations(src));
    }

    #[test]
    fn a_single_field_trailing_comma_still_fixes() {
        let src = "register_bitfields![u32, R [A OFFSET(0) NUMBITS(1) [],], S [B OFFSET(0) NUMBITS(1) [], C OFFSET(1) NUMBITS(1) []]];\n";
        let fixed = fix_source(src).unwrap().unwrap();
        assert_eq!(tokens(&fixed), tokens(src));
        assert!(check_source(&fixed).is_empty(), "{fixed}");
        assert!(fixed.contains("R [A OFFSET(0) NUMBITS(1) [],],"), "{fixed}");
    }

    #[test]
    fn a_rewrite_that_would_glue_tokens_is_refused() {
        // If the guard compared whitespace-stripped text, `a b` and `ab` would
        // look identical; the token stream must keep them apart.
        assert_ne!(tokens("const a b;"), tokens("const ab;"));
    }
}
