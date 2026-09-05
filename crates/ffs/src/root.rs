//! Boot root revision 1: explicit LE, 64 + 2*160 + 64 + 64 bytes.
//!
//! Parsing proves wire invariants only. Authentication additionally checks trusted
//! family/version/key policy. Neither grants memory-write or executable authority.
use fstart_core::ffs::{Compression, Signature, SignatureKind, VerificationKey};

pub const ROOT_SIZE: usize = 512;
pub const SIGNED_SIZE: usize = 448;
pub const ROOT_MAGIC: [u8; 8] = *b"FSTARTBR";
pub const DIRECTORY_REVISION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootError {
    InvalidFormat,
    UnsupportedVersion,
    OutOfBounds,
    PolicyRejected,
    SignatureInvalid,
    UnsupportedAlgorithm,
    DigestMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum BootstrapRole {
    Postcar = 1,
    Mainstage = 2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BootstrapDescriptor {
    pub role: BootstrapRole,
    pub compression: Compression,
    pub offset: u64,
    pub stored_size: u64,
    /// Initialized bytes only; BSS belongs to authenticated startup.
    pub loaded_size: u64,
    pub load_addr: u64,
    pub entry_offset: u64,
    pub scratch_size: u64,
    pub stored_digest: [u8; 32],
    pub loaded_digest: [u8; 32],
}

impl BootstrapDescriptor {
    pub fn validate(&self, image_size: u64) -> Result<(), RootError> {
        if self.stored_size == 0 || self.loaded_size == 0 || self.entry_offset >= self.loaded_size {
            return Err(RootError::InvalidFormat);
        }
        range(self.offset, self.stored_size, image_size)?;
        self.load_addr
            .checked_add(self.loaded_size)
            .ok_or(RootError::OutOfBounds)?;
        if self.compression == Compression::Lz4
            && self.scratch_size
                < self
                    .stored_size
                    .checked_add(self.loaded_size)
                    .ok_or(RootError::OutOfBounds)?
        {
            return Err(RootError::InvalidFormat);
        }
        if self.compression == Compression::None
            && (self.scratch_size != 0
                || self.stored_size != self.loaded_size
                || self.stored_digest != self.loaded_digest)
        {
            return Err(RootError::InvalidFormat);
        }
        Ok(())
    }

    pub fn encode(&self) -> [u8; 160] {
        let mut b = [0; 160];
        b[..2].copy_from_slice(&(self.role as u16).to_le_bytes());
        b[2] = match self.compression {
            Compression::None => 0,
            Compression::Lz4 => 1,
        };
        for (i, n) in [
            self.offset,
            self.stored_size,
            self.loaded_size,
            self.load_addr,
            self.entry_offset,
            self.scratch_size,
        ]
        .into_iter()
        .enumerate()
        {
            b[8 + i * 8..16 + i * 8].copy_from_slice(&n.to_le_bytes());
        }
        b[56..88].copy_from_slice(&self.stored_digest);
        b[88..120].copy_from_slice(&self.loaded_digest);
        b
    }

    pub fn parse(b: &[u8]) -> Result<Self, RootError> {
        if b.len() != 160 || b[3..8] != [0; 5] || b[120..] != [0; 40] {
            return Err(RootError::InvalidFormat);
        }
        let d = Self {
            role: match u16::from_le_bytes(b[..2].try_into().unwrap()) {
                1 => BootstrapRole::Postcar,
                2 => BootstrapRole::Mainstage,
                _ => return Err(RootError::InvalidFormat),
            },
            compression: match b[2] {
                0 => Compression::None,
                1 => Compression::Lz4,
                _ => return Err(RootError::UnsupportedAlgorithm),
            },
            offset: le64(b, 8),
            stored_size: le64(b, 16),
            loaded_size: le64(b, 24),
            load_addr: le64(b, 32),
            entry_offset: le64(b, 40),
            scratch_size: le64(b, 48),
            stored_digest: b[56..88].try_into().unwrap(),
            loaded_digest: b[88..120].try_into().unwrap(),
        };
        d.validate(u64::MAX)?;
        Ok(d)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryRef {
    pub offset: u64,
    pub size: u64,
    pub digest: [u8; 32],
}
impl DirectoryRef {
    pub fn validate(&self, image_size: u64, max_size: u64) -> Result<(), RootError> {
        if self.size == 0 || self.size > max_size {
            return Err(RootError::OutOfBounds);
        }
        range(self.offset, self.size, image_size)
    }
    pub fn encode(&self) -> [u8; 64] {
        let mut b = [0; 64];
        b[..8].copy_from_slice(&self.offset.to_le_bytes());
        b[8..16].copy_from_slice(&self.size.to_le_bytes());
        b[16..48].copy_from_slice(&self.digest);
        b[48..52].copy_from_slice(&DIRECTORY_REVISION.to_le_bytes());
        b
    }
    pub fn parse(b: &[u8]) -> Result<Self, RootError> {
        if b.len() != 64 || b[52..] != [0; 12] {
            return Err(RootError::InvalidFormat);
        }
        if le32(b, 48) != DIRECTORY_REVISION {
            return Err(RootError::UnsupportedVersion);
        }
        let r = Self {
            offset: le64(b, 0),
            size: le64(b, 8),
            digest: b[16..48].try_into().unwrap(),
        };
        r.validate(u64::MAX, u64::MAX)?;
        Ok(r)
    }
    /// Verify this exact stable buffer before parsing it. Reference provenance
    /// must come from an authenticated root or protected family handoff.
    pub fn verify_bytes(&self, bytes: &[u8]) -> Result<(), RootError> {
        if u64::try_from(bytes.len()).ok() != Some(self.size) {
            return Err(RootError::OutOfBounds);
        }
        verify_digest(bytes, &self.digest)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Root {
    pub security_version: u64,
    pub image_family: [u8; 16],
    pub key_id: u32,
    pub descriptors: [Option<BootstrapDescriptor>; 2],
    pub directory: DirectoryRef,
    pub signature: [u8; 64],
}
impl Root {
    pub fn encode(&self) -> [u8; ROOT_SIZE] {
        let mut b = [0; ROOT_SIZE];
        b[..8].copy_from_slice(&ROOT_MAGIC);
        b[8..10].copy_from_slice(&1u16.to_le_bytes());
        b[10..12].copy_from_slice(&64u16.to_le_bytes());
        b[12..16].copy_from_slice(&(ROOT_SIZE as u32).to_le_bytes());
        b[16..24].copy_from_slice(&self.security_version.to_le_bytes());
        b[24..40].copy_from_slice(&self.image_family);
        b[40..44].copy_from_slice(&self.key_id.to_le_bytes());
        for (i, d) in self.descriptors.iter().enumerate() {
            if let Some(d) = d {
                b[64 + i * 160..224 + i * 160].copy_from_slice(&d.encode());
            }
        }
        b[384..448].copy_from_slice(&self.directory.encode());
        b[448..].copy_from_slice(&self.signature);
        b
    }
    pub fn parse(b: &[u8]) -> Result<Self, RootError> {
        if b.len() != ROOT_SIZE
            || b[..8] != ROOT_MAGIC
            || b[10..12] != 64u16.to_le_bytes()
            || le32(b, 12) != ROOT_SIZE as u32
            || b[44..64] != [0; 20]
        {
            return Err(RootError::InvalidFormat);
        }
        if b[8..10] != 1u16.to_le_bytes() {
            return Err(RootError::UnsupportedVersion);
        }
        let mut descriptors = [None; 2];
        for (i, d) in descriptors.iter_mut().enumerate() {
            let bytes = &b[64 + i * 160..224 + i * 160];
            if bytes != [0; 160] {
                *d = Some(BootstrapDescriptor::parse(bytes)?);
            }
        }
        if descriptors[0].is_none() && descriptors[1].is_some() {
            return Err(RootError::InvalidFormat);
        }
        Ok(Self {
            security_version: le64(b, 16),
            image_family: b[24..40].try_into().unwrap(),
            key_id: le32(b, 40),
            descriptors,
            directory: DirectoryRef::parse(&b[384..448])?,
            signature: b[448..].try_into().unwrap(),
        })
    }
}

/// All fields must originate in protected platform configuration, not the image.
pub struct RootPolicy<'a> {
    pub image_family: [u8; 16],
    pub minimum_security_version: u64,
    pub image_size: u64,
    pub max_directory_size: u64,
    pub keys: &'a [VerificationKey],
}
#[derive(Debug, Clone, Copy)]
pub struct AuthenticatedRoot(Root);
impl AuthenticatedRoot {
    pub fn root(&self) -> &Root {
        &self.0
    }
    pub fn descriptors(&self) -> &[Option<BootstrapDescriptor>; 2] {
        &self.0.descriptors
    }
    pub fn directory(&self) -> &DirectoryRef {
        &self.0.directory
    }
}

pub fn authenticate_root(
    policy: &RootPolicy<'_>,
    bytes: &[u8],
) -> Result<AuthenticatedRoot, RootError> {
    let root = Root::parse(bytes)?;
    if root.image_family != policy.image_family
        || root.security_version < policy.minimum_security_version
    {
        return Err(RootError::PolicyRejected);
    }
    root.directory
        .validate(policy.image_size, policy.max_directory_size)?;
    for d in root.descriptors.iter().flatten() {
        d.validate(policy.image_size)?;
    }
    // Current protected anchor keys have u8 IDs. Reject rather than truncate.
    let key_id = u8::try_from(root.key_id).map_err(|_| RootError::PolicyRejected)?;
    let mut keys = policy.keys.iter().filter(|k| k.key_id == key_id);
    let key = keys.next().ok_or(RootError::PolicyRejected)?;
    if keys.next().is_some() {
        return Err(RootError::PolicyRejected);
    }
    let sig = Signature {
        key_id,
        kind: SignatureKind::Ed25519,
        sig_lo: root.signature[..32].try_into().unwrap(),
        sig_hi: root.signature[32..].try_into().unwrap(),
    };
    fstart_crypto::verify::verify_signature(&bytes[..SIGNED_SIZE], &sig, key).map_err(
        |e| match e {
            fstart_crypto::verify::VerifyError::UnsupportedAlgorithm
            | fstart_crypto::verify::VerifyError::AlgorithmMismatch => {
                RootError::UnsupportedAlgorithm
            }
            _ => RootError::SignatureInvalid,
        },
    )?;
    Ok(AuthenticatedRoot(root))
}

pub fn verify_digest(bytes: &[u8], expected: &[u8; 32]) -> Result<(), RootError> {
    fstart_crypto::digest::verify_digest_set(
        bytes,
        &fstart_core::ffs::DigestSet {
            sha256: Some(*expected),
            sha3_256: None,
        },
    )
    .map_err(|e| match e {
        fstart_crypto::digest::DigestError::NoAlgorithmAvailable => RootError::UnsupportedAlgorithm,
        _ => RootError::DigestMismatch,
    })
}
fn range(offset: u64, size: u64, limit: u64) -> Result<(), RootError> {
    if offset.checked_add(size).is_some_and(|end| end <= limit) {
        Ok(())
    } else {
        Err(RootError::OutOfBounds)
    }
}
fn le64(b: &[u8], n: usize) -> u64 {
    u64::from_le_bytes(b[n..n + 8].try_into().unwrap())
}
fn le32(b: &[u8], n: usize) -> u32 {
    u32::from_le_bytes(b[n..n + 4].try_into().unwrap())
}
