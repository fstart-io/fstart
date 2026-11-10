// SPDX-License-Identifier: MIT OR Apache-2.0
//! Layout checks for the custom syntax inside `register_bitfields!`.
//!
//! `proc_macro2` supplies token trees and original byte spans. Formatting only
//! inserts or replaces whitespace at list boundaries; it never re-emits tokens,
//! comments, or literals. Inline block comments at a boundary are deliberately
//! refused rather than guessed to belong to the preceding or following item.

use proc_macro2::{Delimiter, Group, TokenStream, TokenTree};
use std::ops::Range;

pub const MACRO_NAME: &str = "register_bitfields";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.message)
    }
}

struct Edit {
    range: Range<usize>,
    replacement: String,
}

#[derive(Default)]
struct Plan {
    violations: Vec<Violation>,
    edits: Vec<Edit>,
}

fn punct(token: &TokenTree, ch: char) -> bool {
    matches!(token, TokenTree::Punct(p) if p.as_char() == ch)
}

fn ident(token: &TokenTree, name: &str) -> bool {
    matches!(token, TokenTree::Ident(i) if i == name)
}

fn group(token: &TokenTree, delimiter: Delimiter) -> Option<&Group> {
    match token {
        TokenTree::Group(g) if g.delimiter() == delimiter => Some(g),
        _ => None,
    }
}

fn attributes(mut tokens: &[TokenTree]) -> &[TokenTree] {
    while tokens.len() >= 2
        && punct(&tokens[0], '#')
        && group(&tokens[1], Delimiter::Bracket).is_some()
    {
        tokens = &tokens[2..];
    }
    tokens
}

// Groups already hide commas in expressions. A turbofish also hides commas,
// but a comparison (`a < b`) must not hide the next variant's separator.
fn split(tokens: &[TokenTree]) -> Vec<&[TokenTree]> {
    let mut items = Vec::new();
    let (mut start, mut angle) = (0, 0usize);
    for (i, token) in tokens.iter().enumerate() {
        if punct(token, '<')
            && (angle > 0 || i >= 2 && punct(&tokens[i - 1], ':') && punct(&tokens[i - 2], ':'))
        {
            angle += 1;
        } else if punct(token, '>') && angle > 0 {
            angle -= 1;
        } else if punct(token, ',') && angle == 0 {
            items.push(&tokens[start..i]);
            start = i + 1;
        }
    }
    if start < tokens.len() {
        items.push(&tokens[start..]);
    }
    items
}

fn plan(src: &str) -> Result<Plan, String> {
    // Strip a BOM/shebang without changing file-relative byte offsets.
    let mut input = src.to_string();
    if input.starts_with('\u{feff}') {
        input.replace_range(..3, "   ");
    }
    let start = if src.starts_with('\u{feff}') { 3 } else { 0 };
    if input[start..].starts_with("#!") && !input[start..].starts_with("#![") {
        let end = input[start..].find('\n').map_or(input.len(), |i| start + i);
        input.replace_range(start..end, &" ".repeat(end - start));
    }
    let stream = input
        .parse::<TokenStream>()
        .map_err(|e| format!("cannot tokenize source: {e}"))?;
    let mut plan = Plan::default();
    walk(src, stream, &mut plan)?;
    plan.violations.sort_by_key(|v| v.line);
    plan.edits.sort_by_key(|e| (e.range.start, e.range.end));
    for pair in plan.edits.windows(2) {
        if pair[0].range.end > pair[1].range.start {
            return Err("overlapping layout edits; reflow it by hand".into());
        }
    }
    Ok(plan)
}

fn walk(src: &str, stream: TokenStream, plan: &mut Plan) -> Result<(), String> {
    let tokens: Vec<_> = stream.into_iter().collect();
    for (i, token) in tokens.iter().enumerate() {
        if ident(token, MACRO_NAME)
            && tokens.get(i + 1).is_some_and(|t| punct(t, '!'))
            && let Some(TokenTree::Group(body)) = tokens.get(i + 2)
        {
            registers(src, body, plan)?;
        }
        if let TokenTree::Group(g) = token {
            walk(src, g.stream(), plan)?;
        }
    }
    Ok(())
}

fn registers(src: &str, body: &Group, plan: &mut Plan) -> Result<(), String> {
    // Macro definitions may substitute names, widths or entire field lists.
    // Check their concrete invocations instead of pretending to parse `$...`.
    fn template(stream: TokenStream) -> bool {
        stream.into_iter().any(|t| {
            punct(&t, '$')
                || match t {
                    TokenTree::Group(g) => template(g.stream()),
                    _ => false,
                }
        })
    }
    if template(body.stream()) {
        return Ok(());
    }
    let tokens: Vec<_> = body.stream().into_iter().collect();
    let entries = split(&tokens);
    let Some((width, registers)) = entries.split_first() else {
        return Err("register_bitfields! without a width".into());
    };
    if width.len() != 1 || !matches!(&width[0], TokenTree::Ident(_)) {
        return Err("unrecognised register_bitfields! width".into());
    }
    for entry in registers {
        let mut entry = attributes(entry);
        if entry.first().is_some_and(|t| ident(t, "pub")) {
            entry = &entry[1..];
            if entry
                .first()
                .and_then(|t| group(t, Delimiter::Parenthesis))
                .is_some()
            {
                entry = &entry[1..];
            }
        }
        let [TokenTree::Ident(_), TokenTree::Group(fields)] = entry else {
            return Err("unrecognised register declaration".into());
        };
        if fields.delimiter() != Delimiter::Bracket {
            return Err("register field list must use brackets".into());
        }
        fields_list(src, fields, plan)?;
    }
    Ok(())
}

