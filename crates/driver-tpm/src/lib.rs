// SPDX-License-Identifier: MIT OR Apache-2.0
//! TPM interfaces and the commands platform firmware sends.
//!
//! - [`tis`]: the FIFO interface of the TCG PC Client TIS 1.3 (TPM 1.2) and
//!   PTP (TPM 2.0), memory mapped at `FED4_0000h`.
//! - [`tpm12`], [`tpm20`]: the subset of the TPM 1.2 Main and TPM 2.0
//!   Library command sets that firmware needs for measured boot (startup,
//!   self test, PCR extend, physical presence and the platform hierarchy).
//!
//! Commands go through a [`Transport`], so the command sets do not depend
//! on how the TPM is attached.
#![no_std]

mod command;
pub mod tis;
pub mod tpm12;
pub mod tpm20;

pub use command::Command;

/// TPM failures as the firmware sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The interface did not reach the expected state in time.
    Timeout,
    /// The locality could not be claimed.
    Locality,
    /// The interface broke the transfer protocol (unexpected `Expect` or
    /// `dataAvail`).
    Protocol,
    /// The response is malformed or longer than the buffer.
    Response,
    /// The TPM returned this nonzero response code.
    Code(u32),
}

/// Byte transport of whole commands and responses.
pub trait Transport {
    /// Send `command` and read the response into `response`; returns the
    /// response length.
    fn transmit(&mut self, command: &[u8], response: &mut [u8]) -> Result<usize, Error>;

    /// Wait `ms` milliseconds before a command is retried.
    fn wait_ms(&mut self, ms: u32);

    /// Give the TPM up until the next command (relinquish the locality,
    /// go idle); the next `transmit` claims it again.
    fn release(&mut self) {}
}

/// TPM family, as the interface reports it or `TPM2_Startup` proves it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Tpm12,
    Tpm20,
}

/// TPM 2.0 hash algorithm identifiers (`TPM_ALG_ID`), also used for the
/// SHA-1 log of TPM 1.2.
pub mod alg {
    pub const SHA1: u16 = 0x0004;
    pub const SHA256: u16 = 0x000B;
    pub const SHA384: u16 = 0x000C;
    pub const SHA512: u16 = 0x000D;

    /// Digest size of a supported hash algorithm.
    pub const fn digest_size(alg: u16) -> Option<usize> {
        match alg {
            SHA1 => Some(20),
            SHA256 => Some(32),
            SHA384 => Some(48),
            SHA512 => Some(64),
            _ => None,
        }
    }
}

/// Overwrite `bytes` with zeros in a way the compiler keeps: buffers that
/// held an authorization value or TPM randomness are scrubbed with it.
pub fn zeroize(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        // SAFETY: `byte` is a valid, exclusive reference.
        unsafe { core::ptr::write_volatile(byte, 0) };
    }
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
}

/// Largest command or response this crate builds or accepts.
pub const BUFFER_SIZE: usize = 1024;

/// Response header: tag, size and response code (both families).
pub(crate) const HEADER_SIZE: usize = 10;

/// Check the header of `response` and return the parameter area.
pub(crate) fn response_body(response: &[u8]) -> Result<&[u8], Error> {
    let header = response.get(..HEADER_SIZE).ok_or(Error::Response)?;
    let size = u32::from_be_bytes([header[2], header[3], header[4], header[5]]) as usize;
    let code = u32::from_be_bytes([header[6], header[7], header[8], header[9]]);
    if size != response.len() {
        return Err(Error::Response);
    }
    if code != 0 {
        return Err(Error::Code(code));
    }
    Ok(&response[HEADER_SIZE..])
}

/// Codes that ask to send the command again later: TPM 2.0
/// `TPM_RC_YIELDED`, `TPM_RC_TESTING` (a self test runs, PFP §8.3) and
/// `TPM_RC_RETRY`; TPM 1.2 `TPM_RETRY` and `TPM_DOING_SELFTEST`.
const RETRY_CODES: [u32; 5] = [0x908, 0x90A, 0x922, 0x800, 0x802];
/// Retries of one command, 10 ms apart: longer than any self test.
pub(crate) const RETRIES: u32 = 500;

/// Send `command` built in a scratch buffer and return the checked body,
/// retrying while the TPM is busy.
pub(crate) fn execute<'a, T: Transport + ?Sized>(
    tpm: &mut T,
    command: &[u8],
    response: &'a mut [u8; BUFFER_SIZE],
) -> Result<&'a [u8], Error> {
    let mut len = tpm.transmit(command, response)?;
    for _ in 0..RETRIES {
        match response_body(&response[..len]) {
            Err(Error::Code(code)) if RETRY_CODES.contains(&code) => {
                tpm.wait_ms(10);
                len = tpm.transmit(command, response)?;
            }
            _ => break,
        }
    }
    response_body(&response[..len])
}

/// Big-endian reader over a response body.
pub(crate) struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }
    pub(crate) fn bytes(&mut self, len: usize) -> Result<&'a [u8], Error> {
        if self.0.len() < len {
            return Err(Error::Response);
        }
        let (head, tail) = self.0.split_at(len);
        self.0 = tail;
        Ok(head)
    }
    pub(crate) fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.bytes(1)?[0])
    }
    pub(crate) fn u16(&mut self) -> Result<u16, Error> {
        let b = self.bytes(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }
    pub(crate) fn u32(&mut self) -> Result<u32, Error> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }
}
