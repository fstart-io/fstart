//! Constant root-verification policy, independent of packed media locations.
//!
//! The image builder patches this block before compressing any containing file.
//! Its expected keys/family/floor are authority only when the containing code is
//! protected or reached through a trusted predecessor. Finding this marker on
//! untrusted media does not establish trust.

use super::{TRUST_MAX_KEYS, VerificationKey};
use zerocopy::byteorder::{LE, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout};

pub const TRUST_MAGIC: [u8; 8] = *b"FSTRUST1";
pub const TRUST_VERSION: u32 = 1;
pub const TRUST_SIZE: usize = core::mem::size_of::<TrustBlock>();

/// Fixed-size policy bytes; no root, directory, image or microcode location.
#[derive(Debug, Clone, Copy, FromBytes, IntoBytes, Immutable, KnownLayout)]
#[repr(C, align(8))]
pub struct TrustBlock {
    pub magic: [u8; 8],
    pub version: U32<LE>,
    pub encoded_size: U32<LE>,
    pub minimum_security_version: U64<LE>,
    pub key_count: U32<LE>,
    pub reserved: U32<LE>,
    pub image_family: [u8; 16],
    pub keys: [VerificationKey; TRUST_MAX_KEYS],
}

// All fields have explicit widths and endian order, and every byte belongs
// to a field. The borrowed key array never requires an unaligned load.
const _: () = {
    assert!(TRUST_SIZE == 320);
    // The packer scans initial-image patch sites at eight-byte boundaries.
    assert!(core::mem::align_of::<TrustBlock>() == 8);
    assert!(core::mem::offset_of!(TrustBlock, minimum_security_version) == 16);
    assert!(core::mem::offset_of!(TrustBlock, image_family) == 32);
    assert!(core::mem::offset_of!(TrustBlock, keys) == 48);
};

impl TrustBlock {
    pub const fn placeholder() -> Self {
        Self {
            magic: TRUST_MAGIC,
            version: U32::new(TRUST_VERSION),
            encoded_size: U32::new(TRUST_SIZE as u32),
            minimum_security_version: U64::new(0),
            key_count: U32::new(0),
            reserved: U32::new(0),
            image_family: [0; 16],
            keys: [VerificationKey::ZERO; TRUST_MAX_KEYS],
        }
    }

    /// Decode wire bytes by value, without alignment or volatile-memory assumptions.
    /// Parsing proves structure, not protected policy provenance.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let block = Self::read_from_bytes(bytes).ok()?;
        (block.magic == TRUST_MAGIC
            && block.version.get() == TRUST_VERSION
            && block.encoded_size.get() == TRUST_SIZE as u32
            && block.key_count.get() as usize <= TRUST_MAX_KEYS
            && block.reserved.get() == 0)
            .then_some(block)
    }

    pub fn valid_keys(&self) -> &[VerificationKey] {
        &self.keys[..self.key_count.get() as usize]
    }

    pub fn set_key_count(&mut self, count: u32) {
        self.key_count.set(count);
    }

    pub fn set_minimum_security_version(&mut self, version: u64) {
        self.minimum_security_version.set(version);
    }

    /// Host patch representation is explicitly little-endian on any host.
    pub fn write_to(&self, dest: &mut [u8]) {
        assert!(dest.len() >= TRUST_SIZE, "short trust destination");
        dest[..TRUST_SIZE].copy_from_slice(self.as_bytes());
    }
}

/// Borrow constant policy without copying four public keys onto an early stack.
#[derive(Clone, Copy)]
pub struct TrustRef<'a> {
    block: &'a TrustBlock,
}

impl<'a> TrustRef<'a> {
    /// Read a build-patched policy block. No caller-supplied location or header
    /// may substitute for the expected policy's provenance.
    ///
    /// # Safety
    /// The bytes must remain initialized and immutable for the returned borrow.
    /// The caller owns their protected-code/trusted-predecessor provenance.
    pub unsafe fn read_volatile(data: &'a [u8]) -> Option<Self> {
        if data.len() != TRUST_SIZE
            || data
                .as_ptr()
                .align_offset(core::mem::align_of::<TrustBlock>())
                != 0
        {
            return None;
        }
        let ptr = data.as_ptr().cast::<TrustBlock>();
        // SAFETY: size/alignment checked above; each scalar is initialized.
        let magic = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*ptr).magic)) };
        let version = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*ptr).version)) };
        let size = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*ptr).encoded_size)) };
        let count = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*ptr).key_count)) };
        let reserved = unsafe { core::ptr::read_volatile(core::ptr::addr_of!((*ptr).reserved)) };
        if magic != TRUST_MAGIC
            || version.get() != TRUST_VERSION
            || size.get() != TRUST_SIZE as u32
            || count.get() as usize > TRUST_MAX_KEYS
            || reserved.get() != 0
        {
            return None;
        }
        // SAFETY: all fields are integer/byte arrays; header was validated.
        Some(Self {
            block: unsafe { &*ptr },
        })
    }

    pub fn minimum_security_version(self) -> u64 {
        // SAFETY: aligned scalar in the live, immutable policy block.
        unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(self.block.minimum_security_version))
        }
        .get()
    }

    pub fn image_family(self) -> [u8; 16] {
        // SAFETY: initialized byte array in the live policy block.
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(self.block.image_family)) }
    }

    pub fn valid_keys(self) -> &'a [VerificationKey] {
        // SAFETY: validated count in immutable storage. Keys contain only bytes.
        let count =
            unsafe { core::ptr::read_volatile(core::ptr::addr_of!(self.block.key_count)) }.get();
        &self.block.keys[..count as usize]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[repr(align(8))]
    struct Bytes([u8; TRUST_SIZE]);

    #[test]
    fn constant_policy_round_trip_and_malformed_headers() {
        let mut policy = TrustBlock::placeholder();
        policy.set_minimum_security_version(7);
        policy.image_family = [0xa5; 16];
        policy.set_key_count(1);
        policy.keys[0] = VerificationKey::ed25519(3, [0x5a; 32]);
        let mut bytes = Bytes([0; TRUST_SIZE]);
        policy.write_to(&mut bytes.0);
        // SAFETY: local aligned bytes stay immutable for each view's lifetime.
        let view = unsafe { TrustRef::read_volatile(&bytes.0) }.unwrap();
        assert_eq!(view.minimum_security_version(), 7);
        assert_eq!(view.image_family(), [0xa5; 16]);
        assert_eq!(view.valid_keys()[0].key_id, 3);
        assert_eq!(view.valid_keys()[0].key_lo, [0x5a; 32]);
        for (offset, value) in [(8, 2), (12, 0), (24, 5), (28, 1)] {
            let mut bad = Bytes(bytes.0);
            bad.0[offset] = value;
            assert!(unsafe { TrustRef::read_volatile(&bad.0) }.is_none());
        }
        assert!(unsafe { TrustRef::read_volatile(&bytes.0[..TRUST_SIZE - 1]) }.is_none());
    }
}
