//! Stage-local access to the linker-embedded build-layout descriptor.
//!
//! Only migrated stage linkers define these symbols. A caller using this API
//! must link the descriptor emission from fbuild; a missing descriptor is a
//! link error, never a fallback to board constants. `cargo check` does not need
//! the symbols to resolve, keeping this API visible to editor analysis.

use fstart_core::layout::{Error, HEADER_LEN, Layout, MAX_ENCODED_LEN};

/// Borrow and validate this stage's read-only layout bytes without allocating.
///
/// The linker contract places the start/end symbols around one initialized,
/// allocated region that remains mapped and immutable for the stage's lifetime.
/// This does not authenticate the descriptor or validate discovered RAM.
pub fn current() -> Result<Layout<'static>, Error> {
    unsafe extern "C" {
        static _fstart_layout_start: u8;
        static _fstart_layout_end: u8;
    }

    let start = &raw const _fstart_layout_start;
    let end = &raw const _fstart_layout_end;
    let len = descriptor_len(start.addr(), end.addr())?;
    // SAFETY: the stage linker defines these bounds around one loaded,
    // immutable descriptor with stage lifetime, as documented above. Check
    // non-null start, subtraction and the wire bound before forming a slice. No native struct
    // cast or alignment beyond bytes is involved.
    let bytes = unsafe { core::slice::from_raw_parts(start, len) };
    Layout::parse(bytes)
}

fn descriptor_len(start: usize, end: usize) -> Result<usize, Error> {
    let len = end.checked_sub(start).ok_or(Error::InvalidLength)?;
    if start == 0 || !(HEADER_LEN..=MAX_ENCODED_LEN).contains(&len) {
        return Err(Error::InvalidLength);
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_linker_bounds_before_forming_a_slice() {
        assert_eq!(descriptor_len(0, HEADER_LEN), Err(Error::InvalidLength));
        assert_eq!(descriptor_len(1, 0), Err(Error::InvalidLength));
        assert_eq!(descriptor_len(1, 1), Err(Error::InvalidLength));
        assert_eq!(descriptor_len(1, HEADER_LEN), Err(Error::InvalidLength));
        assert_eq!(
            descriptor_len(0, MAX_ENCODED_LEN + 1),
            Err(Error::InvalidLength)
        );
        assert_eq!(descriptor_len(usize::MAX, 1), Err(Error::InvalidLength));
        assert_eq!(descriptor_len(1, 1 + MAX_ENCODED_LEN), Ok(MAX_ENCODED_LEN));
    }
}
