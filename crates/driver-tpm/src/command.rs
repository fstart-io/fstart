// SPDX-License-Identifier: MIT OR Apache-2.0
//! Big-endian command marshalling, shared by both command sets.

use crate::{BUFFER_SIZE, Error, zeroize};

/// A command under construction: tag, size (patched by [`Command::finish`])
/// and command code or ordinal, then parameters.
pub struct Command {
    buf: [u8; BUFFER_SIZE],
    len: usize,
    overflow: bool,
}

impl Command {
    /// Start a command with `tag` and `code`.
    pub fn new(tag: u16, code: u32) -> Self {
        let mut command = Self {
            buf: [0; BUFFER_SIZE],
            len: 0,
            overflow: false,
        };
        command.u16(tag).u32(0).u32(code);
        command
    }

    pub fn bytes(&mut self, bytes: &[u8]) -> &mut Self {
        match self.buf.get_mut(self.len..self.len + bytes.len()) {
            Some(dest) => {
                dest.copy_from_slice(bytes);
                self.len += bytes.len();
            }
            None => self.overflow = true,
        }
        self
    }

    pub fn u8(&mut self, value: u8) -> &mut Self {
        self.bytes(&[value])
    }

    pub fn u16(&mut self, value: u16) -> &mut Self {
        self.bytes(&value.to_be_bytes())
    }

    pub fn u32(&mut self, value: u32) -> &mut Self {
        self.bytes(&value.to_be_bytes())
    }

    /// A `TPM2B`: 16-bit size, then the bytes.
    pub fn sized(&mut self, bytes: &[u8]) -> &mut Self {
        self.u16(bytes.len() as u16).bytes(bytes)
    }

    /// Patch the size field and return the command bytes.
    pub fn finish(&mut self) -> Result<&[u8], Error> {
        if self.overflow {
            return Err(Error::Response);
        }
        let size = (self.len as u32).to_be_bytes();
        self.buf[2..6].copy_from_slice(&size);
        Ok(&self.buf[..self.len])
    }
}

/// Commands may carry an authorization value: none outlives the command.
impl Drop for Command {
    fn drop(&mut self) {
        zeroize(&mut self.buf[..self.len]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_size_is_patched() {
        let mut command = Command::new(0x8001, 0x144);
        command.u16(0);
        assert_eq!(
            command.finish().unwrap(),
            &[0x80, 0x01, 0, 0, 0, 12, 0, 0, 1, 0x44, 0, 0]
        );
    }
}
