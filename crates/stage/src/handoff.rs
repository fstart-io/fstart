//! Reading the predecessor's [`StageHandoff`] from its reserved buffer.
use fstart_core::handoff::{HANDOFF_MAX_SIZE, StageHandoff};

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
    StageHandoff::read_from(buf)
}
