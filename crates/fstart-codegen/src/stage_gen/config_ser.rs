//! Convert a [`DriverInstance`] into construction token streams.
//!
//! Uses a custom serde [`Serializer`](serde::Serializer) to walk the
//! driver's typed config struct and emit a Rust struct literal as a
//! [`TokenStream`].  Adding a new driver requires **zero changes** here —
//! just derive `Serialize` on the config struct and add a one-line
//! `serialize_config` arm in [`DriverInstance`].

use std::fmt;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::ser::{
    self, Impossible, Serialize, SerializeSeq, SerializeStruct, SerializeStructVariant,
    SerializeTuple, SerializeTupleStruct, SerializeTupleVariant,
};

use fstart_device_registry::DriverInstance;

use super::tokens::hex_addr;

// =======================================================================
// Public API
// =======================================================================

/// Serialize any [`Serialize`] value to tokens (for testing).
#[cfg(test)]
pub(super) fn serialize_to_tokens<T: Serialize>(value: &T) -> TokenStream {
    value
        .serialize(ConfigTokenSerializer)
        .unwrap_or_else(|e| panic!("failed to serialize to tokens: {e}"))
}

/// Generate the config struct constructor tokens for a driver instance.
///
/// Returns tokens like:
/// ```text
/// Ns16550Config { base_addr: 0x10000000, clock_freq: 3686400u32, baud_rate: 115200u32 }
/// ```
///
/// This delegates to [`DriverInstance::serialize_config`] with a custom
/// serde serializer — no per-driver field knowledge needed.
pub(super) fn config_tokens(instance: &DriverInstance) -> TokenStream {
    instance
        .serialize_config(ConfigTokenSerializer)
        .unwrap_or_else(|e| panic!("failed to serialize driver config to tokens: {e}"))
}

/// Generate the driver type constructor tokens (e.g., `Ns16550`).
pub(super) fn driver_type_tokens(instance: &DriverInstance) -> TokenStream {
    let type_name = format_ident!("{}", instance.meta().type_name);
    quote! { #type_name }
}

// =======================================================================
// Error type
// =======================================================================

/// Error type for the config-to-tokens serializer.
#[derive(Debug)]
struct TokenError(String);

impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for TokenError {}

impl ser::Error for TokenError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        Self(msg.to_string())
    }
}

// =======================================================================
// Serializer — converts serde data model to TokenStream
// =======================================================================

/// Serde serializer that produces Rust token streams.
///
/// Handles the serde data model types that appear in driver config structs:
///
/// - **Structs** → `StructName { field: value, ... }`
/// - **Integers** → decimal for small types, hex for `u64` (addresses)
/// - **Booleans** → `true` / `false`
/// - **Strings** → string literal
/// - **Chars** → char literal
/// - **Bytes** → `&[0u8, 1u8, ...]`
/// - **Option** → `None` / `Some(value)`
/// - **Unit** → `()`
/// - **Unit struct** → `StructName`
/// - **Unit enum variant** → `EnumName::Variant`
/// - **Newtype struct** → `StructName(value)`
/// - **Newtype variant** → `EnumName::Variant(value)`
/// - **Sequences / tuples** → `[elem, ...]`
/// - **Tuple struct** → `StructName(elem, ...)`
/// - **Tuple variant** → `EnumName::Variant(elem, ...)`
/// - **Struct variant** → `EnumName::Variant { field: value, ... }`
///
/// Maps and floating-point values are not supported (uncommon in driver
/// configs).
struct ConfigTokenSerializer;

/// Helper macro for unsupported serializer methods.
macro_rules! unsupported {
    ($name:ident, $($arg:ident : $ty:ty),*) => {
        fn $name(self, $($arg: $ty),*) -> Result<Self::Ok, Self::Error> {
            $(let _ = $arg;)*
            Err(TokenError(format!(
                "driver config serialization does not support {}",
                stringify!($name),
            )))
        }
    };
}

impl ser::Serializer for ConfigTokenSerializer {
    type Ok = TokenStream;
    type Error = TokenError;

    type SerializeSeq = SeqTokenSerializer;
    type SerializeTuple = TupleTokenSerializer;
    type SerializeTupleStruct = TupleStructTokenSerializer;
    type SerializeTupleVariant = TupleVariantTokenSerializer;
    type SerializeMap = Impossible<TokenStream, TokenError>;
    type SerializeStruct = StructTokenSerializer;
    type SerializeStructVariant = StructVariantTokenSerializer;

    // -- Primitives -------------------------------------------------------

