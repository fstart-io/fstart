//! Compile-time validation of ACPI names and method arguments.
//!
//! ACPI names must be 1-4 characters, `[A-Z0-9_]` only. Predefined
//! names like `_HID`, `_CRS`, `_STA` are allowed. Method argument
//! count must be 0-7.

use proc_macro2::Span;
use syn::{Error, Result};

use crate::parse::{DslItem, NameOrInterp};

/// Validate a list of DSL items.
pub fn validate_items(items: &[DslItem]) -> Result<()> {
    for item in items {
        validate_item(item)?;
    }
    Ok(())
}

fn validate_item(item: &DslItem) -> Result<()> {
    match item {
        DslItem::Scope {
            path,
            children,
            span,
        } => {
            // Only validate literal paths; interpolations are checked at runtime.
            if let NameOrInterp::Literal(p) = path {
                validate_acpi_path(p, *span)?;
            }
            validate_items(children)?;
        }
        DslItem::Device {
            name,
            children,
            span,
        } => {
            if let NameOrInterp::Literal(n) = name {
                validate_acpi_name(n, *span)?;
            }
            validate_items(children)?;
        }
        DslItem::Name { name, span, .. } => {
            validate_acpi_name(name, *span)?;
        }
        DslItem::Method {
            name,
            argc,
            body,
            span,
            ..
        } => {
            validate_acpi_name(name, *span)?;
            if *argc > 7 {
                return Err(Error::new(*span, "method argument count must be 0-7"));
            }
            validate_items(body)?;
        }
        DslItem::Return { .. } => {}
    }
    Ok(())
}

/// Validate an ACPI path (e.g., `\\_SB_`, `\\_SB.PCI0`).
fn validate_acpi_path(path: &str, span: Span) -> Result<()> {
    if path.is_empty() {
        return Err(Error::new(span, "ACPI path cannot be empty"));
    }

    // Root path or relative path
    let segments: Vec<&str> = if let Some(rest) = path.strip_prefix('\\') {
        if rest.is_empty() {
            return Ok(()); // root "\" alone is valid
        }
        rest.split('.').collect()
    } else {
        path.split('.').collect()
    };

    for seg in &segments {
        // Remove leading underscores for predefined names, then validate
        let clean = seg.trim_start_matches('_');
        if seg.is_empty() {
            return Err(Error::new(span, format!("empty path segment in `{path}`")));
        }
        if seg.len() > 4 {
            return Err(Error::new(
                span,
                format!("ACPI name segment `{seg}` exceeds 4 characters"),
            ));
        }
        // Allow A-Z, 0-9, _ (case-insensitive for validation)
        if !seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(Error::new(
                span,
                format!("ACPI name `{seg}` contains invalid characters (allowed: A-Z, 0-9, _)"),
            ));
        }
        let _ = clean; // suppress unused warning
    }
    Ok(())
}

/// Validate a single ACPI NameSeg (1-4 chars, `[A-Z0-9_]`).
fn validate_acpi_name(name: &str, span: Span) -> Result<()> {
    if name.is_empty() || name.len() > 4 {
        return Err(Error::new(
            span,
            format!("ACPI name `{name}` must be 1-4 characters"),
        ));
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(Error::new(
            span,
            format!("ACPI name `{name}` contains invalid characters (allowed: A-Z, 0-9, _)"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_names() {
        let span = Span::call_site();
        assert!(validate_acpi_name("_HID", span).is_ok());
        assert!(validate_acpi_name("PCI0", span).is_ok());
        assert!(validate_acpi_name("COM0", span).is_ok());
        assert!(validate_acpi_name("_CRS", span).is_ok());
        assert!(validate_acpi_name("_UID", span).is_ok());
    }

    #[test]
    fn test_invalid_names() {
        let span = Span::call_site();
        assert!(validate_acpi_name("", span).is_err());
        assert!(validate_acpi_name("TOOLONG", span).is_err());
        assert!(validate_acpi_name("bad!", span).is_err());
    }

    #[test]
    fn test_valid_paths() {
        let span = Span::call_site();
        assert!(validate_acpi_path("\\_SB_", span).is_ok());
        assert!(validate_acpi_path("\\_SB.PCI0", span).is_ok());
        assert!(validate_acpi_path("PCI0", span).is_ok());
        assert!(validate_acpi_path("\\", span).is_ok());
    }

    #[test]
    fn test_invalid_paths() {
        let span = Span::call_site();
        assert!(validate_acpi_path("", span).is_err());
    }
}
