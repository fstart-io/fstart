pub mod fullread;
pub use fullread::*;

pub mod double;

pub mod ml_dsa;
pub mod ed25519;

use embedded_io_async::Read;
use digest::{Update, FixedOutputReset, Output, OutputSizeUser};
use signature::{Verifier, Result, Error};

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
