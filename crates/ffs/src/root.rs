//! Boot root revision 1: explicit LE, 64 + 2*160 + 64 + 64 bytes.
//!
//! Parsing proves wire invariants only. Authentication additionally checks trusted
//! family/version/key policy. Neither grants memory-write or executable authority.
use fstart_core::ffs::{Compression, Signature, SignatureKind, VerificationKey};
use zerocopy::byteorder::{LE, U16, U32, U64};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

pub const ROOT_SIZE: usize = 512;
pub const SIGNED_SIZE: usize = 448;
pub const ROOT_MAGIC: [u8; 8] = *b"FSTARTBR";
pub const DIRECTORY_REVISION: u32 = 2;

#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct BootstrapWire {
    role: U16<LE>,
    compression: u8,
    reserved_3: [u8; 5],
    offset: U64<LE>,
    stored_size: U64<LE>,
    loaded_size: U64<LE>,
    load_addr: U64<LE>,
    entry_offset: U64<LE>,
    scratch_size: U64<LE>,
    stored_digest: [u8; 32],
    loaded_digest: [u8; 32],
    reserved_120: [u8; 40],
}
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct DirectoryWire {
    offset: U64<LE>,
    size: U64<LE>,
    digest: [u8; 32],
    revision: U32<LE>,
    reserved: [u8; 12],
}
#[derive(Clone, Copy, FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned)]
#[repr(C)]
struct RootWire {
    magic: [u8; 8],
    version: U16<LE>,
    header_len: U16<LE>,
    root_size: U32<LE>,
    security_version: U64<LE>,
    image_family: [u8; 16],
    key_id: U32<LE>,
    fw_version: U32<LE>,
    reserved: [u8; 16],
}
const _: () = assert!(core::mem::size_of::<BootstrapWire>() == 160);
const _: () = assert!(core::mem::size_of::<DirectoryWire>() == 64);
const _: () = assert!(core::mem::size_of::<RootWire>() == 64);

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
        if self.compression != Compression::None
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
        let wire = BootstrapWire {
            role: U16::new(self.role as u16),
            compression: match self.compression {
                Compression::None => 0,
                Compression::Lz4 => 1,
                Compression::Lzma => 2,
            },
            reserved_3: [0; 5],
            offset: U64::new(self.offset),
            stored_size: U64::new(self.stored_size),
            loaded_size: U64::new(self.loaded_size),
            load_addr: U64::new(self.load_addr),
            entry_offset: U64::new(self.entry_offset),
            scratch_size: U64::new(self.scratch_size),
            stored_digest: self.stored_digest,
            loaded_digest: self.loaded_digest,
            reserved_120: [0; 40],
        };
        let mut b = [0; 160];
        b.copy_from_slice(wire.as_bytes());
        b
    }

    pub fn parse(b: &[u8]) -> Result<Self, RootError> {
        let wire = BootstrapWire::read_from_bytes(b).map_err(|_| RootError::InvalidFormat)?;
        if wire.reserved_3 != [0; 5] || wire.reserved_120 != [0; 40] {
            return Err(RootError::InvalidFormat);
        }
        let d = Self {
            role: match wire.role.get() {
                1 => BootstrapRole::Postcar,
                2 => BootstrapRole::Mainstage,
                _ => return Err(RootError::InvalidFormat),
            },
            compression: match wire.compression {
                0 => Compression::None,
                1 => Compression::Lz4,
                2 => Compression::Lzma,
                _ => return Err(RootError::UnsupportedAlgorithm),
            },
            offset: wire.offset.get(),
            stored_size: wire.stored_size.get(),
            loaded_size: wire.loaded_size.get(),
            load_addr: wire.load_addr.get(),
            entry_offset: wire.entry_offset.get(),
            scratch_size: wire.scratch_size.get(),
            stored_digest: wire.stored_digest,
            loaded_digest: wire.loaded_digest,
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
        let wire = DirectoryWire {
            offset: U64::new(self.offset),
            size: U64::new(self.size),
            digest: self.digest,
            revision: U32::new(DIRECTORY_REVISION),
            reserved: [0; 12],
        };
        let mut b = [0; 64];
        b.copy_from_slice(wire.as_bytes());
        b
    }
    pub fn parse(b: &[u8]) -> Result<Self, RootError> {
        let wire = DirectoryWire::read_from_bytes(b).map_err(|_| RootError::InvalidFormat)?;
        if wire.reserved != [0; 12] {
            return Err(RootError::InvalidFormat);
        }
        if wire.revision.get() != DIRECTORY_REVISION {
            return Err(RootError::UnsupportedVersion);
        }
        let r = Self {
            offset: wire.offset.get(),
            size: wire.size.get(),
            digest: wire.digest,
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
    pub fw_version: u32,
    pub image_family: [u8; 16],
    pub key_id: u32,
    pub descriptors: [Option<BootstrapDescriptor>; 2],
    pub directory: DirectoryRef,
    pub signature: [u8; 64],
}
impl Root {
    pub fn encode(&self) -> [u8; ROOT_SIZE] {
        let mut b = [0; ROOT_SIZE];
        let header = RootWire {
            magic: ROOT_MAGIC,
            version: U16::new(1),
            header_len: U16::new(64),
            root_size: U32::new(ROOT_SIZE as u32),
            security_version: U64::new(self.security_version),
            image_family: self.image_family,
            key_id: U32::new(self.key_id),
            fw_version: U32::new(self.fw_version),
            reserved: [0; 16],
        };
        b[..64].copy_from_slice(header.as_bytes());
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
        if b.len() != ROOT_SIZE {
            return Err(RootError::InvalidFormat);
        }
        let (wire, _) = RootWire::ref_from_prefix(b).map_err(|_| RootError::InvalidFormat)?;
        if wire.magic != ROOT_MAGIC
            || wire.header_len.get() != 64
            || wire.root_size.get() != ROOT_SIZE as u32
            || wire.reserved != [0; 16]
        {
            return Err(RootError::InvalidFormat);
        }
        if wire.version.get() != 1 {
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
            security_version: wire.security_version.get(),
            fw_version: wire.fw_version.get(),
            image_family: wire.image_family,
            key_id: wire.key_id.get(),
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
    // Plain slice iteration keeps the bounded loop visible to Kani.
    for d in &root.descriptors {
        let Some(d) = d else { continue };
        d.validate(policy.image_size)?;
    }
    // Current protected anchor keys have u8 IDs. Reject rather than truncate.
    let key_id = u8::try_from(root.key_id).map_err(|_| RootError::PolicyRejected)?;
    // Exactly one protected key may carry the ID; duplicates are ambiguous.
    let mut key = None;
    for k in policy.keys {
        if k.key_id == key_id {
            if key.is_some() {
                return Err(RootError::PolicyRejected);
            }
            key = Some(k);
        }
    }
    let key = key.ok_or(RootError::PolicyRejected)?;
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

#[cfg(kani)]
mod proofs {
    use super::*;
    use fstart_core::ffs::TRUST_MAX_KEYS;
    use fstart_crypto::verify::VerifyError;

    fn in_bounds(offset: u64, size: u64, limit: u64) -> bool {
        u128::from(offset) + u128::from(size) <= u128::from(limit)
    }

    #[kani::proof]
    fn verify_bootstrap_descriptor_parse_encode_round_trip() {
        let bytes: [u8; 160] = kani::any();
        if let Ok(d) = BootstrapDescriptor::parse(&bytes) {
            kani::cover!(d.compression != Compression::None, "compressed descriptor");
            kani::cover!(d.compression == Compression::None, "stored descriptor");
            assert!(d.validate(u64::MAX).is_ok());
            assert!(d.stored_size != 0 && d.entry_offset < d.loaded_size);
            assert!(in_bounds(d.offset, d.stored_size, u64::MAX));
            assert!(in_bounds(d.load_addr, d.loaded_size, u64::MAX));
            // Reserved bytes are zero, so encoding reproduces the wire exactly.
            assert!(d.encode() == bytes);
        }
        // Any other length is rejected without panicking.
        let len: usize = kani::any_where(|&l| l < bytes.len());
        assert!(BootstrapDescriptor::parse(&bytes[..len]).is_err());
    }

    #[kani::proof]
    fn verify_bootstrap_descriptor_validate_encode_parse_round_trip() {
        let bytes: [u8; 160] = kani::any();
        let Ok(d) = BootstrapDescriptor::parse(&bytes) else {
            return;
        };
        // Re-encode any accepted descriptor after symbolic field updates.
        let mut e = d;
        e.offset = kani::any();
        e.stored_size = kani::any();
        e.loaded_size = kani::any();
        e.load_addr = kani::any();
        e.entry_offset = kani::any();
        e.scratch_size = kani::any();
        e.stored_digest = kani::any();
        e.loaded_digest = kani::any();
        let image_size: u64 = kani::any();
        if e.validate(image_size).is_ok() {
            kani::cover!(true, "updated descriptor accepted");
            kani::cover!(
                e.compression == Compression::None,
                "stored descriptor accepted"
            );
            assert!(in_bounds(e.offset, e.stored_size, image_size));
            if e.compression != Compression::None {
                assert!(
                    u128::from(e.scratch_size)
                        >= u128::from(e.stored_size) + u128::from(e.loaded_size)
                );
            } else {
                assert!(e.scratch_size == 0 && e.stored_size == e.loaded_size);
                assert!(e.stored_digest == e.loaded_digest);
            }
            assert_eq!(BootstrapDescriptor::parse(&e.encode()), Ok(e));
        } else {
            kani::cover!(
                e.compression == Compression::None
                    && e.stored_size != 0
                    && e.stored_size == e.loaded_size
                    && e.entry_offset < e.loaded_size
                    && in_bounds(e.offset, e.stored_size, image_size)
                    && in_bounds(e.load_addr, e.loaded_size, u64::MAX)
                    && e.scratch_size == 0
                    && e.stored_digest != e.loaded_digest,
                "stored descriptor rejected only for mismatched digests"
            );
        }
    }

    #[kani::proof]
    fn verify_directory_ref_parse_validate_encode_round_trip() {
        let bytes: [u8; 64] = kani::any();
        if let Ok(r) = DirectoryRef::parse(&bytes) {
            kani::cover!(true, "directory reference accepted");
            assert!(r.size != 0 && in_bounds(r.offset, r.size, u64::MAX));
            assert!(r.encode() == bytes);
        }
        let r = DirectoryRef {
            offset: kani::any(),
            size: kani::any(),
            digest: kani::any(),
        };
        let image_size: u64 = kani::any();
        let max_size: u64 = kani::any();
        let valid = r.size != 0 && r.size <= max_size && in_bounds(r.offset, r.size, image_size);
        assert_eq!(r.validate(image_size, max_size).is_ok(), valid);
        if valid {
            assert_eq!(DirectoryRef::parse(&r.encode()), Ok(r));
        }
        let len: usize = kani::any_where(|&l| l < bytes.len());
        assert!(DirectoryRef::parse(&bytes[..len]).is_err());
    }

    #[kani::proof]
    fn verify_root_parse_enforces_layout_and_round_trips() {
        let bytes: [u8; ROOT_SIZE] = kani::any();
        let Ok(root) = Root::parse(&bytes) else {
            return;
        };
        kani::cover!(root.descriptors[1].is_some(), "root with two descriptors");
        kani::cover!(root.descriptors[0].is_none(), "root without descriptors");
        assert!(bytes[..8] == ROOT_MAGIC);
        assert!(bytes[44..64] == [0; 20]);
        // Slots fill in order; an empty slot is all-zero on the wire.
        assert!(root.descriptors[0].is_some() || root.descriptors[1].is_none());
        for (i, d) in root.descriptors.iter().enumerate() {
            let slot = &bytes[64 + i * 160..224 + i * 160];
            match d {
                Some(d) => assert!(d.validate(u64::MAX).is_ok()),
                None => assert!(slot == [0; 160]),
            }
        }
        assert!(root.directory.validate(u64::MAX, u64::MAX).is_ok());
        assert!(root.encode() == bytes);
    }

    #[kani::proof]
    fn verify_root_parse_rejects_wrong_length() {
        let bytes: [u8; ROOT_SIZE + 1] = kani::any();
        let len: usize = kani::any_where(|&l| l <= ROOT_SIZE + 1 && l != ROOT_SIZE);
        assert_eq!(Root::parse(&bytes[..len]), Err(RootError::InvalidFormat));
    }

    /// What `authenticate_root` passed to signature verification, and what
    /// the (stubbed) cryptography answered.
    #[derive(Clone, Copy)]
    struct VerifyCall {
        message: *const u8,
        message_len: usize,
        key: *const VerificationKey,
        key_id: u8,
        ed25519: bool,
        sig_lo: [u8; 32],
        sig_hi: [u8; 32],
        result: Result<(), VerifyError>,
    }

    static mut CALLS: usize = 0;
    static mut CALL: Option<VerifyCall> = None;

    /// Signature checking is cryptography, not policy: any outcome is
    /// possible. The call and its outcome are recorded for the harness.
    fn verify_signature_stub(
        message: &[u8],
        signature: &Signature,
        key: &VerificationKey,
    ) -> Result<(), VerifyError> {
        let result = match kani::any::<u8>() {
            0 => Ok(()),
            1 => Err(VerifyError::InvalidSignature),
            2 => Err(VerifyError::AlgorithmMismatch),
            3 => Err(VerifyError::UnsupportedAlgorithm),
            4 => Err(VerifyError::InvalidKey),
            _ => Err(VerifyError::KeyNotFound),
        };
        // SAFETY: Kani harnesses are single-threaded.
        unsafe {
            CALLS += 1;
            CALL = Some(VerifyCall {
                message: message.as_ptr(),
                message_len: message.len(),
                key,
                key_id: signature.key_id,
                ed25519: signature.kind == SignatureKind::Ed25519,
                sig_lo: signature.sig_lo,
                sig_hi: signature.sig_hi,
                result,
            });
        }
        result
    }

    fn any_key() -> VerificationKey {
        VerificationKey {
            key_id: kani::any(),
            algorithm: kani::any(),
            _pad: kani::any(),
            key_lo: kani::any(),
            key_hi: kani::any(),
        }
    }

    /// `authenticate_root` succeeds exactly when the root parses, satisfies
    /// family, version, directory and descriptor policy, names exactly one
    /// protected key, and that key's signature check over the exact signed
    /// bytes succeeds. Policy failures never reach the cryptography.
    ///
    /// The key table has symbolic length, so its loop needs an explicit
    /// bound; `ci/kani.sh` raises memcmp's for the 160-byte slot compares.
    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::stub(fstart_crypto::verify::verify_signature, verify_signature_stub)]
    fn verify_authenticate_root_enforces_policy() {
        let bytes: [u8; ROOT_SIZE] = kani::any();
        let keys: [VerificationKey; TRUST_MAX_KEYS] = core::array::from_fn(|_| any_key());
        let key_count: usize = kani::any_where(|&n| n <= TRUST_MAX_KEYS);
        let policy = RootPolicy {
            image_family: kani::any(),
            minimum_security_version: kani::any(),
            image_size: kani::any(),
            max_directory_size: kani::any(),
            keys: &keys[..key_count],
        };
        let parsed = Root::parse(&bytes);
        let result = authenticate_root(&policy, &bytes);
        // SAFETY: Kani harnesses are single-threaded.
        let (calls, call) = unsafe { (CALLS, CALL) };
        assert!(calls <= 1);

        // Independent policy model over the (separately proven) parser.
        let mut selected = None;
        let mut policy_ok = false;
        if let Ok(root) = parsed {
            let mut matching = 0;
            for (i, k) in keys.iter().enumerate() {
                if i < key_count && u32::from(k.key_id) == root.key_id {
                    matching += 1;
                    selected = Some(i);
                }
            }
            policy_ok = root.image_family == policy.image_family
                && root.security_version >= policy.minimum_security_version
                && root.directory.size != 0
                && root.directory.size <= policy.max_directory_size
                && in_bounds(
                    root.directory.offset,
                    root.directory.size,
                    policy.image_size,
                )
                && root
                    .descriptors
                    .iter()
                    .flatten()
                    .all(|d| d.validate(policy.image_size).is_ok())
                && matching == 1;
        }

        let Some(call) = call else {
            assert!(!policy_ok);
            assert!(result.is_err());
            kani::cover!(result.is_err(), "policy rejection before cryptography");
            return;
        };
        assert!(policy_ok);
        let i = selected.unwrap();
        assert!(core::ptr::eq(call.key, &keys[i]));
        assert!(core::ptr::eq(call.message, bytes.as_ptr()));
        assert_eq!(call.message_len, SIGNED_SIZE);
        assert_eq!(call.key_id, keys[i].key_id);
        assert!(call.ed25519);
        assert!(call.sig_lo == bytes[448..480]);
        assert!(call.sig_hi == bytes[480..512]);
        match call.result {
            Ok(()) => {
                let auth = result.unwrap();
                kani::cover!(
                    auth.root().descriptors[0].is_some() && key_count == TRUST_MAX_KEYS,
                    "authenticated root with descriptor and full key table"
                );
                assert!(Ok(*auth.root()) == parsed);
                let dir = auth.directory();
                assert!(dir.size != 0 && dir.size <= policy.max_directory_size);
                assert!(in_bounds(dir.offset, dir.size, policy.image_size));
                for d in auth.descriptors().iter().flatten() {
                    assert!(in_bounds(d.offset, d.stored_size, policy.image_size));
                }
            }
            Err(VerifyError::UnsupportedAlgorithm | VerifyError::AlgorithmMismatch) => {
                assert_eq!(result.unwrap_err(), RootError::UnsupportedAlgorithm);
            }
            Err(_) => {
                kani::cover!(true, "signature rejected");
                assert_eq!(result.unwrap_err(), RootError::SignatureInvalid);
            }
        }
    }
}
