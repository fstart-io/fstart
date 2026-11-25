//! The loading chain: read → verify → measure → decompress → place.
//!
//! Every load of firmware or payload bytes goes through these steps, each a
//! type the next one needs:
//!
//! - [`read`] copies bytes from a [`BootMedia`] into memory the caller owns,
//!   giving [`Unverified`] bytes;
//! - [`Unverified::verify`] checks them against a digest, giving
//!   [`Verified`] bytes: nothing parses or decodes bytes before this;
//! - [`Verified::measure`] records them ([`Measure`]), giving [`Measured`]
//!   bytes. Every load measures, or passes [`Measurement::NONE`];
//! - [`Measured::decompress`] runs a [`Codec`] the caller chose into an
//!   output buffer, giving new [`Unverified`] bytes to check against the
//!   loaded digest;
//! - [`place`] checks a destination against a [`MemoryPolicy`] before the
//!   first write.
//!
//! The bytes verified, measured and used are the same: the chain reads into
//! memory it then borrows until it hands the bytes on, so nothing in safe
//! code can change them between the check and the use. Bytes that stay in
//! the boot media, such as the static root of trust's own image, are
//! [`InPlace`]: measuring them records what the media held at that moment.
//!
//! The codec is an argument: a stage that only loads stored data passes
//! [`Stored`] and never links a decoder. Platforms select matching
//! [`Compression`] values for the host plan and the stage's [`codec`].
//! [`load_bootstrap`] composes the chain for a [`BootstrapDescriptor`].
//!
//! Descriptors imported from a handoff are trusted only because their producer
//! was verified before entry and the handoff RAM remains protected. These APIs
//! do not authenticate RAM, enforce flash write protection, or prevent rollback.

#![no_std]

use fstart_core::ffs::Compression;
use fstart_core::services::BootMedia;
pub use fstart_core::services::measure::{Event, Measure, Measurement, Unmeasured};
use fstart_ffs::root::BootstrapDescriptor;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ufmt::derive::uDebug)]
pub enum LoadError {
    Bounds,
    MemoryPolicy,
    /// The stage's codec does not handle the image's compression.
    Unsupported,
    Io,
    StoredDigest,
    LoadedDigest,
    Decompression,
}

// ---------------------------------------------------------------------------
// Digests and codecs
// ---------------------------------------------------------------------------

/// A digest the chain verifies bytes against.
pub trait Digest {
    type Output: PartialEq;
    fn digest(bytes: &[u8]) -> Self::Output;
}

/// SHA-256: the digest of the FFS stored and loaded images.
pub struct Sha256;

impl Digest for Sha256 {
    type Output = [u8; 32];
    fn digest(bytes: &[u8]) -> [u8; 32] {
        fstart_crypto::digest::hash_sha256(bytes)
    }
}

/// Decompression of verified bytes. A stage passes the codec it links; an
/// image compressed in a way the codec does not handle fails to load
/// ([`LoadError::Unsupported`]).
pub trait Codec: Sync {
    /// Whether this codec decodes `compression`. Checked before any read.
    fn supports(&self, compression: Compression) -> bool;

    /// Decode `input`, compressed as `compression`, into exactly `output`.
    fn decompress(
        &self,
        compression: Compression,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), LoadError>;
}

/// No decoder: stored (uncompressed) images only.
pub struct Stored;

impl Codec for Stored {
    fn supports(&self, _: Compression) -> bool {
        false
    }

    fn decompress(&self, _: Compression, _: &[u8], _: &mut [u8]) -> Result<(), LoadError> {
        Err(LoadError::Unsupported)
    }
}

/// LZ4 block decompression.
pub struct Lz4;

impl Codec for Lz4 {
    fn supports(&self, compression: Compression) -> bool {
        compression == Compression::Lz4
    }

    fn decompress(
        &self,
        compression: Compression,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), LoadError> {
        if compression != Compression::Lz4 {
            return Err(LoadError::Unsupported);
        }
        let count = fstart_ffs::lz4::decompress_block(input, output)
            .map_err(|_| LoadError::Decompression)?;
        if count != output.len() {
            return Err(LoadError::Decompression);
        }
        Ok(())
    }
}

/// LZMA-Alone decompression, with a fixed memory limit for the dictionary
/// and probability tables. The calling stage must provision a heap.
#[cfg(feature = "lzma")]
pub struct Lzma;

