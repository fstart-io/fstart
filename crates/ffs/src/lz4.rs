//! Minimal LZ4 block decompressor — no_std, no alloc.
//!
//! This is a safe, minimal implementation of the LZ4 block format decompressor,
//! designed for firmware use. Source and destination slices must be disjoint.
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
/// Source and destination must not overlap, as required by Rust reference
/// aliasing rules. A shared allocation may be split with `split_at_mut`.
/// The complete input must be consumed; unexplained trailing bytes are rejected.
pub fn decompress_block(src: &[u8], dst: &mut [u8]) -> Result<usize, Lz4Error> {
    decode_sequences(src, dst, decode_sequence)
}

/// Drive `step` from the start of both buffers until the final sequence.
/// Shared with the reference decoder, so per-sequence equivalence carries
/// over to whole blocks by induction.
#[inline(always)]
fn decode_sequences(
    src: &[u8],
    dst: &mut [u8],
    step: impl Fn(&[u8], &mut [u8], usize, usize) -> Result<Sequence, Lz4Error>,
) -> Result<usize, Lz4Error> {
    let mut ip = 0usize; // index into src
    let mut op = 0usize; // index into dst
    loop {
        match step(src, dst, ip, op)? {
            Sequence::Next { ip: i, op: o } => (ip, op) = (i, o),
            Sequence::Last { op } => return Ok(op),
        }
    }
}

/// Outcome of decoding one sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sequence {
    /// Literals and a match were decoded; continue at these positions.
    Next { ip: usize, op: usize },
    /// The final literal-only sequence consumed the whole input.
    Last { op: usize },
}

