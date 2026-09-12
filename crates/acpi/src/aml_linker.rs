//! Allocation-free linking into caller-owned storage.
//!
//! Scope emission reserves four PkgLength bytes before compacting. Capacity
//! must include this temporary slack at every active nesting level. Errors are
//! sticky: even an ignored write error prevents table finalization. A failed
//! scope rolls back its logical output; backing bytes must not be published.

use crate::{AmlError, AmlFragment, AmlFragmentSink, BoundAmlFragment};
use alloc::vec::Vec;

/// Validated AML namespace path. Empty segments, mixed root/parent prefixes,
/// invalid NameSeg characters and more than 255 segments are rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AmlPath<'a>(&'a str);

impl<'a> AmlPath<'a> {
    pub fn new(path: &'a str) -> Result<Self, AmlError> {
        let rest = if let Some(rest) = path.strip_prefix('\\') {
            rest
        } else {
            path.trim_start_matches('^')
        };
        if path.is_empty() || (rest.is_empty() && path != "\\" && !path.bytes().all(|b| b == b'^'))
        {
            return Err(AmlError::InvalidPath);
        }
        if !rest.is_empty() {
            if rest.split('.').count() > 255 {
                return Err(AmlError::InvalidPath);
            }
            for segment in rest.split('.') {
                let mut chars = segment.bytes();
                if segment.len() > 4
                    || !chars.next().is_some_and(name_lead)
                    || !chars.all(|b| name_lead(b) || b.is_ascii_digit())
                {
                    return Err(AmlError::InvalidPath);
                }
            }
        }
        Ok(Self(path))
    }

    pub fn as_str(self) -> &'a str {
        self.0
    }
}

fn name_lead(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_uppercase()
}

/// Sequential AML/table writer backed by caller-provided storage.
pub struct AmlWriter<'a> {
    bytes: &'a mut [u8],
    pos: usize,
    error: Option<AmlError>,
}

impl<'a> AmlWriter<'a> {
    pub fn new(bytes: &'a mut [u8]) -> Self {
        Self {
            bytes,
            pos: 0,
            error: None,
        }
    }
    pub fn position(&self) -> usize {
        self.pos
    }
    /// Only expose successful output, never a partially failed table.
    pub fn as_slice(&self) -> Result<&[u8], AmlError> {
        self.status()?;
        Ok(&self.bytes[..self.pos])
    }
    fn status(&self) -> Result<(), AmlError> {
        self.error.map_or(Ok(()), Err)
    }
    fn record<T>(&mut self, result: Result<T, AmlError>) -> Result<T, AmlError> {
        if let Err(error) = result {
            self.error.get_or_insert(error);
        }
        result
    }
    pub fn emit<const N: usize, const K: usize>(
        &mut self,
        fragment: &AmlFragment<N, K>,
        operands: &[u64; K],
    ) -> Result<(), AmlError> {
        self.status()?;
        let result = fragment.emit(self, operands);
        self.record(result)
    }
    pub fn emit_bound<const N: usize, const K: usize>(
        &mut self,
        fragment: &BoundAmlFragment<N, K>,
    ) -> Result<(), AmlError> {
        self.status()?;
        let result = fragment.emit(self);
        self.record(result)
    }
    pub fn raw(&mut self, bytes: &[u8]) -> Result<(), AmlError> {
        self.reserve(bytes.len())?.copy_from_slice(bytes);
        Ok(())
    }

    pub fn scope(
        &mut self,
        path: &str,
        children: impl FnOnce(&mut Self) -> Result<(), AmlError>,
    ) -> Result<(), AmlError> {
        let start = self.pos;
        let result = (|| {
            let path = AmlPath::new(path)?;
            self.byte(0x10)?;
            let package = self.pos;
            self.raw(&[0; 4])?;
            write_name_string(self, path)?;
            children(self)?;
            self.status()?;
            let end = self.pos;
            let (encoded, width) = package_length(end - package - 4)?;
            self.bytes.copy_within(package + 4..end, package + width);
            self.bytes[package..package + width].copy_from_slice(&encoded[..width]);
            self.pos -= 4 - width;
            Ok(())
        })();
        if result.is_err() {
            self.pos = start;
        }
        self.record(result)
    }

