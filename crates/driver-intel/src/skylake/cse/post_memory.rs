// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native me.c MBP/FW-capabilities and common CSE End-of-Post protocol.
//! The actual cold-stage owner sequences these after DID and owns the scratch.
use super::*;

pub const MBP_BUFFER_BYTES: usize = 8 + 0x1000;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BiosPayload {
    pub firmware_capabilities: Option<u32>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndOfPostAction {
    Continue,
    GlobalReset,
}

// MBP contains sensitive items. Erase even on transport/parse errors; ordinary
// dead-store-eliminable memset is not sufficient for this caller-owned buffer.
struct PayloadBuffer<'a>(&'a mut [u8]);
impl Drop for PayloadBuffer<'_> {
    fn drop(&mut self) {
        for byte in self.0.iter_mut() {
            // A volatile store the optimizer cannot drop, on plain memory.
            // SAFETY: exclusively borrowed initialized output byte, no aliases.
            #[allow(clippy::disallowed_methods)]
            unsafe {
                core::ptr::write_volatile(byte, 0)
            };
        }
        core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    }
}
impl EndOfPostAction {
    fn parse(response: &[u8]) -> Result<Self, Error> {
        let data = parse_response(Command::EndOfPost, response)?;
        match u32::from_le_bytes(data) {
            0 => Ok(Self::Continue),
            1 => Ok(Self::GlobalReset),
            action => Err(Error::InvalidEopAction(action)),
        }
    }
}
fn bios_payload(bytes: &[u8]) -> Result<BiosPayload, Error> {
    response_header(Command::BiosPayload, bytes)?;
    if bytes.len() < 8 || !bytes.len().is_multiple_of(4) {
        return Err(Error::InvalidBiosPayload);
    }
    let info = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let dwords = (info & 255) as usize; // includes MBP header, excludes MKHI
    let entries = ((info >> 8) & 255) as usize;
    let limit = 4 + 4 * dwords;
    if dwords == 0 || limit > bytes.len() {
        return Err(Error::InvalidBiosPayload);
    }
    let mut pos = 8;
    let mut capabilities = None;
    for _ in 0..entries {
        if pos + 4 > limit {
            return Err(Error::InvalidBiosPayload);
        }
        let item = u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap());
        let id = ((item & 255) << 4) | ((item >> 8) & 255);
        let length = ((item >> 16) & 255) as usize * 4;
        if length == 0 || pos + length > limit {
            return Err(Error::InvalidBiosPayload);
        }
        if id == 0x12 {
            if length < 8 || capabilities.is_some() {
                return Err(Error::InvalidBiosPayload);
            }
            capabilities = Some(u32::from_le_bytes(
                bytes[pos + 4..pos + 8].try_into().unwrap(),
            ));
        }
        pos += length;
    }
    if pos != limit {
        return Err(Error::InvalidBiosPayload);
    }
    Ok(BiosPayload {
        firmware_capabilities: capabilities,
    })
}

impl<H, M, W, R> Cse<H, M, W, R>
where
    H: Readable<T = u32, R = Csr::Register> + Writeable<T = u32, R = Csr::Register>,
    M: Readable<T = u32, R = Csr::Register>,
    W: Writeable<T = u32, R = Fifo::Register>,
    R: Readable<T = u32, R = Fifo::Register>,
{
    /// Read the actual bring-up payload, erase it, then signal completion by
    /// resetting the real HECI interface. Only the small capabilities summary
    /// survives. No injected payload, presumed success, or post-error replay.
    /// Caller supplies exclusive DRAM scratch, AFTER authenticated cold DID.
    pub fn read_bios_payload(
        &mut self,
        scratch: &mut [u8],
        delay: &mut impl DelayNs,
    ) -> Result<BiosPayload, Error> {
        if scratch.len() < MBP_BUFFER_BYTES {
            return Err(Error::InvalidBiosPayload);
        }
        if !self.validate_binding()?.allows(Command::BiosPayload) {
            return Err(Error::Unsupported);
        }
        let request = Command::BiosPayload.request(0);
        let parsed = {
            let response = PayloadBuffer(&mut scratch[..MBP_BUFFER_BYTES]);
            response.0.fill(0);
            let count = self
                .transport
                .send_receive(&request, response.0, 0, 7, delay)?;
            bios_payload(response.0.get(..count).ok_or(Error::InvalidBiosPayload)?)
        }; // volatile erase before the ME buffer is released by HECI reset
        self.reset_interface(delay)?;
        let mut payload = parsed?;
        if payload.firmware_capabilities.is_none() {
            payload.firmware_capabilities = Some(self.firmware_capabilities(delay)?);
        }
        Ok(payload)
    }
    fn firmware_capabilities(&mut self, delay: &mut impl DelayNs) -> Result<u32, Error> {
        let status = self.validate_binding()?;
        let hfs = LocalRegisterCopy::<u32, Hfs1::Register>::new(status.hfs1);
        if hfs.read(Hfs1::MODE) != 0 || !status.allows(Command::FirmwareCapabilities) {
            return Err(Error::Unsupported);
        }
        let request = Command::FirmwareCapabilities.request(0); // FW_CAPS_RULE_ID=0
        let mut response = [0u8; 13];
        let count = self
            .transport
            .send_receive(&request, &mut response, 0, 7, delay)?;
        if count != response.len() {
            return Err(Error::ResponseSize {
                expected: response.len(),
                actual: count,
            });
        }
        response_header(Command::FirmwareCapabilities, &response)?;
        if response[4..8] != [0; 4] || response[8] != 4 {
            return Err(Error::ResponseHeader);
        }
        Ok(u32::from_le_bytes(response[9..13].try_into().unwrap()))
    }
    /// Actual synchronous EOP. Continue is returned ONLY for a valid response
    /// and action 0. A global-reset action remains distinct; callers must stop,
    /// not continue or retry destructive hardware initialization. No automatic
    /// interface reset/retry can turn a missing ACK into authorization.
    pub fn end_of_post(&mut self, delay: &mut impl DelayNs) -> Result<EndOfPostAction, Error> {
        let status = self.validate_binding()?;
        if !status.allows(Command::EndOfPost)
            || LocalRegisterCopy::<u32, Hfs1::Register>::new(status.hfs1).read(Hfs1::MODE) == 3
        {
            return Err(Error::Unsupported);
        }
        let request = Command::EndOfPost.request(0);
        let mut response = [0u8; 8];
        let count = self
            .transport
            .send_receive(&request[..4], &mut response, 0, 7, delay)?;
        EndOfPostAction::parse(response.get(..count).ok_or(Error::ResponseHeader)?)
    }
}
#[cfg(test)]
#[path = "post_memory_tests.rs"]
mod tests;
