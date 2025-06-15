pub mod fullread;
pub use fullread::*;

pub mod double;

pub mod ml_dsa;
pub mod ed25519;

use embedded_io_async::Read;
use digest::{Update, FixedOutputReset, Output, OutputSizeUser};
use signature::{Verifier, Result, Error};
use sha2::Sha512;
use sha3::Sha3_256;
use fdt::node::FdtNode;
use crate::metadata;

pub trait FindDigest: OutputSizeUser {
    fn find_into(out: &mut Output<Self>, fdt: &FdtNode) -> crate::Result<()>;

    fn find(fdt: &FdtNode) -> Option<Output<Self>> {
        let mut out = Output::<Self>::default();
        Self::find_into(&mut out, fdt).ok().map(|_| out)
    }
}

impl<D: OutputSizeUser + AssociatedAlgo> FindDigest for D {
    fn find_into(out: &mut Output<Self>, fdt: &FdtNode) -> crate::Result<()> {
        fdt.property(Self::algo().name()).ok_or(crate::Error::NotFound)
            .and_then(|prop| (prop.value.len() == Self::output_size())
                                .then(|| out.clone_from_slice(prop.value))
                                .ok_or(crate::Error::InvalidData))
    }
}

pub trait AssociatedAlgo {
    fn algo() -> metadata::HashAlgo;
}
impl AssociatedAlgo for Sha512 {
    fn algo() -> metadata::HashAlgo { metadata::HashAlgo::Sha512 }
}
impl AssociatedAlgo for Sha3_256 {
    fn algo() -> metadata::HashAlgo { metadata::HashAlgo::Sha3_256 }
}

pub trait ParseSignature<S> {
    #[allow(async_fn_in_trait)]
    async fn try_parse_signature<R: Read>(read: &mut R) -> Option<S>;
}

pub struct SignatureVerify<'a, V, S> {
    verifier: &'a V,
    signature: &'a S,
}

impl<'a, V, S> SignatureVerify<'a, V, S>
where
    V: Verifier<S>,
{
    pub fn new(verifier: &'a V, signature: &'a S) -> Self {
        Self { verifier, signature }
    }

    #[must_use]
    pub fn verify(&self, msg: &[u8]) -> Result<()> {
        self.verifier.verify(msg, self.signature)
    }
}

pub struct HashVerify<'a, D: OutputSizeUser> {
    digest: D,
    expected: &'a Output<D>,
}

impl<'a, D> HashVerify<'a, D>
where
    D: Update + FixedOutputReset,
{
    pub fn new(digest: D, expected: &'a Output<D>) -> Self {
        Self { digest, expected }
    }

    #[must_use]
    pub fn verify(&mut self) -> Result<()> {
        (self.digest.finalize_fixed_reset() == *self.expected)
            .then_some(()).ok_or(Error::new())
    }

    fn update(&mut self, input: &[u8]) {
        self.digest.update(input)
    }
}