#[cfg(feature = "lzma")]
impl Codec for Lzma {
    fn supports(&self, compression: Compression) -> bool {
        compression == Compression::Lzma
    }

    fn decompress(
        &self,
        compression: Compression,
        input: &[u8],
        output: &mut [u8],
    ) -> Result<(), LoadError> {
        use lzma_rust2::{LzmaReader, Read};

        if !self.supports(compression) {
            return Err(LoadError::Unsupported);
        }
        // The LZMA-Alone header must agree with the authenticated output size.
        if input.get(5..13) != Some(&(output.len() as u64).to_le_bytes()) {
            return Err(LoadError::Decompression);
        }
        let mut reader =
            LzmaReader::new_mem_limit(input, 1088, None).map_err(|_| LoadError::Decompression)?;
        reader.set_relaxed_end_condition(false);
        reader
            .read_exact(output)
            .map_err(|_| LoadError::Decompression)
    }
}

/// The codec for images stored as `compression`. Evaluate it in a const
/// (`const { codec(..) }`): the stage then names, and links, only that
/// decoder.
#[must_use]
pub const fn codec(compression: Compression) -> &'static dyn Codec {
    match compression {
        Compression::None => &Stored,
        Compression::Lz4 => &Lz4,
        #[cfg(feature = "lzma")]
        Compression::Lzma => &Lzma,
        #[cfg(not(feature = "lzma"))]
        Compression::Lzma => panic!("LZMA decoder not enabled"),
    }
}

// ---------------------------------------------------------------------------
// The chain
// ---------------------------------------------------------------------------

/// Bytes read or decoded but not checked yet.
#[must_use]
pub struct Unverified<'a>(&'a mut [u8]);

/// Bytes that matched their digest. Only these are measured.
#[must_use]
pub struct Verified<'a>(&'a [u8]);

/// Verified bytes that were measured. Only these are decoded or handed on.
#[must_use]
pub struct Measured<'a>(&'a [u8]);

/// Read exactly `into.len()` bytes at `offset`; short successful device
/// reads are still failures.
pub fn read<'a>(
    media: &(impl BootMedia + ?Sized),
    offset: u64,
    into: &'a mut [u8],
) -> Result<Unverified<'a>, LoadError> {
    let offset = usize::try_from(offset).map_err(|_| LoadError::Bounds)?;
    if offset
        .checked_add(into.len())
        .filter(|&end| end <= media.size())
        .is_none()
    {
        return Err(LoadError::Bounds);
    }
    match media.read_at(offset, into) {
        Ok(size) if size == into.len() => Ok(Unverified(into)),
        _ => Err(LoadError::Io),
    }
}

impl<'a> Unverified<'a> {
    /// Bytes the caller already holds (a decoder's output).
    pub fn new(bytes: &'a mut [u8]) -> Self {
        Self(bytes)
    }

    /// Check the bytes against `expected`; `error` names the failure.
    pub fn verify<D: Digest>(
        self,
        expected: &D::Output,
        error: LoadError,
    ) -> Result<Verified<'a>, LoadError> {
        if D::digest(self.0) == *expected {
            Ok(Verified(self.0))
        } else {
            Err(error)
        }
    }
}

impl<'a> Verified<'a> {
    /// Record these bytes as `measurement`.
    pub fn measure(self, measurement: &Measurement<'_>) -> Measured<'a> {
        measurement.record(self.0);
        Measured(self.0)
    }
}

impl<'a> Measured<'a> {
    pub fn bytes(&self) -> &'a [u8] {
        self.0
    }

    /// Decode into `output` with `codec`: the result is checked again.
    pub fn decompress<'o>(
        self,
        codec: &dyn Codec,
        compression: Compression,
        output: &'o mut [u8],
    ) -> Result<Unverified<'o>, LoadError> {
        codec.decompress(compression, self.0, output)?;
        Ok(Unverified(output))
    }
}

/// Bytes measured where the boot media maps them, not copied. Nothing
/// keeps the media from changing after the measurement: use it only for
/// what has no stable copy to measure, such as the static root of trust's
/// own image, and never hand the bytes on.
pub struct InPlace<'a>(&'a [u8]);

impl<'a> InPlace<'a> {
    /// `len` bytes at `offset` of memory-mapped `media`.
    pub fn new(
        media: &'a (impl BootMedia + ?Sized),
        offset: u64,
        len: u64,
    ) -> Result<Self, LoadError> {
        let start = usize::try_from(offset).map_err(|_| LoadError::Bounds)?;
        let len = usize::try_from(len).map_err(|_| LoadError::Bounds)?;
        let end = start.checked_add(len).ok_or(LoadError::Bounds)?;
        if end > media.size() {
            return Err(LoadError::Bounds);
        }
        media.map(start, len).map(Self).ok_or(LoadError::Io)
    }

