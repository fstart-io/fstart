//! Minimal LZ4 block decompressor — no_std, no alloc.
//!
//! This is a safe, minimal implementation of the LZ4 block format decompressor,
//! designed for firmware use. It supports in-place decompression (where the
//! compressed source overlaps the tail of the output buffer).
//!
//! The algorithm follows the LZ4 block specification:
//! <https://github.com/lz4/lz4/blob/dev/doc/lz4_Block_format.md>
//!
//! Based on coreboot's `lz4.c.inc` (BSD 2-Clause, Yann Collet).

/// Minimum match length in the LZ4 format.
const MIN_MATCH: usize = 4;

/// Number of trailing literal bytes that must end every block.
const LAST_LITERALS: usize = 5;

/// Wild copy length — copies happen in 8-byte chunks.
const WILD_COPY_LEN: usize = 8;

/// Minimum bytes remaining for the fast path (match + literal guard).
const MFLIMIT: usize = WILD_COPY_LEN + MIN_MATCH;

/// ML_BITS / ML_MASK / RUN_MASK for token parsing.
const ML_BITS: u8 = 4;
const ML_MASK: u8 = (1 << ML_BITS) - 1;
const RUN_MASK: u8 = (1 << (8 - ML_BITS)) - 1;

/// Error returned when decompression fails.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lz4Error {
    /// Input or output overrun.
    Overrun,
    /// Match offset points before the start of the output.
    BadOffset,
}

/// Decompress an LZ4 block from `src` into `dst`.
///
/// Returns the number of bytes written to `dst` on success, or an error.
/// The caller should verify the returned count matches the expected
/// decompressed size — the function does not enforce an exact fill.
///
/// ## In-place decompression
///
/// This function supports in-place decompression where `src` points into
/// the tail of a larger buffer that starts at `dst`. The caller must ensure
/// `src` starts at `dst_base + in_place_size - src.len()`. The decompressor
/// detects the overlap and guards against the output pointer overtaking the
/// input pointer.
///
/// # Safety
///
/// `src` and `dst` may overlap **only** if `src` is entirely within or
/// past the end of `dst` (i.e., in-place layout). Any other overlap is
/// undefined behavior.
pub fn decompress_block(src: &[u8], dst: &mut [u8]) -> Result<usize, Lz4Error> {
    let mut ip = 0usize; // index into src
    let mut op = 0usize; // index into dst
    let iend = src.len();
    let oend = dst.len();

    // Detect in-place decompression: src pointer is at or after dst start.
    // When in-place, the output write pointer must not overtake the input
    // read pointer (with a WILD_COPY_LEN margin for the fast copy path).
    let in_place = src.as_ptr() as usize >= dst.as_ptr() as usize;

    loop {
        // Read token
        if ip >= iend {
            return Err(Lz4Error::Overrun);
        }
        let token = src[ip];
        ip += 1;

        // --- Literal length ---
        let mut lit_len = (token >> ML_BITS) as usize;
        if lit_len == RUN_MASK as usize {
            loop {
                if ip >= iend.saturating_sub(RUN_MASK as usize) {
                    return Err(Lz4Error::Overrun);
                }
                let s = src[ip] as usize;
                ip += 1;
                lit_len += s;
                if s != 255 {
                    break;
                }
            }
        }

        // --- Copy literals ---
        let cpy = op + lit_len;
        // Check if this is the last literal block (end of compressed stream)
        if cpy > oend.saturating_sub(MFLIMIT)
            || ip + lit_len > iend.saturating_sub(2 + 1 + LAST_LITERALS)
        {
            // Final literals — must consume all remaining input
            if cpy > oend {
                return Err(Lz4Error::Overrun);
            }
            if ip + lit_len > iend {
                return Err(Lz4Error::Overrun);
            }
            // Use byte-by-byte copy for the final block (safe for overlap)
            copy_within(src, ip, dst, op, lit_len);
            op += lit_len;
            break; // End of block
        }

        // In-place guard: before the wild copy fast path, ensure the
        // output write position (with WILD_COPY_LEN margin) hasn't
        // caught up to the current input read position. Same check as
        // coreboot's lz4.c.inc line 146.
        if in_place {
            let src_abs = src.as_ptr() as usize + ip;
            let dst_abs = dst.as_ptr() as usize + op;
            if dst_abs + WILD_COPY_LEN > src_abs {
                return Err(Lz4Error::Overrun);
            }
        }

        // Fast path: copy literals (may overshoot by up to WILD_COPY_LEN-1)
        wild_copy(src, ip, dst, op, cpy);
        ip += lit_len;
        op = cpy;

        // --- Match offset (16-bit little-endian) ---
        if ip + 1 >= iend {
            return Err(Lz4Error::Overrun);
        }
        let offset = (src[ip] as usize) | ((src[ip + 1] as usize) << 8);
        ip += 2;

        if offset == 0 || offset > op {
            return Err(Lz4Error::BadOffset);
        }
        let match_pos = op - offset;

        // --- Match length ---
        let mut match_len = (token & ML_MASK) as usize;
        if match_len == ML_MASK as usize {
            loop {
                if ip > iend.saturating_sub(LAST_LITERALS) {
                    return Err(Lz4Error::Overrun);
                }
                let s = src[ip] as usize;
                ip += 1;
                match_len += s;
                if s != 255 {
                    break;
                }
            }
        }
        match_len += MIN_MATCH;

        let copy_end = op + match_len;
        if copy_end > oend {
            return Err(Lz4Error::Overrun);
        }

        // In-place guard: the match copy writes to dst[op..copy_end].
        // Ensure this doesn't overwrite unread compressed data in the
        // overlapping region.
        if in_place {
            let src_abs = src.as_ptr() as usize + ip;
            let dst_end_abs = dst.as_ptr() as usize + copy_end;
            if dst_end_abs > src_abs {
                return Err(Lz4Error::Overrun);
            }
        }

        // --- Copy match (from earlier in the output buffer) ---
        // Match copies can overlap (e.g., offset=1 means repeat last byte).
        if offset < 8 {
            // Short offset: byte-by-byte for the first 8 bytes
            copy_match_short(dst, op, match_pos, offset, match_len);
        } else if copy_end > oend.saturating_sub(WILD_COPY_LEN + LAST_LITERALS - 1) {
            // Near end of buffer: careful copy
            copy_match_careful(dst, op, match_pos, match_len);
        } else {
            // Fast path: 8-byte chunks from non-overlapping match
            wild_copy_within(dst, match_pos, op, copy_end);
        }
        op = copy_end;
    }

    Ok(op)
}

