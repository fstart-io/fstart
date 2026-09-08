//! Constant root-verification policy, independent of packed media locations.
//!
//! The image builder patches this block before compressing any containing file.
//! Its expected keys/family/floor are authority only when the containing code is
//! protected or reached through a trusted predecessor. Finding this marker on
//! untrusted media does not establish trust.

use super::{TRUST_MAX_KEYS, VerificationKey};

pub const TRUST_MAGIC: [u8; 8] = *b"FSTRUST1";
pub const TRUST_VERSION: u32 = 1;
pub const TRUST_SIZE: usize = core::mem::size_of::<TrustBlock>();

/// Fixed-size policy bytes; no root, directory, image or microcode location.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct TrustBlock {
    pub magic: [u8; 8],
    pub version: u32,
    pub encoded_size: u32,
    pub minimum_security_version: u64,
    pub key_count: u32,
    pub reserved: u32,
    pub image_family: [u8; 16],
    pub keys: [VerificationKey; TRUST_MAX_KEYS],
}

// All fields have explicit widths, and every byte belongs to a field. These
// offsets also ensure the borrowed key array never requires an unaligned load.
const _: () = {
    assert!(TRUST_SIZE == 320);
    assert!(core::mem::offset_of!(TrustBlock, minimum_security_version) == 16);
    assert!(core::mem::offset_of!(TrustBlock, image_family) == 32);
    assert!(core::mem::offset_of!(TrustBlock, keys) == 48);
};

impl TrustBlock {
    pub const fn placeholder() -> Self {
        Self {
            magic: TRUST_MAGIC,
            version: TRUST_VERSION,
            encoded_size: TRUST_SIZE as u32,
            minimum_security_version: 0,
            key_count: 0,
            reserved: 0,
            image_family: [0; 16],
            keys: [VerificationKey::ZERO; TRUST_MAX_KEYS],
        }
    }

    /// Decode wire bytes by value, without alignment or volatile-memory assumptions.
    /// Parsing proves structure, not protected policy provenance.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != TRUST_SIZE {
            return None;
        }
        let mut block = Self {
            magic: bytes[..8].try_into().ok()?,
            version: u32::from_le_bytes(bytes[8..12].try_into().ok()?),
            encoded_size: u32::from_le_bytes(bytes[12..16].try_into().ok()?),
            minimum_security_version: u64::from_le_bytes(bytes[16..24].try_into().ok()?),
            key_count: u32::from_le_bytes(bytes[24..28].try_into().ok()?),
            reserved: u32::from_le_bytes(bytes[28..32].try_into().ok()?),
            image_family: bytes[32..48].try_into().ok()?,
            keys: [VerificationKey::ZERO; TRUST_MAX_KEYS],
        };
        if block.magic != TRUST_MAGIC
            || block.version != TRUST_VERSION
            || block.encoded_size != TRUST_SIZE as u32
            || block.key_count as usize > TRUST_MAX_KEYS
            || block.reserved != 0
        {
            return None;
        }
        for (key, bytes) in block.keys.iter_mut().zip(bytes[48..].chunks_exact(68)) {
            *key = VerificationKey {
                key_id: bytes[0],
                algorithm: bytes[1],
                _pad: bytes[2..4].try_into().ok()?,
                key_lo: bytes[4..36].try_into().ok()?,
                key_hi: bytes[36..68].try_into().ok()?,
            };
        }
        Some(block)
    }

    pub fn valid_keys(&self) -> &[VerificationKey] {
        &self.keys[..self.key_count as usize]
    }

    /// Host patch representation. Firmware targets and the host packer use
    /// little-endian encoding, not the host's native scalar representation.
    pub fn write_to(&self, dest: &mut [u8]) {
        assert!(dest.len() >= TRUST_SIZE, "short trust destination");
        let dest = &mut dest[..TRUST_SIZE];
        dest[..8].copy_from_slice(&self.magic);
        dest[8..12].copy_from_slice(&self.version.to_le_bytes());
        dest[12..16].copy_from_slice(&self.encoded_size.to_le_bytes());
        dest[16..24].copy_from_slice(&self.minimum_security_version.to_le_bytes());
        dest[24..28].copy_from_slice(&self.key_count.to_le_bytes());
        dest[28..32].copy_from_slice(&self.reserved.to_le_bytes());
        dest[32..48].copy_from_slice(&self.image_family);
        for (key, bytes) in self.keys.iter().zip(dest[48..].chunks_exact_mut(68)) {
            bytes[0] = key.key_id;
            bytes[1] = key.algorithm;
            bytes[2..4].copy_from_slice(&key._pad);
            bytes[4..36].copy_from_slice(&key.key_lo);
            bytes[36..68].copy_from_slice(&key.key_hi);
        }
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
            || u32::from_le(version) != TRUST_VERSION
            || u32::from_le(size) != TRUST_SIZE as u32
            || u32::from_le(count) as usize > TRUST_MAX_KEYS
            || reserved != 0
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
        u64::from_le(unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(self.block.minimum_security_version))
        })
    }

    pub fn image_family(self) -> [u8; 16] {
        // SAFETY: initialized byte array in the live policy block.
        unsafe { core::ptr::read_volatile(core::ptr::addr_of!(self.block.image_family)) }
    }

    pub fn valid_keys(self) -> &'a [VerificationKey] {
        // SAFETY: validated count in immutable storage. Keys contain only bytes.
        let count = u32::from_le(unsafe {
            core::ptr::read_volatile(core::ptr::addr_of!(self.block.key_count))
        });
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
        policy.minimum_security_version = 7;
        policy.image_family = [0xa5; 16];
        policy.key_count = 1;
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