fn fields_list(src: &str, fields: &Group, plan: &mut Plan) -> Result<(), String> {
    let tokens: Vec<_> = fields.stream().into_iter().collect();
    let items = split(&tokens);
    let indent = layout(src, fields, &items, "bitfields", "field", None, plan)?;
    for (i, item) in items.iter().enumerate() {
        let entry = attributes(item);
        let [
            TokenTree::Ident(_),
            offset,
            offset_value,
            bits,
            bits_value,
            variants,
        ] = entry
        else {
            return Err("unrecognised bitfield declaration".into());
        };
        if !ident(offset, "OFFSET")
            || !ident(bits, "NUMBITS")
            || group(offset_value, Delimiter::Parenthesis).is_none()
            || group(bits_value, Delimiter::Parenthesis).is_none()
        {
            return Err("unrecognised bitfield OFFSET/NUMBITS".into());
        }
        let variants =
            group(variants, Delimiter::Bracket).ok_or("bitfield variant list must use brackets")?;
        let tokens: Vec<_> = variants.stream().into_iter().collect();
        let variants_items = split(&tokens);
        for variant in &variants_items {
            if !matches!(attributes(variant), [TokenTree::Ident(_), eq, _, ..] if punct(eq, '=')) {
                return Err("unrecognised enum variant".into());
            }
        }
        let parent = indent.as_ref().map(|indents| indents[i].as_str());
        layout(
            src,
            variants,
            &variants_items,
            "enum variants",
            "variant",
            parent,
            plan,
        )?;
    }
    Ok(())
}

fn leading_indent(src: &str, at: usize) -> &str {
    let start = src[..at].rfind('\n').map_or(0, |i| i + 1);
    let line = &src[start..at];
    &line[..line.len() - line.trim_start_matches([' ', '\t']).len()]
}

// Return the effective indentation of elements so nested lists can use their
// parent's new indentation even though all spans refer to the original source.
fn layout(
    src: &str,
    list: &Group,
    items: &[&[TokenTree]],
    plural: &str,
    singular: &str,
    parent: Option<&str>,
    plan: &mut Plan,
) -> Result<Option<Vec<String>>, String> {
    let mut lines = std::collections::BTreeMap::<usize, usize>::new();
    for item in items {
        let token = attributes(item).first().ok_or("empty list item")?;
        *lines.entry(token.span().start().line).or_default() += 1;
    }
    let mut packed = false;
    for (line, count) in lines {
        if count > 1 {
            packed = true;
            plan.violations.push(Violation {
                line,
                message: format!("{count} {plural} on one line; one {singular} per line"),
            });
        }
    }
    if !packed {
        return Ok(None);
    }
    let range = list.span().byte_range();
    let base = parent.unwrap_or_else(|| leading_indent(src, range.start));
    let indent = format!("{base}    ");
    let tokens: Vec<_> = list.stream().into_iter().collect();
    let mut previous = range.start + 1;
    let mut indents = Vec::new();
    for item in items {
        let start = item[0].span().byte_range().start;
        let gap = &src[previous..start];
        if !gap.contains('\n') {
            whitespace_edit(src, previous..start, format!("\n{indent}"), plan)?;
            indents.push(indent.clone());
        } else {
            indents.push(leading_indent(src, start).to_string());
        }
        previous = item.last().unwrap().span().byte_range().end;
        // Whitespace may precede a comma; locate it by the next token.
        if let Some(comma) = tokens
            .iter()
            .find(|t| t.span().byte_range().start >= previous)
            && punct(comma, ',')
        {
            previous = comma.span().byte_range().end;
        }
    }
    let close = range.end - 1;
    if !src[previous..close].contains('\n') {
        // Keep a final block comment next to its original item. Insertion at
        // the closing delimiter never moves or rewrites the comment itself.
        let start = if src[previous..close].chars().all(char::is_whitespace) {
            previous
        } else {
            close
        };
        whitespace_edit(src, start..close, format!("\n{base}"), plan)?;
    }
    Ok(Some(indents))
}

fn whitespace_edit(
    src: &str,
    range: Range<usize>,
    replacement: String,
    plan: &mut Plan,
) -> Result<(), String> {
    if !src[range.clone()].chars().all(char::is_whitespace) {
        return Err(format!(
            "ambiguous inline comment at byte {}; reflow it by hand",
            range.start
        ));
    }
    assert!(!replacement.is_empty() && replacement.chars().all(char::is_whitespace));
    plan.edits.push(Edit { range, replacement });
    Ok(())
}

pub fn check_source(src: &str) -> Result<Vec<Violation>, String> {
    Ok(plan(src)?.violations)
}

pub fn fix_source(src: &str) -> Result<Option<String>, String> {
    let plan = plan(src)?;
    if plan.edits.is_empty() {
        return Ok(None);
    }
    if src.contains('\r') {
        return Err("CRLF line endings; reflow it by hand".into());
    }
    let mut out = src.to_string();
    for edit in plan.edits.into_iter().rev() {
        out.replace_range(edit.range, &edit.replacement);
    }
    if !check_source(&out)?.is_empty() {
        return Err("reflow left violations; not written".into());
    }
    Ok(Some(out))
}