    /// Finalize only after every write and the body succeed. No table is
    /// returned on error, even if the body discarded a writer error.
    pub fn table(
        bytes: &'a mut [u8],
        signature: [u8; 4],
        revision: u8,
        oem_id: [u8; 6],
        oem_table_id: [u8; 8],
        oem_revision: u32,
        body: impl FnOnce(&mut Self) -> Result<(), AmlError>,
    ) -> Result<Self, AmlError> {
        let mut writer = Self::new(bytes);
        writer.raw(&signature)?;
        writer.dword(0)?;
        writer.byte(revision)?;
        writer.byte(0)?;
        writer.raw(&oem_id)?;
        writer.raw(&oem_table_id)?;
        writer.dword(oem_revision)?;
        writer.raw(b"FST0")?;
        writer.dword(1)?;
        body(&mut writer)?;
        writer.status()?;
        let length = u32::try_from(writer.pos).map_err(|_| AmlError::LengthOverflow)?;
        writer.bytes[4..8].copy_from_slice(&length.to_le_bytes());
        let checksum = writer
            .as_slice()?
            .iter()
            .fold(0u8, |sum, b| sum.wrapping_add(*b));
        writer.bytes[9] = 0u8.wrapping_sub(checksum);
        Ok(writer)
    }

    pub fn byte(&mut self, value: u8) -> Result<(), AmlError> {
        self.raw(&[value])
    }
    pub fn word(&mut self, value: u16) -> Result<(), AmlError> {
        self.raw(&value.to_le_bytes())
    }
    pub fn dword(&mut self, value: u32) -> Result<(), AmlError> {
        self.raw(&value.to_le_bytes())
    }
    pub fn qword(&mut self, value: u64) -> Result<(), AmlError> {
        self.raw(&value.to_le_bytes())
    }

    fn reserve(&mut self, len: usize) -> Result<&mut [u8], AmlError> {
        self.status()?;
        let end = self
            .pos
            .checked_add(len)
            .ok_or(AmlError::LengthOverflow)
            .and_then(|end| {
                if end <= self.bytes.len() {
                    Ok(end)
                } else {
                    Err(AmlError::Capacity)
                }
            });
        let end = self.record(end)?;
        let start = self.pos;
        self.pos = end;
        Ok(&mut self.bytes[start..end])
    }
}

impl AmlFragmentSink for AmlWriter<'_> {
    fn reject(&mut self, error: AmlError) -> Result<(), AmlError> {
        self.record(Err(error))
    }

    fn append_fragment<'a>(&'a mut self, bytes: &[u8]) -> Result<&'a mut [u8], AmlError> {
        let out = self.reserve(bytes.len())?;
        out.copy_from_slice(bytes);
        Ok(out)
    }
}

/// Allocation-backed convenience; includes the temporary package prefix.
pub fn scope_vec(path: &str, children: &[u8]) -> Result<Vec<u8>, AmlError> {
    AmlPath::new(path)?;
    let capacity = path
        .len()
        .checked_mul(4)
        .and_then(|n| n.checked_add(8))
        .and_then(|n| n.checked_add(children.len()))
        .ok_or(AmlError::LengthOverflow)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| AmlError::Capacity)?;
    bytes.resize(capacity, 0);
    let mut writer = AmlWriter::new(&mut bytes);
    writer.scope(path, |writer| writer.raw(children))?;
    let len = writer.position();
    bytes.truncate(len);
    Ok(bytes)
}

fn write_name_string(writer: &mut AmlWriter<'_>, path: AmlPath<'_>) -> Result<(), AmlError> {
    let mut rest = path.0;
    if let Some(stripped) = rest.strip_prefix('\\') {
        writer.byte(b'\\')?;
        rest = stripped;
    }
    while let Some(stripped) = rest.strip_prefix('^') {
        writer.byte(b'^')?;
        rest = stripped;
    }
    if rest.is_empty() {
        return writer.byte(0);
    }
    match rest.split('.').count() {
        1 => {}
        2 => writer.byte(0x2e)?,
        n => {
            writer.byte(0x2f)?;
            writer.byte(u8::try_from(n).map_err(|_| AmlError::InvalidPath)?)?;
        }
    }
    for segment in rest.split('.') {
        let mut name = [b'_'; 4];
        name[..segment.len()].copy_from_slice(segment.as_bytes());
        writer.raw(&name)?;
    }
    Ok(())
}

/// Encode a PkgLength including its own width (maximum 28-bit total).
pub fn package_length(content_len: usize) -> Result<([u8; 4], usize), AmlError> {
    let (width, total) = (1..=4)
        .find_map(|width| {
            let total = content_len.checked_add(width)?;
            let limit = if width == 1 {
                0x40
            } else {
                1usize << (4 + 8 * (width - 1))
            };
            (total < limit).then_some((width, total))
        })
        .ok_or(AmlError::LengthOverflow)?;
    if width == 1 {
        return Ok(([total as u8, 0, 0, 0], 1));
    }
    let mut bytes = [0; 4];
    bytes[0] = ((width as u8 - 1) << 6) | (total as u8 & 0x0f);
    for (index, byte) in bytes.iter_mut().enumerate().take(width).skip(1) {
        *byte = (total >> (4 + (index - 1) * 8)) as u8;
    }
    Ok((bytes, width))
}
