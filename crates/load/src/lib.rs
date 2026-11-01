//! The loading chain: read → verify → decompress → place.
//!
//! Every load of firmware or payload bytes goes through these steps, each a
//! type the next one needs:
//!
//! - [`read`] copies bytes from a [`BootMedia`] into memory the caller owns,
//!   giving [`Unverified`] bytes;
//! - [`Unverified::verify`] checks them against a digest, giving
//!   [`Verified`] bytes: nothing parses or decodes bytes before this;
//! - [`Verified::decompress`] runs a [`Codec`] the caller chose into an
//!   output buffer, giving new [`Unverified`] bytes to check against the
//!   loaded digest;
//! - [`place`] checks a destination against a [`MemoryPolicy`] before the
//!   first write.
//!
//! The codec is an argument, not a build feature: a stage that only loads
//! stored data passes [`Stored`] and never links a decoder; one that loads
//! compressed images passes [`Lz4`]. [`load_bootstrap`] composes the chain
//! for a [`BootstrapDescriptor`].
//!
//! Descriptors imported from a handoff are trusted only because their producer
//! was verified before entry and the handoff RAM remains protected. These APIs
//! do not authenticate RAM, enforce flash write protection, or prevent rollback.

#![no_std]

use fstart_core::ffs::Compression;
use fstart_core::services::BootMedia;
use fstart_ffs::root::BootstrapDescriptor;

#[cfg(test)]
mod tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

// ---------------------------------------------------------------------------
// The chain
// ---------------------------------------------------------------------------

/// Bytes read or decoded but not checked yet.
#[must_use]
pub struct Unverified<'a>(&'a mut [u8]);

/// Bytes that matched their digest. Only these are decoded or handed on.
#[must_use]
pub struct Verified<'a>(&'a [u8]);

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
            (Some(end), Some(other_end)) => self.start < other_end && other.start < end,
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
    if let Some(source) = media.as_slice() {
        let source = MemoryWindow {
            start: source.as_ptr() as u64,
            size: source.len() as u64,
        };
        if source.overlaps(MemoryWindow { start, size }) {
            return Err(LoadError::MemoryPolicy);
        }
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
    match descriptor.compression {
        Compression::None => {
            if descriptor.stored_size != descriptor.loaded_size
                || descriptor.stored_digest != descriptor.loaded_digest
            {
                return Err(LoadError::Bounds);
            }
            Ok(descriptor.loaded_size)
        }
        _ => descriptor
            .loaded_size
            .checked_add(descriptor.stored_size)
            .ok_or(LoadError::Bounds),
    }
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
/// represents its load address: read, verify the stored digest, decompress
/// with `codec`, verify the loaded digest. Useful also for block media and
/// host tests.
pub fn load_bootstrap_into(
    media: &(impl BootMedia + ?Sized),
    descriptor: &BootstrapDescriptor,
    workspace: &mut [u8],
    codec: &dyn Codec,
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
    let output = match descriptor.compression {
        Compression::None => read(media, descriptor.offset, output)?,
        compression => {
            // Authenticate precisely the stable bytes passed to the decoder.
            read(media, descriptor.offset, scratch)?
                .verify::<Sha256>(&descriptor.stored_digest, LoadError::StoredDigest)?
                .decompress(codec, compression, output)?
        }
    };
    let error = if descriptor.compression == Compression::None {
        LoadError::StoredDigest
    } else {
        LoadError::LoadedDigest
    };
    output
        .verify::<Sha256>(&descriptor.loaded_digest, error)
        .map(drop)
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
    load_bootstrap_into(media, descriptor, workspace, codec)?;
    Ok(VerifiedExecutable { entry })
}
