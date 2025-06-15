pub mod fullread;
pub use fullread::*;

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
