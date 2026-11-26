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
#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct MbpHeader {
    dwords: u8,
    entries: u8,
    reserved: [u8; 2],
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct MbpItemHeader {
    app_id: u8,
    item_id: u8,
    length_dwords: u8,
    reserved: u8,
}

#[repr(C)]
#[derive(Clone, Copy, FromBytes, Immutable, KnownLayout, Unaligned)]
struct FwCapsPayload {
    rule_id: U32,
    rule_len: u8,
    capabilities: U32,
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
    let rest = response_header(Command::BiosPayload, bytes)?;
    if !bytes.len().is_multiple_of(4) {
        return Err(Error::InvalidBiosPayload);
    }
    let (mbp, body) = MbpHeader::ref_from_prefix(rest).map_err(|_| Error::InvalidBiosPayload)?;
    // `dwords` includes the MBP header itself, excluding the MKHI header.
    let body_len = usize::from(mbp.dwords)
        .checked_sub(1)
        .ok_or(Error::InvalidBiosPayload)?
        .checked_mul(4)
        .ok_or(Error::InvalidBiosPayload)?;
    let mut cursor = body.get(..body_len).ok_or(Error::InvalidBiosPayload)?;
    let mut capabilities = None;
    for _ in 0..mbp.entries {
        let (item, item_rest) =
            MbpItemHeader::ref_from_prefix(cursor).map_err(|_| Error::InvalidBiosPayload)?;
        let payload_len = (usize::from(item.length_dwords) * 4)
            .checked_sub(core::mem::size_of::<MbpItemHeader>())
            .ok_or(Error::InvalidBiosPayload)?;
        let (payload, next) = item_rest
            .split_at_checked(payload_len)
            .ok_or(Error::InvalidBiosPayload)?;
        let id = (u16::from(item.app_id) << 4) | u16::from(item.item_id);
        if id == 0x12 {
            if capabilities.is_some() {
                return Err(Error::InvalidBiosPayload);
            }
            let (caps, _) = U32::ref_from_prefix(payload).map_err(|_| Error::InvalidBiosPayload)?;
            capabilities = Some(caps.get());
        }
        cursor = next;
    }
    if !cursor.is_empty() {
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
                .send_receive(request.as_bytes(), response.0, 0, 7, delay)?;
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
        let mut response =
            [0u8; core::mem::size_of::<MkhiHeader>() + core::mem::size_of::<FwCapsPayload>()];
        let count = self
            .transport
            .send_receive(request.as_bytes(), &mut response, 0, 7, delay)?;
        if count != response.len() {
            return Err(Error::ResponseSize {
                expected: response.len(),
                actual: count,
            });
        }
        let rest = response_header(Command::FirmwareCapabilities, &response)?;
        let caps = FwCapsPayload::ref_from_bytes(rest).map_err(|_| Error::ResponseHeader)?;
        if caps.rule_id.get() != 0 || caps.rule_len != 4 {
            return Err(Error::ResponseHeader);
        }
        Ok(caps.capabilities.get())
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
        let request = Command::EndOfPost.header();
        let mut response = [0u8; 8];
        let count = self
            .transport
            .send_receive(request.as_bytes(), &mut response, 0, 7, delay)?;
        EndOfPostAction::parse(response.get(..count).ok_or(Error::ResponseHeader)?)
    }
}
#[cfg(test)]
#[path = "post_memory_tests.rs"]
mod tests;