/// Decode the sequence whose token is at `src[ip]`, writing at `dst[op]`.
///
/// Reads only `dst[..op]` and leaves `dst[..new op]` exact; bytes past the new
/// `op` may hold wild-copy overshoot. Requires `ip <= src.len()` and
/// `op <= dst.len()`, which `Next` preserves while advancing both.
#[inline(always)]
fn decode_sequence(
    src: &[u8],
    dst: &mut [u8],
    mut ip: usize,
    op: usize,
) -> Result<Sequence, Lz4Error> {
    let iend = src.len();
    let oend = dst.len();

    // Read token
    if ip >= iend {
        return Err(Lz4Error::Overrun);
    }
    let token = src[ip];
    ip += 1;

    // --- Literal length ---
    let mut lit_len = (token >> ML_BITS) as usize;
    if lit_len == RUN_MASK as usize {
        (lit_len, ip) =
            read_length_extension(src, ip, iend.saturating_sub(RUN_MASK as usize), lit_len)?;
    }

    // --- Copy literals ---
    let cpy = op.checked_add(lit_len).ok_or(Lz4Error::Overrun)?;
    let lit_end = ip.checked_add(lit_len).ok_or(Lz4Error::Overrun)?;
    // Check if this is the last literal block (end of compressed stream)
    if cpy > oend.saturating_sub(MFLIMIT) || lit_end > iend.saturating_sub(2 + 1 + LAST_LITERALS) {
        // Final literals — must consume all remaining input
        if cpy > oend || lit_end != iend {
            return Err(Lz4Error::Overrun);
        }
        // Copy final literals after exact input-length validation
        copy_within(src, ip, dst, op, lit_len);
        return Ok(Sequence::Last { op: cpy });
    }

    // Fast path: copy literals (may overshoot by up to WILD_COPY_LEN-1)
    wild_copy(src, ip, dst, op, cpy);
    ip = lit_end;
    let op = cpy;

    // --- Match offset (16-bit little-endian) ---
    if iend - ip < 2 {
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
        // `ip > iend - LAST_LITERALS` stops the loop; the sum cannot wrap.
        let limit = iend.saturating_sub(LAST_LITERALS) + 1;
        (match_len, ip) = read_length_extension(src, ip, limit, match_len)?;
    }
    match_len = match_len.checked_add(MIN_MATCH).ok_or(Lz4Error::Overrun)?;

    let copy_end = op.checked_add(match_len).ok_or(Lz4Error::Overrun)?;
    if copy_end > oend {
        return Err(Lz4Error::Overrun);
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
    Ok(Sequence::Next { ip, op: copy_end })
}

/// Add LZ4 length-extension bytes from `src[ip..]` to `len`: every byte is
/// added, and a byte other than 255 ends the run. Reading at or past `limit`
/// is an overrun. Returns the total length and the index after the run.
///
/// Requires `limit <= src.len()`.
#[inline(always)]
fn read_length_extension(
    src: &[u8],
    mut ip: usize,
    limit: usize,
    mut len: usize,
) -> Result<(usize, usize), Lz4Error> {
    loop {
        if ip >= limit {
            return Err(Lz4Error::Overrun);
        }
        let s = src[ip] as usize;
        ip += 1;
        // Each extension byte adds up to 255: on 32-bit targets a long 0xff
        // run in a large input would otherwise wrap `usize`.
        len = len.checked_add(s).ok_or(Lz4Error::Overrun)?;
        if s != 255 {
            return Ok((len, ip));
        }
    }
}

/// Copy `len` bytes from `src[si..]` to `dst[di..]`, byte-by-byte.
///
/// Source and destination are disjoint; matches within output use other helpers.
#[inline(always)]
#[allow(clippy::manual_memcpy)]
fn copy_within(src: &[u8], si: usize, dst: &mut [u8], di: usize, len: usize) {
    dst[di..di + len].copy_from_slice(&src[si..si + len]);
}

/// Wild copy: 8-byte chunks from src to dst until `dst_end`.
/// May overshoot by up to 7 bytes (caller must ensure space).
#[inline(always)]
fn wild_copy(src: &[u8], si: usize, dst: &mut [u8], di: usize, dst_end: usize) {
    let mut s = si;
    let mut d = di;
    // Fast path: full 8-byte chunks with bounds checked once.
    while d + 8 <= dst.len() && s + 8 <= src.len() && d < dst_end {
        dst[d..d + 8].copy_from_slice(&src[s..s + 8]);
        s += 8;
        d += 8;
    }
    // Remainder: byte-by-byte for the tail.
    while d < dst_end && d < dst.len() && s < src.len() {
        dst[d] = src[s];
        d += 1;
        s += 1;
    }
}

/// Wild copy within the same buffer (for match copies with offset >= 8).
///
/// When offset >= 8, each 8-byte chunk lies entirely before its destination,
/// so it is copied from the already-written head of a `split_at_mut`. For the
/// last partial chunk we fall back to byte copy.
#[inline(always)]
fn wild_copy_within(buf: &mut [u8], src: usize, dst: usize, dst_end: usize) {
    let mut s = src;
    let mut d = dst;
    // Fast path: copy 8-byte chunks. Since offset >= 8 (caller guarantees),
    // `s + 8 <= d`, so the source chunk is in the head below `d`.
    while d + 8 <= dst_end {
        let (head, tail) = buf.split_at_mut(d);
        tail[..8].copy_from_slice(&head[s..s + 8]);
        s += 8;
        d += 8;
    }
    // Remainder: byte-by-byte for the last partial chunk.
    while d < dst_end {
        buf[d] = buf[s];
        d += 1;
        s += 1;
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

/// Byte-at-a-time reference for one sequence, with the same acceptance rules
/// as [`decode_sequence`]: positions are `u128`, so no guard can wrap, and every
/// copy is a plain indexed byte move. Proofs and tests compare against it.
#[cfg(any(test, kani))]
fn reference_sequence(
    src: &[u8],
    dst: &mut [u8],
    ip: usize,
    op: usize,
) -> Result<Sequence, Lz4Error> {
    let iend = src.len() as u128;
    let oend = dst.len() as u128;
    let mut ip = ip as u128;
    let mut op = op as u128;
    if ip >= iend {
        return Err(Lz4Error::Overrun);
    }
    let token = src[ip as usize];
    ip += 1;
    let mut lit_len = u128::from(token >> ML_BITS);
    if lit_len == u128::from(RUN_MASK) {
        loop {
            if ip >= iend.saturating_sub(u128::from(RUN_MASK)) {
                return Err(Lz4Error::Overrun);
            }
            let s = src[ip as usize];
            ip += 1;
            lit_len += u128::from(s);
            if s != 255 {
                break;
            }
        }
    }
    // The last sequence is literals only; the format keeps the final MFLIMIT
    // output bytes and the last offset+token+LAST_LITERALS input bytes out of
    // reach of any match.
    let last = op + lit_len > oend.saturating_sub(MFLIMIT as u128)
        || ip + lit_len > iend.saturating_sub((2 + 1 + LAST_LITERALS) as u128);
    if last && (op + lit_len > oend || ip + lit_len != iend) {
        return Err(Lz4Error::Overrun);
    }
    // Output byte `i` of the run is input byte `ip + (i - op)`.
    for i in 0..dst.len() {
        let i = i as u128;
        if i >= op && i < op + lit_len {
            dst[i as usize] = src[(ip + (i - op)) as usize];
        }
    }
    op += lit_len;
    ip += lit_len;
    if last {
        return Ok(Sequence::Last { op: op as usize });
    }
    if ip + 2 > iend {
        return Err(Lz4Error::Overrun);
    }
    let offset = u128::from(src[ip as usize]) | (u128::from(src[ip as usize + 1]) << 8);
    ip += 2;
    if offset == 0 || offset > op {
        return Err(Lz4Error::BadOffset);
    }
    let mut match_len = u128::from(token & ML_MASK);
    if match_len == u128::from(ML_MASK) {
        loop {
            if ip > iend.saturating_sub(LAST_LITERALS as u128) {
                return Err(Lz4Error::Overrun);
            }
            let s = src[ip as usize];
            ip += 1;
            match_len += u128::from(s);
            if s != 255 {
                break;
            }
        }
    }
    match_len += MIN_MATCH as u128;
    if op + match_len > oend {
        return Err(Lz4Error::Overrun);
    }
    // Ascending, so an overlapping match re-reads bytes it just produced.
    for i in 0..dst.len() {
        let i = i as u128;
        if i >= op && i < op + match_len {
            dst[i as usize] = dst[(i - offset) as usize];
        }
    }
    op += match_len;
    Ok(Sequence::Next {
        ip: ip as usize,
        op: op as usize,
    })
}

/// Reference whole-block decoder: [`reference_sequence`] from the start.
#[cfg(any(test, kani))]
fn reference_decompress(src: &[u8], dst: &mut [u8]) -> Result<usize, Lz4Error> {
    decode_sequences(src, dst, reference_sequence)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::{vec, vec::Vec};

    fn check(src: &[u8], dst_len: usize) -> Result<usize, Lz4Error> {
        let mut got = vec![0xa5; dst_len];
        let mut want = vec![0x5a; dst_len];
        let result = decompress_block(src, &mut got);
        assert_eq!(result, reference_decompress(src, &mut want));
        if let Ok(n) = result {
            assert_eq!(got[..n], want[..n]);
        }
        result
    }

    #[test]
    fn literal_run_extension_bytes_accumulate() {
        // 15 + 255 + 10 = 280 literals as the only (final) sequence.
        let mut src = vec![0xf0, 255, 10];
        src.extend((0..280).map(|i| i as u8));
        let mut dst = [0; 280];
        assert_eq!(decompress_block(&src, &mut dst), Ok(280));
        assert!(dst.iter().enumerate().all(|(i, &b)| b == i as u8));
        assert_eq!(check(&src, 280), Ok(280));
        // A truncated extension or literal body is an overrun, not a panic.
        assert_eq!(check(&src[..3], 280), Err(Lz4Error::Overrun));
        assert_eq!(check(&src[..src.len() - 1], 280), Err(Lz4Error::Overrun));
        assert_eq!(check(&[0xf0, 255, 255, 255], 4096), Err(Lz4Error::Overrun));
    }

    #[test]
    fn match_length_extension_and_offsets() {
        // 8 literals, offset 8, match 15+4+3=22, final 5 literals.
        let mut src = vec![0x8f];
        src.extend_from_slice(b"ABCDEFGH");
        src.extend_from_slice(&[8, 0, 3, 0x50]);
        src.extend_from_slice(b"vwxyz");
        assert_eq!(check(&src, 64), Ok(8 + 22 + 5));
        let mut dst = [0; 35];
        assert_eq!(decompress_block(&src, &mut dst), Ok(35));
        assert_eq!(&dst[..30], b"ABCDEFGHABCDEFGHABCDEFGHABCDEF");
        // Exact fill and one byte short.
        assert_eq!(check(&src, 35), Ok(35));
        assert_eq!(check(&src, 34), Err(Lz4Error::Overrun));

        // Short offset repeat patterns (offset 1 and 3), match length 13.
        for (offset, expect) in [
            (1u8, b"abcccccccccccccc12345"),
            (3, b"abcabcabcabcabca12345"),
        ] {
            let mut src = vec![0x39, b'a', b'b', b'c', offset, 0, 0x50];
            src.extend_from_slice(b"12345");
            assert_eq!(check(&src, 64), Ok(21));
            let mut dst = [0; 21];
            assert_eq!(decompress_block(&src, &mut dst), Ok(21));
            assert_eq!(&dst, expect);
        }

        // Offset zero or reaching before the output start.
        let mut bad = vec![0x84];
        bad.extend_from_slice(b"ABCDEFGH");
        bad.extend_from_slice(&[0, 0, 0x50]);
        bad.extend_from_slice(b"vwxyz");
        assert_eq!(check(&bad, 64), Err(Lz4Error::BadOffset));
        bad[9] = 9;
        assert_eq!(check(&bad, 64), Err(Lz4Error::BadOffset));
    }

    #[test]
    fn truncated_match_length_extension_is_overrun() {
        let mut src = vec![0x8f];
        src.extend_from_slice(b"ABCDEFGH");
        src.extend_from_slice(&[8, 0]);
        src.extend_from_slice(&[255; 6]);
        assert_eq!(check(&src, 4096), Err(Lz4Error::Overrun));
    }

    #[test]
    fn empty_and_trailing_input_rejected() {
        assert_eq!(check(&[], 16), Err(Lz4Error::Overrun));
        assert_eq!(check(&[0x00], 16), Ok(0));
        assert_eq!(check(&[0x10, b'x', 0], 16), Err(Lz4Error::Overrun));
    }

    #[test]
    fn reference_agrees_on_lz4_flex_output() {
        let mut data = Vec::new();
        for i in 0..2000u32 {
            data.extend_from_slice(&(i % 37).to_le_bytes()[..(i % 4) as usize + 1]);
        }
        let src = lz4_flex::block::compress(&data);
        assert_eq!(check(&src, data.len()), Ok(data.len()));
        let mut dst = vec![0; data.len()];
        decompress_block(&src, &mut dst).unwrap();
        assert_eq!(dst, data);
    }
}

#[cfg(kani)]
mod proofs {
    use super::*;

    // Modular proof: each copy helper is proven against a byte-at-a-time
    // specification under the precondition `decode_sequence` establishes,
    // then the sequence decoder is proven against the reference with the
    // helpers replaced by stubs that assert that precondition and perform
    // the specification. The stubs over-approximate the helpers, so the
    // composition is sound.
    //
    // All of it is bounded: helpers are proven on the exact `(src, dst)` slice
    // lengths used by the decoder harnesses — `(10, BUF)` and `(SRC, 16)` —
    // so every helper call they make lies inside the proven helper domain.

    const BUF: usize = 24;

    /// Ascending match copy: overlapping matches re-read fresh bytes.
    fn match_spec(buf: &mut [u8], op: usize, offset: usize, len: usize) {
        for i in 0..buf.len() {
            if i >= op && i < op + len {
                buf[i] = buf[i - offset];
            }
        }
    }

    /// `[di, di + len)` equals `want`; every other byte in `0..buf_len` is untouched.
    fn assert_copied(old: &[u8], new: &[u8], want: &[u8], di: usize, len: usize) {
        for i in 0..old.len() {
            if i >= di && i < di + len {
                assert_eq!(new[i], want[i]);
            } else {
                assert_eq!(new[i], old[i]);
            }
        }
    }

    fn any_match() -> ([u8; BUF], usize, usize, usize, usize) {
        let buf: [u8; BUF] = kani::any();
        let buf_len: usize = if kani::any() { BUF } else { 16 };
        let op: usize = kani::any_where(|&o| o <= buf_len);
        let offset: usize = kani::any_where(|&f| f >= 1 && f <= op);
        let len: usize = kani::any_where(|&l| l <= buf_len - op);
        (buf, buf_len, op, offset, len)
    }

    #[kani::proof]
    #[kani::unwind(26)]
    fn verify_copy_match_short_meets_spec() {
        let (old, buf_len, op, offset, len) = any_match();
        kani::assume(offset < 8);
        let (mut new, mut want) = (old, old);
        copy_match_short(&mut new[..buf_len], op, op - offset, offset, len);
        match_spec(&mut want[..buf_len], op, offset, len);
        assert_copied(&old[..buf_len], &new[..buf_len], &want[..buf_len], op, len);
        kani::cover!(offset == 1 && len >= 8, "run-length repeat");
    }

    #[kani::proof]
    #[kani::unwind(26)]
    fn verify_copy_match_careful_meets_spec() {
        let (old, buf_len, op, offset, len) = any_match();
        kani::assume(offset >= 8);
        let (mut new, mut want) = (old, old);
        copy_match_careful(&mut new[..buf_len], op, op - offset, len);
        match_spec(&mut want[..buf_len], op, offset, len);
        assert_copied(&old[..buf_len], &new[..buf_len], &want[..buf_len], op, len);
    }

    /// The chunked fast path never splits below its source chunk.
    #[kani::proof]
    #[kani::unwind(26)]
    fn verify_wild_copy_within_is_sound_and_meets_spec() {
        let (old, buf_len, op, offset, len) = any_match();
        kani::assume(offset >= 8);
        let (mut new, mut want) = (old, old);
        wild_copy_within(&mut new[..buf_len], op - offset, op, op + len);
        match_spec(&mut want[..buf_len], op, offset, len);
        assert_copied(&old[..buf_len], &new[..buf_len], &want[..buf_len], op, len);
        kani::cover!(len >= 16 && offset < len, "overlapping chunked match");
    }

    const SRC: usize = 17;

    /// Length extension against a `u128` sum: bytes are added up to and
    /// including the first non-255 byte, every read stays below `limit`, and
    /// a total that does not fit `usize` is an overrun rather than a wrap.
    #[kani::proof]
    #[kani::unwind(6)]
    fn verify_read_length_extension_meets_spec() {
        const N: usize = 4;
        let src: [u8; N] = kani::any();
        let limit: usize = kani::any_where(|&l| l <= N);
        let ip: usize = kani::any_where(|&i| i <= N);
        let len: usize = kani::any();
        let mut want = Err(Lz4Error::Overrun);
        let mut sum = len as u128;
        let mut i = ip;
        let mut terminated = false;
        while i < limit {
            sum += u128::from(src[i]);
            i += 1;
            if src[i - 1] != 255 {
                terminated = true;
                if let Ok(total) = usize::try_from(sum) {
                    want = Ok((total, i));
                }
                break;
            }
        }
        let got = read_length_extension(&src, ip, limit, len);
        assert_eq!(got, want);
        kani::cover!(
            matches!(got, Ok((_, end)) if end >= ip + 2),
            "multi-byte extension"
        );
        kani::cover!(
            got.is_err() && terminated && sum > usize::MAX as u128,
            "overflow rejected"
        );
    }

    /// Literal wild copy may overshoot by at most `WILD_COPY_LEN - 1` bytes.
    #[kani::proof]
    #[kani::unwind(26)]
    fn verify_wild_copy_meets_spec() {
        let src: [u8; SRC] = kani::any();
        let old: [u8; BUF] = kani::any();
        let (src_len, dst_len) = if kani::any() { (10, BUF) } else { (SRC, 16) };
        let di: usize = kani::any_where(|&d| d <= dst_len);
        let len: usize = kani::any_where(|&l| l <= dst_len - di);
        let si: usize = kani::any_where(|&s| s <= src_len && len <= src_len - s);
        let mut new = old;
        wild_copy(&src[..src_len], si, &mut new[..dst_len], di, di + len);
        for i in 0..dst_len {
            if i >= di && i < di + len {
                assert_eq!(new[i], src[si + (i - di)]);
            } else if i < di || i >= di + len + WILD_COPY_LEN - 1 {
                assert_eq!(new[i], old[i]);
            }
        }
        kani::cover!(len >= 9, "chunked literal copy");
    }

    fn stub_wild_copy(src: &[u8], si: usize, dst: &mut [u8], di: usize, dst_end: usize) {
        assert!(di <= dst_end && dst_end <= dst.len());
        assert!(si + (dst_end - di) <= src.len());
        dst[di..dst_end].copy_from_slice(&src[si..si + (dst_end - di)]);
        for k in 0..WILD_COPY_LEN - 1 {
            if dst_end + k < dst.len() {
                dst[dst_end + k] = kani::any();
            }
        }
    }

    fn stub_match_precondition(buf: &[u8], op: usize, match_pos: usize, len: usize) -> usize {
        assert!(match_pos < op && op + len <= buf.len());
        op - match_pos
    }

    fn stub_copy_match_short(
        buf: &mut [u8],
        op: usize,
        match_pos: usize,
        offset: usize,
        len: usize,
    ) {
        assert_eq!(stub_match_precondition(buf, op, match_pos, len), offset);
        assert!(offset < 8);
        match_spec(buf, op, offset, len);
    }

    fn stub_copy_match_careful(buf: &mut [u8], op: usize, match_pos: usize, len: usize) {
        let offset = stub_match_precondition(buf, op, match_pos, len);
        assert!(offset >= 8);
        match_spec(buf, op, offset, len);
    }

    fn stub_wild_copy_within(buf: &mut [u8], src: usize, dst: usize, dst_end: usize) {
        assert!(dst <= dst_end);
        let offset = stub_match_precondition(buf, dst, src, dst_end - dst);
        assert!(offset >= 8);
        match_spec(buf, dst, offset, dst_end - dst);
    }

    /// One sequence from any state with `ip <= S`, `op <= D` equals the
    /// byte-at-a-time reference. Both decoders run their step through the
    /// same [`decode_sequences`] driver from `(0, 0)`, and `Next` preserves
    /// that precondition while strictly advancing, so blocks of at most `S`
    /// input and `D` output bytes agree by induction over their sequences.
    /// Positions are symbolic within fixed-size buffers, so every pairing of
    /// remaining input/output up to `S`/`D` and every match offset up to `op`
    /// is covered. Longer blocks rest on the helper and length-extension
    /// proofs, not on this bounded equivalence.
    fn check_sequence<const S: usize, const D: usize>() {
        let src: [u8; S] = kani::any();
        let ip: usize = kani::any_where(|&i| i <= S);
        let op: usize = kani::any_where(|&o| o <= D);
        let mut dst: [u8; D] = kani::any();
        // Same decoded prefix, independent garbage past `op`: the step may
        // depend only on `dst[..op]`.
        let mut want: [u8; D] = core::array::from_fn(|i| if i < op { dst[i] } else { kani::any() });

        let got = decode_sequence(&src, &mut dst, ip, op);
        assert_eq!(got, reference_sequence(&src, &mut want, ip, op));
        let end = match got {
            Ok(Sequence::Next {
                ip: next_ip,
                op: next_op,
            }) => {
                assert!(next_ip >= ip + 3 && next_ip <= S);
                assert!(next_op >= op + MIN_MATCH && next_op <= D);
                let token = src[ip];
                kani::cover!(token & ML_MASK == ML_MASK, "match length extension");
                if token & ML_MASK != ML_MASK {
                    let lit = usize::from(token >> ML_BITS);
                    let offset = usize::from(src[next_ip - 2]) | usize::from(src[next_ip - 1]) << 8;
                    let len = usize::from(token & ML_MASK) + MIN_MATCH;
                    kani::cover!(offset < 8 && lit > 0, "literals and short-offset match");
                    kani::cover!(
                        offset >= 8 && next_op + MFLIMIT <= D,
                        ">=8-byte-offset wild match"
                    );
                    kani::cover!(
                        offset >= 8 && next_op + MFLIMIT > D,
                        "careful match at the end"
                    );
                }
                next_op
            }
            Ok(Sequence::Last { op: next_op }) => {
                assert!(next_op >= op && next_op <= D);
                kani::cover!(next_op > op, "final literal-only sequence");
                kani::cover!(src[ip] >> ML_BITS == RUN_MASK, "literal length extension");
                next_op
            }
            Err(_) => return,
        };
        for i in 0..D {
            if i < end {
                assert_eq!(dst[i], want[i]);
            }
        }
    }

    /// Sequences of up to 10 input and 24 output bytes: literals, all
    /// three match copy paths and match-length extension. A literal-length
    /// extension needs `ip < iend - RUN_MASK`, so it is unreachable here.
    #[kani::proof]
    #[kani::unwind(26)]
    #[kani::stub(wild_copy, stub_wild_copy)]
    #[kani::stub(wild_copy_within, stub_wild_copy_within)]
    #[kani::stub(copy_match_short, stub_copy_match_short)]
    #[kani::stub(copy_match_careful, stub_copy_match_careful)]
    fn verify_decode_sequence_matches_reference() {
        check_sequence::<10, BUF>();
    }

    /// Sequences of up to 17 input and 16 output bytes reach literal-length
    /// extension (token, extension byte and 15 literals ending the block).
    #[kani::proof]
    #[kani::unwind(18)]
    #[kani::stub(wild_copy, stub_wild_copy)]
    #[kani::stub(wild_copy_within, stub_wild_copy_within)]
    #[kani::stub(copy_match_short, stub_copy_match_short)]
    #[kani::stub(copy_match_careful, stub_copy_match_careful)]
    fn verify_decode_sequence_with_literal_extension_matches_reference() {
        check_sequence::<SRC, 16>();
    }
}
