use embedded_io_async::{Read, ErrorType};
use signature::Verifier;
use crate::{Error, Result};
use super::SignatureVerify;

pub struct VerifiedFullRead<'a, R, V>
{
    dest:   &'a mut [u8],
    source: R,
    verify: V,
    result: Option<Result<()>>,
}

impl<'a, R, V, S> VerifiedFullRead<'a, R, SignatureVerify<'a, V, S>>
where
    R: Read + ErrorType<Error = Error>,
    V: Verifier<S>
{
    pub const fn new(dest: &'a mut [u8], src: R,
                     verify: SignatureVerify<'a, V, S>) -> Self
    {
        Self {
            dest:   dest,
            source: src,
            verify: verify,
            result: None,
        }
    }

    fn clear_invalid(&mut self) -> Error {
        self.dest.fill(0);
        Error::InvalidData
    }

    pub async fn read_and_verify(mut self) -> Result<()> {
        if self.result.is_none() {
            self.result = Some(
                self.source.read_exact(&mut self.dest).await
                .map_err(crate::rex_to_error)
                .and_then(|_| self.verify.verify(&self.dest)
                    .map_err(|_| self.clear_invalid())));
        }
        self.result.unwrap()
    }
}
