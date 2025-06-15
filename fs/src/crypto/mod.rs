pub mod fullread;
pub use fullread::*;

pub mod ml_dsa;
pub mod ed25519;

use embedded_io_async::Read;
use signature::{Verifier, Result};

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
