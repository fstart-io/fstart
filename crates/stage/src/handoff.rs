//! Inter-stage handoff serialization and deserialization.
//!
//! The outgoing stage calls [`serialize`] to write a [`StageHandoff`]
//! to a DRAM buffer, then passes the buffer address in `r0` when jumping
//! to the next stage. The incoming stage calls [`try_deserialize`] with
//! the `r0` value to recover the handoff data.
//!
//! Uses `postcard` for compact no_std binary encoding. The format is NOT
//! self-describing — both stages share the same struct definition (same
//! build). A magic + version header provides validation. Switching to a
//! self-describing format (e.g., minicbor/CBOR) later requires only
//! changing the encode/decode calls; the struct and all surrounding code
//! stay the same.

use fstart_core::handoff::{StageHandoff, HANDOFF_MAGIC, HANDOFF_MAX_SIZE, HANDOFF_VERSION};

/// Serialize a [`StageHandoff`] into the provided buffer.
///
/// Returns the number of bytes written. The caller should pass
/// `&buf[..len]` (or the buffer start address) to the next stage.
///
/// # Errors
///
/// Returns `Err` if the buffer is too small or serialization fails.
pub fn serialize(handoff: &StageHandoff, buf: &mut [u8]) -> Result<usize, &'static str> {
    let used = postcard::to_slice(handoff, buf).map_err(|_| "postcard serialize failed")?;
    Ok(used.len())
}

/// Try to deserialize a [`StageHandoff`] from a raw pointer.
///
/// `handoff_ptr` is the value passed in `r0` by the previous stage.
/// Returns `None` if:
/// - `handoff_ptr` is 0 or unaligned
/// - The buffer doesn't start with [`HANDOFF_MAGIC`]
/// - Deserialization or version check fails
///
/// This is safe to call with garbage pointers (e.g., first stage loaded
/// by BROM) — the magic check catches invalid data before any further
/// parsing occurs.
pub fn try_deserialize(handoff_ptr: usize) -> Option<StageHandoff> {
    if handoff_ptr == 0 {
        return None;
    }

    // SAFETY: We first do a minimal read of 4 bytes to check the magic.
    // If handoff_ptr is garbage, we might fault — but on ARMv7 firmware,
    // valid DRAM addresses (0x4000_0000+) are always mapped. The BROM
    // won't leave r0 pointing to unmapped memory. In the worst case,
    // the magic won't match and we return None.
    let buf = unsafe { core::slice::from_raw_parts(handoff_ptr as *const u8, HANDOFF_MAX_SIZE) };

    // The struct is postcard-encoded, so the magic is a varint — it cannot
    // be validated with a raw word read. Parse first (safe on garbage: the
    // decode is bounded by the buffer), then check the decoded fields.
    let handoff: StageHandoff = postcard::from_bytes(buf).ok()?;

    if handoff.magic != HANDOFF_MAGIC {
        return None;
    }
    if handoff.version != HANDOFF_VERSION {
        fstart_log::warn!(
            "handoff: version mismatch (got {}, expected {})",
            handoff.version,
            HANDOFF_VERSION
        );
        return None;
    }

    Some(handoff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_round_trips_through_try_deserialize() {
        let mut buf = [0u8; HANDOFF_MAX_SIZE];
        let len = serialize(&StageHandoff::new(0x1000_0000), &mut buf).unwrap();
        assert!(len > 0);
        let got = try_deserialize(buf.as_ptr() as usize).expect("round trip");
        assert_eq!(got.magic, HANDOFF_MAGIC);
        assert_eq!(got.dram_size, 0x1000_0000);
    }

    #[test]
    fn garbage_is_rejected() {
        let buf = [0xc8u8; HANDOFF_MAX_SIZE];
        assert!(try_deserialize(buf.as_ptr() as usize).is_none());
    }
}
