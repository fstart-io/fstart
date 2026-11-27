// SPDX-License-Identifier: MIT OR Apache-2.0
//! Big-endian command marshalling, shared by both command sets.

use crate::{BUFFER_SIZE, Error};
use zerocopy::big_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};
use zeroize::Zeroize;

/// Common big-endian TPM 1.2 / 2.0 command header (10 bytes).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct TpmCommandHeader {
    pub tag: U16,
    pub size: U32,
    pub code: U32,
}

/// Common big-endian TPM 1.2 / 2.0 response header (10 bytes).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, FromBytes, IntoBytes, Immutable, KnownLayout, Unaligned,
)]
#[repr(C)]
pub struct TpmResponseHeader {
    pub tag: U16,
    pub size: U32,
    pub code: U32,
}

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
        command.write(&TpmCommandHeader {
            tag: U16::new(tag),
            size: U32::new(0),
            code: U32::new(code),
        });
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

    pub fn write<T: IntoBytes + Immutable>(&mut self, value: &T) -> &mut Self {
        self.bytes(value.as_bytes())
    }

    pub fn u8(&mut self, value: u8) -> &mut Self {
        self.bytes(&[value])
    }

    pub fn u16(&mut self, value: u16) -> &mut Self {
        self.write(&U16::new(value))
    }

    pub fn u32(&mut self, value: u32) -> &mut Self {
        self.write(&U32::new(value))
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
        let (header, _) = TpmCommandHeader::mut_from_prefix(&mut self.buf[..self.len])
            .map_err(|_| Error::Response)?;
        header.size = U32::new(self.len as u32);
        Ok(&self.buf[..self.len])
    }
}

/// Commands may carry an authorization value: none outlives the command.
impl Drop for Command {
    fn drop(&mut self) {
        self.buf[..self.len].zeroize();
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
