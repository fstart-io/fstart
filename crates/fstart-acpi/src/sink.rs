//! Fixed-size buffer [`AmlSink`] implementation.
//!
//! Provides [`FixedBufSink`], an `AmlSink` backed by a `&mut [u8]` slice
//! with an internal write position.  Useful for writing AML to a
//! pre-allocated buffer in firmware without heap allocation on the
//! output path.
//!
//! Note: builder types in `acpi_tables` still use `alloc` internally
//! for PkgLength computation (they buffer children into temp `Vec<u8>`s).
//! `FixedBufSink` eliminates the *output* allocation -- the final
//! serialized bytes go directly into the target buffer.

use acpi_tables::AmlSink;

/// AML sink backed by a fixed-size byte buffer.
///
/// Writes AML bytes sequentially into a `&mut [u8]` slice.
/// Panics if the buffer overflows -- callers must pre-calculate
/// or over-provision the buffer size.
///
/// # Example
///
/// ```ignore
/// use fstart_acpi::sink::FixedBufSink;
/// use fstart_acpi::Aml;
///
/// let mut buf = [0u8; 4096];
/// let mut sink = FixedBufSink::new(&mut buf);
/// some_aml_object.to_aml_bytes(&mut sink);
/// let written = sink.position();
/// ```
pub struct FixedBufSink<'a> {
    buf: &'a mut [u8],
    pos: usize,
}

impl<'a> FixedBufSink<'a> {
    /// Create a new sink writing to the given buffer at position 0.
    pub fn new(buf: &'a mut [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Current write position (number of bytes written so far).
    pub fn position(&self) -> usize {
        self.pos
    }

    /// Remaining capacity in bytes.
    pub fn remaining(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// View the bytes written so far.
    pub fn as_slice(&self) -> &[u8] {
        &self.buf[..self.pos]
    }
}

impl AmlSink for FixedBufSink<'_> {
    fn byte(&mut self, byte: u8) {
        assert!(
            self.pos < self.buf.len(),
            "FixedBufSink overflow: wrote {} bytes into {}-byte buffer",
            self.pos + 1,
            self.buf.len(),
        );
        self.buf[self.pos] = byte;
        self.pos += 1;
    }

    fn word(&mut self, word: u16) {
        let bytes = word.to_le_bytes();
        self.vec(&bytes);
    }

    fn dword(&mut self, dword: u32) {
        let bytes = dword.to_le_bytes();
        self.vec(&bytes);
    }

    fn qword(&mut self, qword: u64) {
        let bytes = qword.to_le_bytes();
        self.vec(&bytes);
    }

    fn vec(&mut self, v: &[u8]) {
        let end = self.pos + v.len();
        assert!(
            end <= self.buf.len(),
            "FixedBufSink overflow: need {} bytes, buffer has {} remaining",
            v.len(),
            self.remaining(),
        );
        self.buf[self.pos..end].copy_from_slice(v);
        self.pos = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acpi_tables::Aml;

    #[test]
    fn test_fixed_buf_sink_basic() {
        let mut buf = [0u8; 16];
        let mut sink = FixedBufSink::new(&mut buf);

        assert_eq!(sink.position(), 0);
        assert_eq!(sink.remaining(), 16);

        sink.byte(0x42);
        assert_eq!(sink.position(), 1);
        assert_eq!(sink.as_slice(), &[0x42]);

        sink.word(0x1234);
        assert_eq!(sink.position(), 3);
        assert_eq!(sink.as_slice(), &[0x42, 0x34, 0x12]);

        sink.dword(0xDEAD_BEEF);
        assert_eq!(sink.position(), 7);
        assert_eq!(sink.as_slice(), &[0x42, 0x34, 0x12, 0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn test_fixed_buf_sink_vec() {
        let mut buf = [0u8; 8];
        let mut sink = FixedBufSink::new(&mut buf);

        sink.vec(&[1, 2, 3, 4]);
        assert_eq!(sink.position(), 4);
        assert_eq!(sink.remaining(), 4);
        assert_eq!(sink.as_slice(), &[1, 2, 3, 4]);
    }

    #[test]
    fn test_fixed_buf_sink_aml_object() {
        // Serialize a simple AML value (u32 = 0x1000) into the fixed buffer.
        let mut buf = [0u8; 16];
        let mut sink = FixedBufSink::new(&mut buf);

        let val = 0x1000u32;
        val.to_aml_bytes(&mut sink);

        // u32 serializes as: BytePrefix(0x0C) + 4 LE bytes
        assert!(sink.position() > 0);
    }

    #[test]
    #[should_panic(expected = "FixedBufSink overflow")]
    fn test_fixed_buf_sink_overflow_byte() {
        let mut buf = [0u8; 2];
        let mut sink = FixedBufSink::new(&mut buf);
        sink.byte(1);
        sink.byte(2);
        sink.byte(3); // overflow
    }

    #[test]
    #[should_panic(expected = "FixedBufSink overflow")]
    fn test_fixed_buf_sink_overflow_vec() {
        let mut buf = [0u8; 2];
        let mut sink = FixedBufSink::new(&mut buf);
        sink.vec(&[1, 2, 3]); // overflow
    }
}