/// Copy `len` bytes from `src[si..]` to `dst[di..]`, byte-by-byte.
///
/// Uses byte-by-byte copy (not `copy_from_slice`) because `src` and `dst`
/// may alias overlapping memory in the in-place decompression case.
#[inline(always)]
#[allow(clippy::manual_memcpy)]
fn copy_within(src: &[u8], si: usize, dst: &mut [u8], di: usize, len: usize) {
    for i in 0..len {
        dst[di + i] = src[si + i];
    }
}

/// Wild copy: 8-byte chunks from src to dst until `dst_end`.
/// May overshoot by up to 7 bytes (caller must ensure space).
#[inline(always)]
fn wild_copy(src: &[u8], si: usize, dst: &mut [u8], di: usize, dst_end: usize) {
    let mut s = si;
    let mut d = di;
    while d < dst_end {
        let remaining_src = src.len() - s;
        let remaining_dst = dst.len() - d;
        let chunk = 8.min(remaining_src).min(remaining_dst);
        dst[d..d + chunk].copy_from_slice(&src[s..s + chunk]);
        s += 8;
        d += 8;
    }
}

/// Wild copy within the same buffer (for match copies with offset >= 8).
#[inline(always)]
fn wild_copy_within(buf: &mut [u8], src: usize, dst: usize, dst_end: usize) {
    let mut s = src;
    let mut d = dst;
    while d < dst_end {
        // Copy byte-by-byte to handle potential overlap correctly
        // (even with offset >= 8, the match may catch up during long copies)
        let chunk_end = (d + 8).min(dst_end);
        while d < chunk_end {
            buf[d] = buf[s];
            d += 1;
            s += 1;
        }
    }
}

/// Copy a match with a short offset (< 8).
/// Handles the overlapping repeat pattern (e.g., offset=1 fills with one byte).
#[inline(always)]
fn copy_match_short(buf: &mut [u8], op: usize, match_pos: usize, offset: usize, len: usize) {
    let mut s = match_pos;
    let mut d = op;
    let end = op + len;
    while d < end {
        buf[d] = buf[s];
        d += 1;
        s += 1;
        // Wrap source within the pattern
        if s >= match_pos + offset {
            s = match_pos;
        }
    }
}

/// Copy a match carefully (near end of buffer), byte-by-byte.
#[inline(always)]
fn copy_match_careful(buf: &mut [u8], op: usize, match_pos: usize, len: usize) {
    let mut s = match_pos;
    let mut d = op;
    let end = op + len;
    while d < end {
        buf[d] = buf[s];
        d += 1;
        s += 1;
    }
}