    /// Bytes the boot media maps, already found.
    pub fn mapped(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }

    /// The flash address of the bytes, for the event's base.
    pub fn address(&self) -> u64 {
        self.0.as_ptr() as u64
    }

    /// Check the bytes against `expected`.
    pub fn check<D: Digest>(self, expected: &D::Output) -> Result<Self, LoadError> {
        if D::digest(self.0) == *expected {
            Ok(self)
        } else {
            Err(LoadError::StoredDigest)
        }
    }

    /// Record the bytes the media holds now as `measurement`.
    pub fn measure(self, measurement: &Measurement<'_>) {
        measurement.record(self.0);
    }
}

// ---------------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------------

/// Half-open physical memory window, provided by trusted platform code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryWindow {
    pub start: u64,
    pub size: u64,
}

impl MemoryWindow {
    pub fn end(self) -> Option<u64> {
        self.start.checked_add(self.size)
    }

    pub fn contains(self, other: Self) -> bool {
        matches!((self.end(), other.end()), (Some(end), Some(other_end))
            if other.start >= self.start && other_end <= end)
    }

    pub fn overlaps(self, other: Self) -> bool {
        match (self.end(), other.end()) {
            (Some(end), Some(other_end)) => {
                self.size != 0 && other.size != 0 && self.start < other_end && other.start < end
            }
            _ => true,
        }
    }
}

/// Writable destinations must be in an allowed window and outside *every*
/// reservation (running code, stacks, heap, handoff, tables, DMA and media).
#[derive(Clone, Copy)]
pub struct MemoryPolicy<'a> {
    pub writable: &'a [MemoryWindow],
    pub reserved: &'a [MemoryWindow],
    pub entry_alignment: u64,
}

impl MemoryPolicy<'_> {
    pub fn permits(&self, start: u64, size: u64) -> bool {
        let range = MemoryWindow { start, size };
        size != 0
            && start != 0
            && usize::try_from(start).is_ok()
            && size <= isize::MAX as u64
            && range
                .end()
                .and_then(|end| usize::try_from(end).ok())
                .is_some()
            && self.writable.iter().any(|window| window.contains(range))
            && !self.reserved.iter().any(|window| window.overlaps(range))
    }
}

