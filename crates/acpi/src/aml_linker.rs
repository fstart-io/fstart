//! Tiny allocation-free linker for precompiled AML fragments.
//!
//! The byte-emitting DSL fixes every package length inside a fragment at
//! compile time. This writer only has to link fragments together, computing
//! the `PkgLength` of runtime `Scope` wrappers and finalising an ACPI table.

use alloc::vec;
use alloc::vec::Vec;

use crate::{AmlFragment, AmlFragmentSink, BoundAmlFragment};

/// Sequential AML/table writer backed by caller-provided storage.
pub struct AmlWriter<'a> {
    bytes: &'a mut [u8],
    pos: usize,
}

impl<'a> AmlWriter<'a> {
    pub fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.pos]
    }

    pub fn emit<const N: usize, const K: usize>(
        &mut self,
        fragment: &AmlFragment<N, K>,
        operands: &[u64; K],
    ) {
        fragment.emit(self, operands);
    }

    pub fn emit_bound<const N: usize, const K: usize>(
        &mut self,
        fragment: &BoundAmlFragment<N, K>,
    ) {
        fragment.emit(self);
    }

    pub fn raw(&mut self, bytes: &[u8]) {
        self.reserve(bytes.len()).copy_from_slice(bytes);
    }

    /// Link child fragments below an AML namespace scope.
    pub fn scope(&mut self, path: &str, children: impl FnOnce(&mut Self)) {
        self.byte(0x10); // ScopeOp
        let package = self.pos;
        self.raw(&[0; 4]);
        write_name_string(self, path);
        children(self);

        let end = self.pos;
        let content_len = end - package - 4;
        let (encoded, encoded_len) = package_length(content_len);
        let unused = 4 - encoded_len;
        self.bytes
            .copy_within(package + 4..end, package + encoded_len);
        self.bytes[package..package + encoded_len].copy_from_slice(&encoded[..encoded_len]);
        self.pos -= unused;
    }

    /// Start an ACPI SDT, invoke `body`, and finalise length and checksum.
    pub fn table(
        bytes: &'a mut [u8],
        signature: [u8; 4],
        revision: u8,
        oem_id: [u8; 6],
        oem_table_id: [u8; 8],
        oem_revision: u32,
        body: impl FnOnce(&mut Self),
    ) -> Self {
        let mut writer = Self::new(bytes);
        writer.raw(&signature);
        writer.dword(0); // length
        writer.byte(revision);
        writer.byte(0); // checksum
        writer.raw(&oem_id);
        writer.raw(&oem_table_id);
        writer.dword(oem_revision);
        writer.raw(b"FST0");
        writer.dword(1);
        body(&mut writer);
        writer.bytes[4..8].copy_from_slice(&(writer.pos as u32).to_le_bytes());
        let checksum = writer
            .as_slice()
            .iter()
            .fold(0u8, |sum, byte| sum.wrapping_add(*byte));
        writer.bytes[9] = 0u8.wrapping_sub(checksum);
        writer
    }

    pub fn byte(&mut self, byte: u8) {
        self.reserve(1)[0] = byte;
    }

    pub fn word(&mut self, word: u16) {
        self.raw(&word.to_le_bytes());
    }

    pub fn dword(&mut self, dword: u32) {
        self.raw(&dword.to_le_bytes());
    }

    pub fn qword(&mut self, qword: u64) {
        self.raw(&qword.to_le_bytes());
    }

    fn reserve(&mut self, len: usize) -> &mut [u8] {
        let start = self.pos;
        let end = start.checked_add(len).expect("AML output length overflow");
        assert!(end <= self.bytes.len(), "AML output buffer overflow");
        self.pos = end;
        &mut self.bytes[start..end]
    }
}

impl AmlFragmentSink for AmlWriter<'_> {
    fn append_fragment<'a>(&'a mut self, bytes: &[u8]) -> &'a mut [u8] {
        let out = self.reserve(bytes.len());
        out.copy_from_slice(bytes);
        out
    }
}

/// Allocation-backed convenience for callers that still collect table parts.
/// The linking implementation itself remains allocation-free.
pub fn scope_vec(path: &str, children: &[u8]) -> Vec<u8> {
    let segments = path.split('.').count();
    let mut bytes = vec![0; children.len() + 8 + segments * 4];
    let len = {
        let mut writer = AmlWriter::new(&mut bytes);
        writer.scope(path, |writer| writer.raw(children));
        writer.position()
    };
    bytes.truncate(len);
    bytes
}

fn write_name_string(writer: &mut AmlWriter<'_>, path: &str) {
    let mut rest = path;
    if let Some(stripped) = rest.strip_prefix('\\') {
        writer.byte(b'\\');
        rest = stripped;
    }
    while let Some(stripped) = rest.strip_prefix('^') {
        writer.byte(b'^');
        rest = stripped;
    }
    let count = if rest.is_empty() {
        0
    } else {
        rest.split('.').count()
    };
    match count {
        0 => writer.byte(0),
        1 => {}
        2 => writer.byte(0x2e),
        _ => {
            writer.byte(0x2f);
            writer.byte(count as u8);
        }
    }
    for segment in rest.split('.').filter(|segment| !segment.is_empty()) {
        assert!(segment.len() <= 4, "AML NameSeg exceeds four bytes");
        let mut name = [b'_'; 4];
        name[..segment.len()].copy_from_slice(segment.as_bytes());
        writer.raw(&name);
    }
}

/// Encode an AML PkgLength whose value includes the encoded length itself.
pub fn package_length(content_len: usize) -> ([u8; 4], usize) {
    let width = if content_len + 1 < 0x40 {
        1
    } else if content_len + 2 < 0x1000 {
        2
    } else if content_len + 3 < 0x10_0000 {
        3
    } else {
        4
    };
    let total = content_len + width;
    if width == 1 {
        return ([total as u8, 0, 0, 0], 1);
    }
    let mut bytes = [0u8; 4];
    bytes[0] = ((width as u8 - 1) << 6) | (total as u8 & 0x0f);
    for index in 1..width {
        bytes[index] = (total >> (4 + (index - 1) * 8)) as u8;
    }
    (bytes, width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fstart_acpi_macros::acpi_dsl;

    static CHILD: AmlFragment<6, 0> = acpi_dsl! { Name("TEST", 1u32); };

    #[test]
    fn links_scope_and_finalises_dsdt() {
        let mut bytes = [0u8; 128];
        let table = AmlWriter::table(
            &mut bytes,
            *b"DSDT",
            2,
            *b"FSTART",
            *b"LINKTEST",
            1,
            |writer| writer.scope("\\_SB_.PCI0", |writer| writer.emit(&CHILD, &[])),
        );
        assert_eq!(&table.as_slice()[0..4], b"DSDT");
        assert_eq!(
            table
                .as_slice()
                .iter()
                .fold(0u8, |sum, b| sum.wrapping_add(*b)),
            0
        );
        assert_eq!(
            u32::from_le_bytes(table.as_slice()[4..8].try_into().unwrap()) as usize,
            table.position()
        );
    }
}
