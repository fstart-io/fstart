//! Bounded version-2 predecessor handoff. A fixed magic/version/length header
//! precedes the compact postcard body; there is no legacy decoder.
use fstart_core::handoff::{HANDOFF_MAGIC, HANDOFF_MAX_SIZE, HANDOFF_VERSION, StageHandoff};

const HEADER_SIZE: usize = 8;

pub fn serialize(handoff: &StageHandoff, buf: &mut [u8]) -> Result<usize, &'static str> {
    if !handoff.is_valid() || buf.len() < HEADER_SIZE {
        return Err("invalid handoff/header capacity");
    }
    let limit = buf.len().min(HANDOFF_MAX_SIZE);
    let size = postcard::to_slice(handoff, &mut buf[HEADER_SIZE..limit])
        .map_err(|_| "postcard serialize failed")?
        .len();
    buf[..4].copy_from_slice(&HANDOFF_MAGIC.to_le_bytes());
    buf[4..6].copy_from_slice(&HANDOFF_VERSION.to_le_bytes());
    buf[6..8].copy_from_slice(&(size as u16).to_le_bytes());
    Ok(HEADER_SIZE + size)
}

/// Decode only the declared body, rejecting unknown versions, invalid lengths
/// and trailing body bytes. A valid header is not proof of trusted provenance.
pub fn deserialize(buf: &[u8]) -> Option<StageHandoff> {
    let header = buf.get(..HEADER_SIZE)?;
    if u32::from_le_bytes(header[..4].try_into().ok()?) != HANDOFF_MAGIC
        || u16::from_le_bytes(header[4..6].try_into().ok()?) != HANDOFF_VERSION
    {
        return None;
    }
    let length = usize::from(u16::from_le_bytes(header[6..8].try_into().ok()?));
    if length == 0 || length > HANDOFF_MAX_SIZE - HEADER_SIZE {
        return None;
    }
    let body = buf.get(HEADER_SIZE..HEADER_SIZE + length)?;
    let (handoff, rest): (StageHandoff, _) = postcard::take_from_bytes(body).ok()?;
    (rest.is_empty() && handoff.is_valid()).then_some(handoff)
}

/// # Safety
/// The platform must validate the configured handoff address and guarantee a
/// readable HANDOFF_MAX_SIZE-byte reserved buffer. Arbitrary pointers are NOT
/// made safe by a magic check. Keep the predecessor buffer stable during copy.
pub unsafe fn try_deserialize(handoff_ptr: usize) -> Option<StageHandoff> {
    if handoff_ptr == 0 {
        return None;
    }
    // SAFETY: the platform guarantees this reserved readable range.
    let buf = unsafe { core::slice::from_raw_parts(handoff_ptr as *const u8, HANDOFF_MAX_SIZE) };
    deserialize(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_length_and_exact_body_are_checked() {
        let mut bytes = [0; HANDOFF_MAX_SIZE];
        let mut sent = StageHandoff::new(0x1000_0000);
        sent.media = Some(fstart_core::ffs::locator::MediaLocator {
            image_offset: 0,
            image_size: 0x10000,
            root_offset: 0xfe00,
            root_size: 512,
            microcode_offset: 0,
            microcode_size: 0,
        });
        let length = serialize(&sent, &mut bytes).unwrap();
        let received = deserialize(&bytes[..length]).unwrap();
        assert_eq!(received.dram_size, sent.dram_size);
        assert_eq!(received.media, sent.media);
        for cut in 0..length {
            assert!(deserialize(&bytes[..cut]).is_none());
        }
        let mut bad = bytes;
        bad[4] = 1;
        assert!(deserialize(&bad).is_none());
        let mut bad = bytes;
        bad[6..8].copy_from_slice(&255u16.to_le_bytes());
        assert!(deserialize(&bad).is_none());
        let mut bad = bytes;
        bad[6..8].copy_from_slice(&((length - HEADER_SIZE + 1) as u16).to_le_bytes());
        assert!(deserialize(&bad).is_none());
    }
}