/// Check `start..start + size` against `policy` and against `media`'s own
/// mapping before the first write.
pub fn place(
    media: &(impl BootMedia + ?Sized),
    policy: &MemoryPolicy<'_>,
    start: u64,
    size: u64,
) -> Result<(), LoadError> {
    if !policy.permits(start, size) {
        return Err(LoadError::MemoryPolicy);
    }
    if media.overlaps_range(start, size) {
        return Err(LoadError::MemoryPolicy);
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bootstrap descriptors
// ---------------------------------------------------------------------------

/// Only created after exact initialized destination bytes have been verified.
#[derive(Debug)]
pub struct VerifiedExecutable {
    entry: u64,
}

impl VerifiedExecutable {
    pub fn entry(&self) -> u64 {
        self.entry
    }
}

/// Independently computed live footprint, not an allocation justified by a
/// descriptor's advertised scratch size. Compressed input follows the output
/// and never overlaps it.
pub fn bootstrap_footprint(descriptor: &BootstrapDescriptor) -> Result<u64, LoadError> {
    descriptor
        .validate(u64::MAX)
        .map_err(|_| LoadError::Bounds)?;
    let footprint = match descriptor.compression {
        Compression::None => {
            if descriptor.stored_size != descriptor.loaded_size
                || descriptor.stored_digest != descriptor.loaded_digest
            {
                return Err(LoadError::Bounds);
            }
            descriptor.loaded_size
        }
        _ => descriptor
            .loaded_size
            .checked_add(descriptor.stored_size)
            .ok_or(LoadError::Bounds)?,
    };
    // Descriptor validation bounds only the output; the compressed input
    // window behind it must not wrap the address space either.
    descriptor
        .load_addr
        .checked_add(footprint)
        .ok_or(LoadError::Bounds)?;
    Ok(footprint)
}

/// Where [`load_bootstrap`] leaves the descriptor's verified stored bytes:
/// the compressed input follows the output, an uncompressed image is its
/// own input. They stay intact until the executable runs.
pub fn stored_input(descriptor: &BootstrapDescriptor) -> Result<MemoryWindow, LoadError> {
    let start = match descriptor.compression {
        Compression::None => descriptor.load_addr,
        _ => descriptor
            .load_addr
            .checked_add(descriptor.loaded_size)
            .ok_or(LoadError::Bounds)?,
    };
    Ok(MemoryWindow {
        start,
        size: descriptor.stored_size,
    })
}

/// Load a trusted descriptor into a caller-owned, exclusive workspace that
/// represents its load address: read, verify the stored digest, measure the
/// stored bytes as `measurement`, decompress with `codec`, verify the loaded
/// digest. Useful also for block media and host tests.
pub fn load_bootstrap_into(
    media: &(impl BootMedia + ?Sized),
    descriptor: &BootstrapDescriptor,
    workspace: &mut [u8],
    codec: &dyn Codec,
    measurement: &Measurement<'_>,
) -> Result<(), LoadError> {
    let required =
        usize::try_from(bootstrap_footprint(descriptor)?).map_err(|_| LoadError::Bounds)?;
    if workspace.len() < required
        || descriptor
            .offset
            .checked_add(descriptor.stored_size)
            .filter(|&end| end <= media.size() as u64)
            .is_none()
    {
        return Err(LoadError::Bounds);
    }
    if descriptor.compression != Compression::None && !codec.supports(descriptor.compression) {
        return Err(LoadError::Unsupported);
    }
    let loaded = usize::try_from(descriptor.loaded_size).map_err(|_| LoadError::Bounds)?;
    let (output, scratch) = workspace[..required].split_at_mut(loaded);
    match descriptor.compression {
        // The footprint check made the stored and loaded digests one.
        Compression::None => drop(
            read(media, descriptor.offset, output)?
                .verify::<Sha256>(&descriptor.stored_digest, LoadError::StoredDigest)?
                .measure(measurement),
        ),
        // Authenticate and measure precisely the stable bytes passed to the
        // decoder.
        compression => drop(
            read(media, descriptor.offset, scratch)?
                .verify::<Sha256>(&descriptor.stored_digest, LoadError::StoredDigest)?
                .measure(measurement)
                .decompress(codec, compression, output)?
                .verify::<Sha256>(&descriptor.loaded_digest, LoadError::LoadedDigest)?,
        ),
    }
    Ok(())
}

/// Validate, place and load before granting the executable entry address.
///
/// # Safety
/// The caller supplies authenticated metadata (root or protected handoff) and a
/// trusted memory policy. All permitted physical windows must map exclusively
/// writable RAM for the full operation; reservations must exclude live code,
/// stack, heap, page tables, handoff and device/DMA-owned memory. No writable
/// mapping may alias media, policy, descriptor, or other borrowed Rust objects.
/// DMA and other CPUs must not modify input scratch or output before entry.
pub unsafe fn load_bootstrap(
    media: &(impl BootMedia + ?Sized),
    descriptor: &BootstrapDescriptor,
    policy: &MemoryPolicy<'_>,
    codec: &dyn Codec,
    measurement: &Measurement<'_>,
) -> Result<VerifiedExecutable, LoadError> {
    let footprint = bootstrap_footprint(descriptor)?;
    let entry = descriptor
        .load_addr
        .checked_add(descriptor.entry_offset)
        .ok_or(LoadError::Bounds)?;
    if descriptor.entry_offset >= descriptor.loaded_size
        || policy.entry_alignment == 0
        || !policy.entry_alignment.is_power_of_two()
        || entry % policy.entry_alignment != 0
    {
        return Err(LoadError::MemoryPolicy);
    }
    // Even a readable mmap view does not imply immutability. Reject aliasing
    // before making the mutable destination slice, then buffer compressed input.
    place(media, policy, descriptor.load_addr, footprint)?;
    // SAFETY: the caller's mapping contract and the complete footprint check.
    let workspace = unsafe {
        core::slice::from_raw_parts_mut(descriptor.load_addr as *mut u8, footprint as usize)
    };
    load_bootstrap_into(media, descriptor, workspace, codec, measurement)?;
    Ok(VerifiedExecutable { entry })
}

#[cfg(kani)]
mod proofs {
    use super::*;

    /// Exact `[start, start + size)` end, never wrapping.
    fn end_u128(w: MemoryWindow) -> u128 {
        u128::from(w.start) + u128::from(w.size)
    }

    fn fits(w: MemoryWindow) -> bool {
        end_u128(w) <= u128::from(u64::MAX)
    }

    fn any_window() -> MemoryWindow {
        MemoryWindow {
            start: kani::any(),
            size: kani::any(),
        }
    }

    #[kani::proof]
    fn verify_memory_window_matches_u128_spec() {
        let a = any_window();
        let b = any_window();

        match a.end() {
            Some(end) => assert_eq!(u128::from(end), end_u128(a)),
            None => assert!(!fits(a)),
        }

        let contains = fits(a)
            && fits(b)
            && u128::from(b.start) >= u128::from(a.start)
            && end_u128(b) <= end_u128(a);
        assert_eq!(a.contains(b), contains);

        // A window that does not fit in u64 is conservatively overlapping;
        // representable empty half-open windows share no bytes.
        let overlaps = !fits(a)
            || !fits(b)
            || (a.size != 0
                && b.size != 0
                && u128::from(a.start) < end_u128(b)
                && u128::from(b.start) < end_u128(a));
        assert_eq!(a.overlaps(b), overlaps);
        assert_eq!(a.overlaps(b), b.overlaps(a));
    }

    #[kani::proof]
    fn verify_memory_policy_permits_only_writable_unreserved_ranges() {
        let writable = [any_window(), any_window()];
        let reserved = [any_window(), any_window()];
        let policy = MemoryPolicy {
            writable: &writable,
            reserved: &reserved,
            entry_alignment: kani::any(),
        };
        let start: u64 = kani::any();
        let size: u64 = kani::any();

        if !policy.permits(start, size) {
            return;
        }
        kani::cover!(true, "range accepted beside two reservations");
        let range = MemoryWindow { start, size };
        let end = end_u128(range);
        assert!(size != 0 && start != 0);
        assert!(end <= u128::from(u64::MAX));
        assert!(end <= usize::MAX as u128);
        assert!(u128::from(size) <= isize::MAX as u128);
        assert!(
            policy.writable.iter().any(|w| fits(*w)
                && u128::from(w.start) <= u128::from(start)
                && end <= end_u128(*w))
        );
        for r in policy.reserved {
            // No byte of the accepted range lies in any reservation.
            assert!(fits(*r));
            assert!(r.size == 0 || end <= u128::from(r.start) || u128::from(start) >= end_u128(*r));
        }
    }

    fn any_descriptor() -> BootstrapDescriptor {
        BootstrapDescriptor {
            role: if kani::any() {
                fstart_ffs::root::BootstrapRole::Postcar
            } else {
                fstart_ffs::root::BootstrapRole::Mainstage
            },
            compression: match kani::any::<u8>() % 3 {
                0 => Compression::None,
                1 => Compression::Lz4,
                _ => Compression::Lzma,
            },
            offset: kani::any(),
            stored_size: kani::any(),
            loaded_size: kani::any(),
            load_addr: kani::any(),
            entry_offset: kani::any(),
            scratch_size: kani::any(),
            stored_digest: kani::any(),
            loaded_digest: kani::any(),
        }
    }

    #[kani::proof]
    fn verify_bootstrap_footprint_contains_disjoint_output_and_input() {
        let d = any_descriptor();
        let Ok(footprint) = bootstrap_footprint(&d) else {
            return;
        };
        let input = stored_input(&d).expect("validated descriptor has an input window");

        let base = u128::from(d.load_addr);
        let limit = base + u128::from(footprint);
        let out_end = base + u128::from(d.loaded_size);
        let in_start = u128::from(input.start);
        let in_end = in_start + u128::from(input.size);

        // The whole footprint is addressable without wrap.
        assert!(limit <= u128::from(u64::MAX));
        assert!(d.loaded_size != 0 && input.size == d.stored_size && input.size != 0);
        assert!(out_end <= limit && in_start >= base && in_end <= limit);
        if d.compression == Compression::None {
            kani::cover!(true, "uncompressed footprint");
            assert_eq!(footprint, d.loaded_size);
            assert!(in_start == base && in_end == out_end);
        } else {
            kani::cover!(true, "compressed footprint");
            assert_eq!(
                u128::from(footprint),
                u128::from(d.loaded_size) + u128::from(d.stored_size)
            );
            // Scratch starts where output ends: disjoint half-open intervals.
            assert!(in_start >= out_end);
        }
    }
}