    fn serialize_bool(self, v: bool) -> Result<TokenStream, TokenError> {
        Ok(quote! { #v })
    }

    fn serialize_i8(self, v: i8) -> Result<TokenStream, TokenError> {
        Ok(quote! { #v })
    }
    fn serialize_i16(self, v: i16) -> Result<TokenStream, TokenError> {
        Ok(quote! { #v })
    }
    fn serialize_i32(self, v: i32) -> Result<TokenStream, TokenError> {
        Ok(quote! { #v })
    }
    fn serialize_i64(self, v: i64) -> Result<TokenStream, TokenError> {
        Ok(quote! { #v })
    }

    fn serialize_u8(self, v: u8) -> Result<TokenStream, TokenError> {
        Ok(quote! { #v })
    }
    fn serialize_u16(self, v: u16) -> Result<TokenStream, TokenError> {
        Ok(quote! { #v })
    }
    fn serialize_u32(self, v: u32) -> Result<TokenStream, TokenError> {
        Ok(quote! { #v })
    }

    /// `u64` values are emitted as hex — these are typically MMIO base addresses.
    fn serialize_u64(self, v: u64) -> Result<TokenStream, TokenError> {
        Ok(hex_addr(v))
    }

    /// Serialize a string.
    ///
    /// `heapless::String<N>` serializes via serde as a plain `str`.
    /// Since driver config structs use `heapless::String` (not `&str`),
    /// we emit a construction expression that works for both:
    /// `heapless::String::try_from("...").unwrap()`.
    fn serialize_str(self, v: &str) -> Result<TokenStream, TokenError> {
        Ok(quote! { heapless::String::try_from(#v).unwrap() })
    }

    fn serialize_char(self, v: char) -> Result<TokenStream, TokenError> {
        Ok(quote! { #v })
    }

    fn serialize_bytes(self, v: &[u8]) -> Result<TokenStream, TokenError> {
        Ok(quote! { &[#(#v),*] })
    }

    // -- Option -----------------------------------------------------------

    fn serialize_none(self) -> Result<TokenStream, TokenError> {
        Ok(quote! { None })
    }

    fn serialize_some<T: ?Sized + Serialize>(self, value: &T) -> Result<TokenStream, TokenError> {
        let inner = value.serialize(ConfigTokenSerializer)?;
        Ok(quote! { Some(#inner) })
    }

    // -- Unit types -------------------------------------------------------

    fn serialize_unit(self) -> Result<TokenStream, TokenError> {
        Ok(quote! { () })
    }

    fn serialize_unit_struct(self, name: &'static str) -> Result<TokenStream, TokenError> {
        let ident = format_ident!("{}", name);
        Ok(quote! { #ident })
    }

    /// Fieldless enum variant (e.g., `I2cSpeed::Fast`).
    fn serialize_unit_variant(
        self,
        name: &'static str,
        _variant_index: u32,
        variant: &'static str,
    ) -> Result<TokenStream, TokenError> {
        let enum_ident = format_ident!("{}", name);
        let variant_ident = format_ident!("{}", variant);
        Ok(quote! { #enum_ident::#variant_ident })
    }

    // -- Newtype wrappers -------------------------------------------------

    fn serialize_newtype_struct<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        value: &T,
    ) -> Result<TokenStream, TokenError> {
        let ident = format_ident!("{}", name);
        let inner = value.serialize(ConfigTokenSerializer)?;
        Ok(quote! { #ident(#inner) })
    }

    fn serialize_newtype_variant<T: ?Sized + Serialize>(
        self,
        name: &'static str,
        _idx: u32,
        variant: &'static str,
        value: &T,
    ) -> Result<TokenStream, TokenError> {
        let enum_ident = format_ident!("{}", name);
        let variant_ident = format_ident!("{}", variant);
        let inner = value.serialize(ConfigTokenSerializer)?;
        Ok(quote! { #enum_ident::#variant_ident(#inner) })
    }

    // -- Compound types ---------------------------------------------------

    /// Named-field struct (the config struct itself).
    fn serialize_struct(
        self,
        name: &'static str,
        _len: usize,
    ) -> Result<StructTokenSerializer, TokenError> {
        Ok(StructTokenSerializer {
            name: name.to_string(),
            fields: Vec::new(),
        })
    }

    fn serialize_seq(self, _len: Option<usize>) -> Result<Self::SerializeSeq, TokenError> {
        Ok(SeqTokenSerializer {
            elements: Vec::new(),
        })
    }

    fn serialize_tuple(self, _len: usize) -> Result<Self::SerializeTuple, TokenError> {
        Ok(TupleTokenSerializer {
            elements: Vec::new(),
        })
    }

    fn serialize_tuple_struct(
        self,
        name: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleStruct, TokenError> {
        Ok(TupleStructTokenSerializer {
            name: name.to_string(),
            elements: Vec::new(),
        })
    }

    fn serialize_tuple_variant(
        self,
        name: &'static str,
        _idx: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeTupleVariant, TokenError> {
        Ok(TupleVariantTokenSerializer {
            enum_name: name.to_string(),
            variant: variant.to_string(),
            elements: Vec::new(),
        })
    }

    fn serialize_struct_variant(
        self,
        name: &'static str,
        _idx: u32,
        variant: &'static str,
        _len: usize,
    ) -> Result<Self::SerializeStructVariant, TokenError> {
        Ok(StructVariantTokenSerializer {
            enum_name: name.to_string(),
            variant: variant.to_string(),
            fields: Vec::new(),
        })
    }

    // -- Unsupported types ------------------------------------------------

    unsupported!(serialize_f32, v: f32);
    unsupported!(serialize_f64, v: f64);

    fn serialize_map(self, _len: Option<usize>) -> Result<Self::SerializeMap, TokenError> {
        Err(TokenError("maps not supported in driver configs".into()))
    }
}

// =======================================================================
// Compound type accumulators
// =======================================================================

/// Accumulates struct fields: `StructName { field: value, ... }`
struct StructTokenSerializer {
    name: String,
    fields: Vec<TokenStream>,
}

impl SerializeStruct for StructTokenSerializer {
    type Ok = TokenStream;
    type Error = TokenError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), TokenError> {
        let field_ident = format_ident!("{}", key);
        let field_tokens = value.serialize(ConfigTokenSerializer)?;
        self.fields.push(quote! { #field_ident: #field_tokens });
        Ok(())
    }

    fn end(self) -> Result<TokenStream, TokenError> {
        let struct_ident = format_ident!("{}", self.name);
        let fields = &self.fields;
        Ok(quote! {
            #struct_ident { #(#fields,)* }
        })
    }
}

/// Accumulates sequence elements: `[elem, ...]`
struct SeqTokenSerializer {
    elements: Vec<TokenStream>,
}

impl SerializeSeq for SeqTokenSerializer {
    type Ok = TokenStream;
    type Error = TokenError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), TokenError> {
        self.elements.push(value.serialize(ConfigTokenSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<TokenStream, TokenError> {
        let elems = &self.elements;
        Ok(quote! { [#(#elems),*] })
    }
}

/// Accumulates tuple / array elements: `[elem, ...]`
///
/// Serde serializes `[T; N]` arrays via `serialize_tuple`, so this
/// emits array literal syntax.
struct TupleTokenSerializer {
    elements: Vec<TokenStream>,
}

impl SerializeTuple for TupleTokenSerializer {
    type Ok = TokenStream;
    type Error = TokenError;

    fn serialize_element<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), TokenError> {
        self.elements.push(value.serialize(ConfigTokenSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<TokenStream, TokenError> {
        let elems = &self.elements;
        Ok(quote! { [#(#elems),*] })
    }
}

/// Accumulates tuple struct fields: `StructName(elem, ...)`
struct TupleStructTokenSerializer {
    name: String,
    elements: Vec<TokenStream>,
}

impl SerializeTupleStruct for TupleStructTokenSerializer {
    type Ok = TokenStream;
    type Error = TokenError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), TokenError> {
        self.elements.push(value.serialize(ConfigTokenSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<TokenStream, TokenError> {
        let ident = format_ident!("{}", self.name);
        let elems = &self.elements;
        Ok(quote! { #ident(#(#elems),*) })
    }
}

/// Accumulates tuple variant fields: `EnumName::Variant(elem, ...)`
struct TupleVariantTokenSerializer {
    enum_name: String,
    variant: String,
    elements: Vec<TokenStream>,
}

impl SerializeTupleVariant for TupleVariantTokenSerializer {
    type Ok = TokenStream;
    type Error = TokenError;

    fn serialize_field<T: ?Sized + Serialize>(&mut self, value: &T) -> Result<(), TokenError> {
        self.elements.push(value.serialize(ConfigTokenSerializer)?);
        Ok(())
    }

    fn end(self) -> Result<TokenStream, TokenError> {
        let enum_ident = format_ident!("{}", self.enum_name);
        let variant_ident = format_ident!("{}", self.variant);
        let elems = &self.elements;
        Ok(quote! { #enum_ident::#variant_ident(#(#elems),*) })
    }
}

/// Accumulates struct variant fields: `EnumName::Variant { field: value, ... }`
struct StructVariantTokenSerializer {
    enum_name: String,
    variant: String,
    fields: Vec<TokenStream>,
}

impl SerializeStructVariant for StructVariantTokenSerializer {
    type Ok = TokenStream;
    type Error = TokenError;

    fn serialize_field<T: ?Sized + Serialize>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), TokenError> {
        let field_ident = format_ident!("{}", key);
        let field_tokens = value.serialize(ConfigTokenSerializer)?;
        self.fields.push(quote! { #field_ident: #field_tokens });
        Ok(())
    }

    fn end(self) -> Result<TokenStream, TokenError> {
        let enum_ident = format_ident!("{}", self.enum_name);
        let variant_ident = format_ident!("{}", self.variant);
        let fields = &self.fields;
        Ok(quote! {
            #enum_ident::#variant_ident { #(#fields,)* }
        })
    }
}
