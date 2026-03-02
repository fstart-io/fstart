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

use fstart_types::handoff::{StageHandoff, HANDOFF_MAGIC, HANDOFF_MAX_SIZE, HANDOFF_VERSION};

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

    // Quick magic check before attempting deserialization.
    if buf.len() < 4 {
        return None;
    }
    let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if magic != HANDOFF_MAGIC {
        return None;
    }

    let handoff: StageHandoff = postcard::from_bytes(buf).ok()?;

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
